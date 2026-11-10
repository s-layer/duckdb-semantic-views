//! Differential proptest over random acyclic relationship graphs in random
//! TABLES orders (Phase 7, PR 3).
//!
//! CREATE used to refuse any view where a table is referenced by two different
//! tables unless that table was declared first, so the other harnesses fix
//! their topologies (a star, a chain, one diamond, one bridge). Snowflake
//! creates every acyclic graph in any order (fixture 3, 2026-11-10) and decides
//! per query. This harness draws the graph: three to five tables `t0..`, each
//! `t{j}` referencing at least one earlier table and maybe more, so diamonds,
//! two children of one parent, bridges and two bridges between the same tables
//! all occur; a quarter of the graphs are seeded with two bridges between
//! `t0` and `t1`, which random edges reach too rarely. Every table has a dimension `g{k}` and a metric `s{k}` =
//! `SUM(t{k}.v)`; the data has NULL and dangling foreign keys.
//!
//! The oracle restates Snowflake's rules from the graph, counting the chains
//! of foreign keys (directed paths) between tables:
//!
//! - **A metric on `t{m}`.** Every member (a queried dimension, the filtered
//!   dimension) whose table is reached by exactly one chain is answered, and the
//!   answer is `t{m}` LEFT JOINed along each member's chain, grouped. A member
//!   reached by two chains is refused as multi-path (010246). A member reached
//!   by no chain is refused when there is more than one path to it at all,
//!   over relationships in either direction (000002: two bridges, fixture 3,
//!   Q2, Q3, Q7; customers and stores through orders and through regions,
//!   fixture 5, X1, P3, P4); with one such path (a child, a bridge's far side)
//!   its number is left to the fan-trap and bridge harnesses, and only the
//!   last property is checked.
//! - **No metric.** Snowflake lists the rows of the one table every member is
//!   reached from, the one nearest them, and joins up. With one such table and
//!   one chain to each member the answer is the distinct member values over its
//!   rows; with two chains to a member, two nearest tables (two bridges), or
//!   none (unrelated tables), the query is refused.
//! - **Any query** gives the same rows, or is refused, in two TABLES orders.
//!
//! Rows are compared as sorted multisets of text, every column cast to
//! VARCHAR inside DuckDB.

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, MetricName, QueryRequest};
use semantic_views::model::{
    Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef,
};

/// One table's rows: `(g, v, fks)`, `fks` aligned with [`Graph::out_edges`].
type Rows = Vec<(Option<i64>, Option<i64>, Vec<Option<i64>>)>;

#[derive(Debug, Clone)]
struct Case {
    n: usize,
    /// `(from, to)` with `from > to`: `t{from}.f{to} REFERENCES t{to}(id)`.
    edges: Vec<(usize, usize)>,
    rows: Vec<Rows>,
    /// Two TABLES orders, each a permutation of `0..n`.
    orders: [Vec<usize>; 2],
    /// The metric's table, or `None` for a query with no metrics.
    metric: Option<usize>,
    dims: Vec<usize>,
    /// `g{k} = c` as the query's filter.
    filter: Option<(usize, i64)>,
}

impl Case {
    fn out_edges(&self, from: usize) -> Vec<usize> {
        self.edges
            .iter()
            .filter(|(f, _)| *f == from)
            .map(|(_, t)| *t)
            .collect()
    }

    /// The chains of foreign keys from `from` to `to`, each `[from, …, to]`.
    fn chains(&self, from: usize, to: usize) -> Vec<Vec<usize>> {
        if from == to {
            return vec![vec![from]];
        }
        let mut out = Vec::new();
        for next in self.out_edges(from) {
            for mut rest in self.chains(next, to) {
                rest.insert(0, from);
                out.push(rest);
            }
        }
        out
    }

    fn members(&self) -> Vec<usize> {
        let mut m: Vec<usize> = self.dims.clone();
        m.extend(self.filter.map(|(k, _)| k));
        m.sort_unstable();
        m.dedup();
        m
    }
}

fn arb_case() -> impl Strategy<Value = Case> {
    (3usize..=5, prop::bool::weighted(0.25))
        .prop_flat_map(|(n, seeded)| {
            // Each later table references one earlier table for sure (so the
            // graph is connected) and each other earlier table with chance 2/5.
            let firsts: Vec<BoxedStrategy<usize>> = (1..n).map(|j| (0..j).boxed()).collect();
            let extras = prop::collection::vec(prop::bool::weighted(0.4), n * n);
            (Just(n), Just(seeded && n >= 4), firsts, extras)
        })
        .prop_map(|(n, seeded, firsts, extras)| {
            let mut edges: Vec<(usize, usize)> = Vec::new();
            if seeded {
                // Two bridges between `t0` and `t1` (`t2` and `t3` each
                // reference both, and neither the other), which the random
                // edges below reach too rarely; any later table is random.
                edges.extend([(2, 0), (2, 1), (3, 0), (3, 1)]);
            }
            for j in 1..n {
                if seeded && j < 4 {
                    continue;
                }
                for i in 0..j {
                    if firsts[j - 1] == i || extras[j * n + i] {
                        edges.push((j, i));
                    }
                }
            }
            (n, seeded, edges)
        })
        .prop_flat_map(|(n, seeded, edges)| {
            let out_degree: Vec<usize> = (0..n)
                .map(|k| edges.iter().filter(|(f, _)| *f == k).count())
                .collect();
            let cell = || prop_oneof![1 => Just(None), 4 => (0i64..3).prop_map(Some)];
            let value = || prop_oneof![1 => Just(None), 5 => (1i64..10).prop_map(Some)];
            // A foreign key may be NULL or dangle (4 is past every table's rows).
            let fk = || prop_oneof![1 => Just(None), 6 => (0i64..5).prop_map(Some)];
            let rows: Vec<BoxedStrategy<Rows>> = out_degree
                .iter()
                .map(|&d| {
                    prop::collection::vec((cell(), value(), prop::collection::vec(fk(), d)), 1..5)
                        .boxed()
                })
                .collect();
            let order = || Just((0..n).collect::<Vec<usize>>()).prop_shuffle();
            let metric = prop_oneof![1 => Just(None), 3 => (0..n).prop_map(Some)];
            // A seeded graph queries the two bridged tables in half its cases.
            let dims = prop_oneof![
                3 => prop::sample::subsequence((0..n).collect::<Vec<_>>(), 0..=2),
                u32::from(seeded) * 3 => Just(vec![0, 1]),
            ];
            let filter = prop_oneof![2 => Just(None), 1 => (0..n, 0i64..3).prop_map(Some)];
            (
                Just(n),
                Just(edges),
                rows,
                order(),
                order(),
                metric,
                dims,
                filter,
            )
        })
        .prop_filter(
            "a query with no metric names a dimension",
            |(_, _, _, _, _, metric, dims, _)| metric.is_some() || !dims.is_empty(),
        )
        .prop_map(|(n, edges, rows, o1, o2, metric, dims, filter)| Case {
            n,
            edges,
            rows,
            orders: [o1, o2],
            metric,
            dims,
            filter,
        })
}

fn build_def(case: &Case, order: &[usize]) -> SemanticViewDefinition {
    let t = |k: usize| format!("t{k}");
    SemanticViewDefinition {
        tables: order
            .iter()
            .map(|&k| TableRef {
                alias: t(k),
                table: t(k),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            })
            .collect(),
        joins: case
            .edges
            .iter()
            .map(|&(from, to)| Join {
                table: t(to),
                from_alias: t(from),
                fk_columns: vec![format!("f{to}")],
                ref_columns: vec!["id".to_string()],
                name: Some(format!("r{from}_{to}")),
                cardinality: Cardinality::ManyToOne,
                ..Default::default()
            })
            .collect(),
        dimensions: (0..case.n)
            .map(|k| Dimension {
                name: format!("g{k}"),
                expr: format!("t{k}.g"),
                source_table: Some(t(k)),
                ..Default::default()
            })
            .collect(),
        metrics: (0..case.n)
            .map(|k| Metric {
                name: format!("s{k}"),
                expr: format!("SUM(t{k}.v)"),
                source_table: Some(t(k)),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn request(case: &Case) -> QueryRequest {
    QueryRequest {
        where_clause: case.filter.map(|(k, c)| format!("g{k} = {c}")),
        dimensions: case
            .dims
            .iter()
            .map(|k| DimensionName::new(format!("g{k}")))
            .collect(),
        metrics: case
            .metric
            .iter()
            .map(|k| MetricName::new(format!("s{k}")))
            .collect(),
        // PIN: a FACTS query reads one table's rows like a query with no metrics; this harness oracles the graph's paths, which the dims-only property already walks.
        facts: vec![],
    }
}

fn make_db(case: &Case) -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    let cell = |c: &Option<i64>| c.map_or_else(|| "NULL".to_string(), |v| v.to_string());
    for k in 0..case.n {
        let outs = case.out_edges(k);
        let fk_cols: String = outs.iter().map(|to| format!(", f{to} INTEGER")).collect();
        conn.execute_batch(&format!(
            "CREATE TABLE t{k} (id INTEGER, g INTEGER, v INTEGER{fk_cols});"
        ))
        .expect("create table");
        let values: Vec<String> = case.rows[k]
            .iter()
            .enumerate()
            .map(|(i, (g, v, fks))| {
                let fks: String = fks.iter().map(|f| format!(", {}", cell(f))).collect();
                format!("({i}, {}, {}{fks})", cell(g), cell(v))
            })
            .collect();
        conn.execute_batch(&format!("INSERT INTO t{k} VALUES {};", values.join(", ")))
            .expect("insert rows");
    }
    conn
}

/// `FROM t{from}` LEFT JOINed along one chain per member, each table of a
/// chain under its own alias, and the alias each member's columns are read at.
fn joined_from(from: usize, chains: &[(usize, Vec<usize>)]) -> (String, Vec<(usize, String)>) {
    let mut sql = format!("t{from} AS x");
    let mut at: Vec<(usize, String)> = vec![(from, "x".to_string())];
    for (ci, (member, chain)) in chains.iter().enumerate() {
        let mut prev = "x".to_string();
        for (step, pair) in chain.windows(2).enumerate() {
            let alias = format!("c{ci}_{step}");
            sql.push_str(&format!(
                " LEFT JOIN t{to} AS {alias} ON {prev}.f{to} = {alias}.id",
                to = pair[1]
            ));
            prev = alias;
        }
        at.push((*member, prev));
    }
    (sql, at)
}

/// The oracle's SQL for a query whose every member is reached by one chain
/// from `from`, the metric's table or the table a query with no metrics lists.
fn oracle_sql(case: &Case, from: usize) -> String {
    let chains: Vec<(usize, Vec<usize>)> = case
        .members()
        .into_iter()
        .map(|m| (m, case.chains(from, m).remove(0)))
        .collect();
    let (from_sql, at) = joined_from(from, &chains);
    let alias = |k: usize| {
        at.iter()
            .rev()
            .find(|(m, _)| *m == k)
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| "x".to_string())
    };
    let mut items: Vec<String> = case
        .dims
        .iter()
        .map(|&k| format!("{}.g AS g{k}", alias(k)))
        .collect();
    let filter = case
        .filter
        .map(|(k, c)| format!(" WHERE {}.g = {c}", alias(k)))
        .unwrap_or_default();
    match case.metric {
        Some(m) => {
            items.push(format!("SUM(x.v) AS s{m}"));
            let group = if case.dims.is_empty() {
                String::new()
            } else {
                format!(
                    " GROUP BY {}",
                    (1..=case.dims.len())
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!("SELECT {} FROM {from_sql}{filter}{group}", items.join(", "))
        }
        None => format!(
            "SELECT DISTINCT {} FROM {from_sql}{filter}",
            items.join(", ")
        ),
    }
}

/// What the oracle expects of a case.
#[derive(Debug, PartialEq, Eq)]
enum Expect {
    /// Answered, read from this table.
    Rows(usize),
    /// Refused, with a message holding one of these texts (the first member
    /// the expansion checks decides which).
    Refused(Vec<&'static str>),
    /// Not restated here (a member reached by no chain from the metric's
    /// table): only the two TABLES orders are compared.
    Either,
}

/// The paths from `from` to `to` over relationships in either direction, each
/// table at most once, counted by brute force (a graph has at most five
/// tables).
fn undirected_paths(case: &Case, from: usize, to: usize) -> usize {
    fn walk(case: &Case, at: usize, to: usize, seen: &mut Vec<usize>) -> usize {
        if at == to {
            return 1;
        }
        let mut count = 0;
        for next in 0..case.n {
            let linked = case.edges.contains(&(at, next)) || case.edges.contains(&(next, at));
            if linked && !seen.contains(&next) {
                seen.push(next);
                count += walk(case, next, to, seen);
                seen.pop();
            }
        }
        count
    }
    walk(case, from, to, &mut vec![from])
}

fn expect(case: &Case) -> Expect {
    let members = case.members();
    if let Some(m) = case.metric {
        let mut refusals: Vec<&'static str> = Vec::new();
        for &d in &members {
            match case.chains(m, d).len() {
                0 if undirected_paths(case, m, d) > 1 => refusals.push("many-to-many"),
                0 | 1 => {}
                _ => refusals.push("more than one path"),
            }
        }
        if !refusals.is_empty() {
            return Expect::Refused(refusals);
        }
        if members.iter().all(|&d| case.chains(m, d).len() == 1) {
            return Expect::Rows(m);
        }
        return Expect::Either;
    }
    let candidates: Vec<usize> = (0..case.n)
        .filter(|&c| members.iter().all(|&d| !case.chains(c, d).is_empty()))
        .collect();
    let nearest: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|&c| {
            !candidates
                .iter()
                .any(|&o| o != c && !case.chains(c, o).is_empty())
        })
        .collect();
    match nearest.as_slice() {
        [] => Expect::Refused(vec!["not related"]),
        [anchor] if members.iter().all(|&d| case.chains(*anchor, d).len() == 1) => {
            Expect::Rows(*anchor)
        }
        [_] => Expect::Refused(vec!["more than one path"]),
        _ => Expect::Refused(vec!["many-to-many"]),
    }
}

/// The rows of `sql` as sorted text (every column cast to VARCHAR, so an
/// integer's width does not matter), so answers compare directly.
fn rows(conn: &duckdb::Connection, sql: &str) -> Result<Vec<String>, String> {
    let text = format!("SELECT COLUMNS(*)::VARCHAR FROM ({sql}) AS q");
    let mut stmt = conn.prepare(&text).map_err(|e| e.to_string())?;
    let mut out: Vec<String> = stmt
        .query_map([], |r| {
            let n = r.as_ref().column_count();
            Ok((0..n)
                .map(|i| {
                    let v: Option<String> = r.get(i).unwrap_or(None);
                    v.unwrap_or_else(|| "NULL".to_string())
                })
                .collect::<Vec<_>>()
                .join("|"))
        })
        .map_err(|e| e.to_string())?
        .map(|r| r.map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    out.sort();
    Ok(out)
}

fn check_case(case: &Case) -> Result<(), TestCaseError> {
    let req = request(case);
    let conn = make_db(case);
    let mut answers: Vec<Result<Vec<String>, String>> = Vec::new();
    for order in &case.orders {
        let def = build_def(case, order);
        if let Err(e) = semantic_views::graph::validate_graph(&def) {
            return Err(TestCaseError::fail(format!(
                "CREATE must accept every acyclic graph in any order: {}",
                e.message
            )));
        }
        let answer = match expand("dag", &def, &req) {
            Ok(sql) => Ok(rows(&conn, &sql)
                .map_err(|e| TestCaseError::fail(format!("emitted SQL fails: {e}\n{sql}")))?),
            Err(e) => Err(e.to_string()),
        };
        match (expect(case), &answer) {
            (Expect::Rows(from), Ok(got)) => {
                let oracle = oracle_sql(case, from);
                let want = rows(&conn, &oracle)
                    .map_err(|e| TestCaseError::fail(format!("oracle fails: {e}\n{oracle}")))?;
                prop_assert_eq!(got, &want, "order {:?}\n--- oracle:\n{}", order, oracle);
            }
            (Expect::Rows(_), Err(e)) => {
                return Err(TestCaseError::fail(format!(
                    "order {order:?}: expected an answer, got: {e}"
                )));
            }
            (Expect::Refused(texts), Ok(got)) => {
                return Err(TestCaseError::fail(format!(
                    "order {order:?}: expected a refusal ({texts:?}), got rows {got:?}"
                )));
            }
            (Expect::Refused(texts), Err(e)) => {
                prop_assert!(
                    texts.iter().any(|t| e.contains(t)),
                    "order {:?}: expected one of {:?} in: {}",
                    order,
                    texts,
                    e
                );
            }
            (Expect::Either, _) => {}
        }
        answers.push(answer);
    }
    prop_assert_eq!(
        &answers[0],
        &answers[1],
        "the two TABLES orders {:?} and {:?} disagree",
        case.orders[0],
        case.orders[1]
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn random_graphs_answer_by_their_paths_in_any_tables_order(case in arb_case()) {
        check_case(&case)?;
    }
}

/// The generator reaches every outcome the oracle restates, with a shared
/// table (one referenced by two tables) in the graph, so no branch is vacuous.
#[test]
fn generator_reaches_every_outcome() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for _ in 0..1000 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        let shared = (0..case.n).any(|k| case.edges.iter().filter(|(_, t)| *t == k).count() > 1);
        if !shared {
            *seen.entry("no shared table").or_default() += 1;
            continue;
        }
        let key = match (case.metric, expect(&case)) {
            (Some(_), Expect::Rows(_)) if case.members().is_empty() => "metric alone",
            (Some(_), Expect::Rows(_)) => "metric answered",
            (Some(_), Expect::Refused(t)) if t.contains(&"many-to-many") => "metric many-to-many",
            (Some(_), Expect::Refused(_)) => "metric multi-path",
            (Some(_), Expect::Either) => "metric unrestated",
            (None, Expect::Rows(_)) => "dims answered",
            (None, Expect::Refused(t)) if t == ["more than one path"] => "dims multi-path",
            (None, Expect::Refused(t)) if t == ["many-to-many"] => "dims two bridges",
            (None, Expect::Refused(_)) => "dims unrelated",
            (None, Expect::Either) => unreachable!("a query with no metrics is always restated"),
        };
        *seen.entry(key).or_default() += 1;
    }
    for key in [
        "no shared table",
        "metric alone",
        "metric answered",
        "metric multi-path",
        "metric many-to-many",
        "metric unrestated",
        "dims answered",
        "dims multi-path",
        "dims two bridges",
        "dims unrelated",
    ] {
        assert!(
            seen.get(key).copied().unwrap_or(0) >= 10,
            "outcome '{key}' reached {} times in 1000 cases: {seen:?}",
            seen.get(key).copied().unwrap_or(0)
        );
    }
}

/// Found by the property: `t3 -> t2 -> t1` is the one chain from the metric's
/// table to `t1`, but `t1` also references `t0`, which `t3` references. With
/// `t3` declared first the join tree reached `t1` through `t0` (a fan-out),
/// and the one `t3` row, whose `f0` is NULL, read no `t1` row: `NULL` where
/// the chain gives `0`.
#[test]
fn a_chain_is_followed_beside_a_shorter_path_that_fans_out() {
    let case = Case {
        n: 4,
        edges: vec![(1, 0), (2, 1), (3, 0), (3, 2)],
        rows: vec![
            vec![(None, None, vec![])],
            vec![(Some(0), None, vec![None])],
            vec![(None, None, vec![Some(0)])],
            vec![(None, None, vec![None, Some(0)])],
        ],
        orders: [vec![3, 1, 2, 0], vec![0, 1, 2, 3]],
        metric: Some(3),
        dims: vec![1],
        filter: None,
    };
    check_case(&case).unwrap();
}
