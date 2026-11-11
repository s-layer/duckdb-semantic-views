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
//! `SUM(t{k}.v)`; the data has NULL and dangling foreign keys. A quarter of
//! the graphs double one relationship (`t{a}.f{b}b REFERENCES t{b}`, a second
//! role for `t{b}`), and `t{a}` then has `s{a}_a` and `s{a}_b`, the same sum
//! with a `USING` naming one role each.
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
//!   rows; with two chains to a member or two nearest tables (two bridges), the
//!   query is refused. With none, the members are answered from the table
//!   their paths meet at, if it has a metric (Phase 7 PR 4d, fixture 7's D4
//!   and N8; every table here has one): each pair of members joined by one
//!   path over relationships in either direction, and a table on those paths
//!   that shares with every member a table under both (one that reaches each
//!   by a chain, or is it), the lowest such table if a member is above it.
//!   This is restated from chains, not from the hop-by-hop walk the
//!   extension makes.
//!   The answer is the distinct member values over that table's rows
//!   inner-joined along those paths, each table once. With no such table, or
//!   a path across the doubled relationship, the query is refused as not
//!   related.
//! - **A doubled relationship.** A chain across it counts once per role, so a
//!   member reached across it is refused (no role picked) unless the metric is
//!   on `t{a}` and its `USING` names a role, which then is the one chain
//!   followed. A member on `t{b}` or past it that no chain crosses to, a metric
//!   on `t{b}` or past it with other members, a `USING` beside a chain that
//!   does not cross, a `USING` beside a filter on `t{b}` or past it (a filter
//!   names no role), and a query with no metrics near the role-played table
//!   are not restated (only the two TABLES orders are compared).
//! - **Any query** gives the same rows, or is refused, in two TABLES orders
//!   (with the same message, unless a relationship is doubled).
//!
//! Rows are compared as sorted multisets of text, every column cast to
//! VARCHAR inside DuckDB.

use proptest::prelude::*;
use semantic_views::expand::{
    dimensions_for_metric, expand, DimensionName, MetricName, QueryRequest,
};
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
    /// Aligned with `edges`: the relationship is declared one-to-one, so a
    /// step back along it does not fan out. The data does not keep it one to
    /// one, so an answer that steps back along one where a chain of foreign
    /// keys leads gives other rows than the oracle's.
    one_to_one: Vec<bool>,
    rows: Vec<Rows>,
    /// Two TABLES orders, each a permutation of `0..n`.
    orders: [Vec<usize>; 2],
    /// The metric's table, or `None` for a query with no metrics.
    metric: Option<usize>,
    dims: Vec<usize>,
    /// `g{k} = c` as the query's filter.
    filter: Option<(usize, i64)>,
    /// An index into `edges`: that relationship is declared twice, the second
    /// time over column `f{to}b` (a second role for its table).
    doubled: Option<usize>,
    /// For a metric on the doubled relationship's referencing table, the role
    /// its `USING` names: `false` the first relationship, `true` the second.
    using: Option<bool>,
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

    /// The doubled relationship, `(from, to)`.
    fn pair(&self) -> Option<(usize, usize)> {
        self.doubled.map(|i| self.edges[i])
    }

    /// The role the queried metric's `USING` names, if it names one.
    fn role(&self) -> Option<bool> {
        match (self.pair(), self.metric) {
            (Some((from, _)), Some(m)) if m == from => self.using,
            _ => None,
        }
    }

    /// Is `k` the role-played table, or reached from it by a chain?
    fn past_roles(&self, k: usize) -> bool {
        self.pair()
            .is_some_and(|(_, to)| !self.chains(to, k).is_empty())
    }

    /// [`Self::chains`], each crossing the doubled relationship once per role:
    /// `Some(role)` for a chain across it, `None` for one that does not cross.
    fn role_chains(&self, from: usize, to: usize) -> Vec<(Vec<usize>, Option<bool>)> {
        let mut out = Vec::new();
        for chain in self.chains(from, to) {
            let crosses = self
                .pair()
                .is_some_and(|(a, b)| chain.windows(2).any(|w| w == [a, b]));
            if crosses {
                out.push((chain.clone(), Some(false)));
                out.push((chain, Some(true)));
            } else {
                out.push((chain, None));
            }
        }
        out
    }

    /// The one chain the oracle follows from `from` to `to`: across the role
    /// the metric's `USING` names, when it names one.
    fn chosen_chain(&self, from: usize, to: usize) -> (Vec<usize>, Option<bool>) {
        let role = self.role();
        self.role_chains(from, to)
            .into_iter()
            .find(|(_, r)| r.is_none() || *r == role)
            .expect("an answered member has a chain")
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
            let one_to_one = prop::collection::vec(prop::bool::weighted(0.25), edges.len());
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
            let doubled = prop_oneof![3 => Just(None), 1 => (0..edges.len()).prop_map(Some)];
            let using = prop_oneof![1 => Just(None), 2 => any::<bool>().prop_map(Some)];
            (
                (Just(n), Just(edges), one_to_one, rows, order(), order()),
                (
                    metric,
                    dims,
                    filter,
                    doubled,
                    using,
                    prop::bool::weighted(0.5),
                ),
            )
        })
        .prop_filter(
            "a query with no metric names a dimension",
            |(_, (metric, dims, ..))| metric.is_some() || !dims.is_empty(),
        )
        .prop_map(
            |(
                (n, edges, one_to_one, rows, o1, o2),
                (metric, dims, filter, doubled, using, aim),
            )| {
                let mut case = Case {
                    n,
                    edges,
                    one_to_one,
                    rows,
                    orders: [o1, o2],
                    metric,
                    dims,
                    filter,
                    doubled,
                    using,
                };
                // Half the doubled graphs query the referencing table's metric
                // by the role-played table's dimension, which random draws
                // reach too rarely.
                if let (true, Some((from, to))) = (aim, case.pair()) {
                    case.metric = Some(from);
                    if !case.dims.contains(&to) {
                        case.dims.push(to);
                    }
                }
                case
            },
        )
}

/// The view, its TABLES in `order`, its RELATIONSHIPS reversed when
/// `reverse_relationships` (so the two declarations differ in both).
fn build_def(case: &Case, order: &[usize], reverse_relationships: bool) -> SemanticViewDefinition {
    let t = |k: usize| format!("t{k}");
    let mut joins: Vec<Join> = case
        .edges
        .iter()
        .zip(&case.one_to_one)
        .map(|(&(from, to), &one)| Join {
            table: t(to),
            from_alias: t(from),
            fk_columns: vec![format!("f{to}")],
            ref_columns: vec!["id".to_string()],
            name: Some(format!("r{from}_{to}")),
            cardinality: if one {
                Cardinality::OneToOne
            } else {
                Cardinality::ManyToOne
            },
            ..Default::default()
        })
        .collect();
    let mut metrics: Vec<Metric> = (0..case.n)
        .map(|k| Metric {
            name: format!("s{k}"),
            expr: format!("SUM(t{k}.v)"),
            source_table: Some(t(k)),
            ..Default::default()
        })
        .collect();
    if let (Some(i), Some((from, to))) = (case.doubled, case.pair()) {
        joins.insert(
            i + 1,
            Join {
                table: t(to),
                from_alias: t(from),
                fk_columns: vec![format!("f{to}b")],
                ref_columns: vec!["id".to_string()],
                name: Some(format!("r{from}_{to}b")),
                cardinality: Cardinality::ManyToOne,
                ..Default::default()
            },
        );
        for (suffix, rel) in [
            ("a", format!("r{from}_{to}")),
            ("b", format!("r{from}_{to}b")),
        ] {
            metrics.push(Metric {
                name: format!("s{from}_{suffix}"),
                expr: format!("SUM(t{from}.v)"),
                source_table: Some(t(from)),
                using_relationships: vec![rel],
                ..Default::default()
            });
        }
    }
    if reverse_relationships {
        joins.reverse();
    }
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
        joins,
        dimensions: (0..case.n)
            .map(|k| Dimension {
                name: format!("g{k}"),
                expr: format!("t{k}.g"),
                source_table: Some(t(k)),
                ..Default::default()
            })
            .collect(),
        metrics,
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
            .map(|k| match case.role() {
                Some(false) => MetricName::new(format!("s{k}_a")),
                Some(true) => MetricName::new(format!("s{k}_b")),
                None => MetricName::new(format!("s{k}")),
            })
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
        let second = case.pair().filter(|(from, _)| *from == k).map(|(_, to)| to);
        let mut fk_cols: String = outs.iter().map(|to| format!(", f{to} INTEGER")).collect();
        if let Some(to) = second {
            fk_cols.push_str(&format!(", f{to}b INTEGER"));
        }
        conn.execute_batch(&format!(
            "CREATE TABLE t{k} (id INTEGER, g INTEGER, v INTEGER{fk_cols});"
        ))
        .expect("create table");
        let values: Vec<String> = case.rows[k]
            .iter()
            .enumerate()
            .map(|(i, (g, v, fks))| {
                let mut fks: String = fks.iter().map(|f| format!(", {}", cell(f))).collect();
                if second.is_some() {
                    // The second role's key: other rows than the first's, one
                    // NULL in four and some dangling (4 is past every table).
                    let b = (i % 4 != 3).then_some((i as i64 * 2 + 1) % 5);
                    fks.push_str(&format!(", {}", cell(&b)));
                }
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
/// A chain across the doubled relationship in its second role joins over the
/// second key, `f{to}b`.
fn joined_from(
    case: &Case,
    from: usize,
    chains: &[(usize, Vec<usize>, Option<bool>)],
) -> (String, Vec<(usize, String)>) {
    let mut sql = format!("t{from} AS x");
    let mut at: Vec<(usize, String)> = vec![(from, "x".to_string())];
    for (ci, (member, chain, role)) in chains.iter().enumerate() {
        let mut prev = "x".to_string();
        for (step, pair) in chain.windows(2).enumerate() {
            let alias = format!("c{ci}_{step}");
            let second = *role == Some(true) && case.pair() == Some((pair[0], pair[1]));
            sql.push_str(&format!(
                " LEFT JOIN t{to} AS {alias} ON {prev}.f{to}{b} = {alias}.id",
                to = pair[1],
                b = if second { "b" } else { "" }
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
    let chains: Vec<(usize, Vec<usize>, Option<bool>)> = case
        .members()
        .into_iter()
        .map(|m| {
            let (chain, role) = case.chosen_chain(from, m);
            (m, chain, role)
        })
        .collect();
    let (from_sql, at) = joined_from(case, from, &chains);
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
    /// Answered with no metric from the table the members' paths meet at
    /// ([`meeting_table`]), inner-joined along those paths ([`meet_sql`]).
    Meet(usize),
    /// Refused, with a message holding one of these texts (the first member
    /// the expansion checks decides which).
    Refused(Vec<&'static str>),
    /// Not restated here (a member reached by no chain from the metric's
    /// table): only the two TABLES orders are compared.
    Either,
}

/// The paths from `from` to `to` over relationships in either direction, each
/// table at most once, found by brute force (a graph has at most five
/// tables). The doubled relationship is one step here.
fn undirected_paths(case: &Case, from: usize, to: usize) -> Vec<Vec<usize>> {
    fn walk(case: &Case, at: usize, to: usize, seen: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if at == to {
            out.push(seen.clone());
            return;
        }
        for next in 0..case.n {
            let linked = case.edges.contains(&(at, next)) || case.edges.contains(&(next, at));
            if linked && !seen.contains(&next) {
                seen.push(next);
                walk(case, next, to, seen, out);
                seen.pop();
            }
        }
    }
    let mut out = Vec::new();
    walk(case, from, to, &mut vec![from], &mut out);
    out
}

fn expect(case: &Case) -> Expect {
    let members = case.members();
    if let Some(m) = case.metric {
        if case.past_roles(m) && members.iter().any(|&d| d != m) {
            return Expect::Either;
        }
        // A filter carries no `USING`, so one past the role-played table is
        // refused whatever role the metric names: a fence of its own.
        if case.role().is_some() && case.filter.is_some_and(|(k, _)| case.past_roles(k)) {
            return Expect::Either;
        }
        let crosses_roles = |path: &[usize]| {
            case.pair()
                .is_some_and(|(a, b)| path.windows(2).any(|w| w == [a, b] || w == [b, a]))
        };
        let mut refusals: Vec<&'static str> = Vec::new();
        let mut restated = true;
        for &d in &members {
            let chains = case.role_chains(m, d);
            let across = chains.iter().filter(|(_, r)| r.is_some()).count();
            let beside = chains.len() - across;
            if across == 0 && case.past_roles(d) {
                // A chain that does not cross to a role-played table: the
                // role-playing check refuses its dimension without a USING.
                restated = false;
                continue;
            }
            let count = match case.role() {
                Some(_) if across > 0 && beside > 0 => {
                    restated = false;
                    continue;
                }
                Some(_) => across / 2 + beside,
                None => chains.len(),
            };
            match count {
                0 => {
                    let paths = undirected_paths(case, m, d);
                    if paths.iter().any(|p| crosses_roles(p)) {
                        restated = false;
                    } else if paths.len() > 1 {
                        refusals.push("many-to-many");
                    }
                }
                1 => {}
                _ if across > 0 => refusals.extend(["ambiguous", "more than one path"]),
                _ => refusals.push("more than one path"),
            }
        }
        if !refusals.is_empty() {
            return Expect::Refused(refusals);
        }
        let one_each = members.iter().all(|&d| {
            let chains = case.role_chains(m, d);
            match case.role() {
                Some(r) => {
                    chains
                        .iter()
                        .filter(|(_, x)| x.is_none() || *x == Some(r))
                        .count()
                        == 1
                }
                None => chains.len() == 1,
            }
        });
        if restated && one_each {
            return Expect::Rows(m);
        }
        return Expect::Either;
    }
    if members.iter().any(|&d| case.past_roles(d)) {
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
        [] => match meeting_table(case, &members) {
            Some(meet) => Expect::Meet(meet),
            None => Expect::Refused(vec!["not related"]),
        },
        [anchor] if members.iter().all(|&d| case.chains(*anchor, d).len() == 1) => {
            Expect::Rows(*anchor)
        }
        [_] => Expect::Refused(vec!["more than one path"]),
        _ => Expect::Refused(vec!["many-to-many"]),
    }
}

/// The table a query with no metrics, whose members no table reaches by
/// chains, is answered from (see the module docs), or `None` when it is
/// refused as not related.
///
/// Restated from chains of foreign keys rather than by walking the paths
/// hop by hop as `sql_gen::meeting_table` does: a table meets a member when
/// some table is under both (reaches each by a chain, or is it), so the
/// member is reached by fanning out to that table and climbing from it.
fn meeting_table(case: &Case, members: &[usize]) -> Option<usize> {
    let doubled = |w: &[usize]| case.pair().is_some_and(|(a, b)| w == [a, b] || w == [b, a]);
    let mut between: Vec<usize> = Vec::new();
    for (i, &a) in members.iter().enumerate() {
        for &b in &members[i + 1..] {
            let paths = undirected_paths(case, a, b);
            let [path] = paths.as_slice() else {
                return None;
            };
            if path.windows(2).any(doubled) {
                return None;
            }
            for &t in path {
                if !between.contains(&t) {
                    between.push(t);
                }
            }
        }
    }
    // `x` is `t`, or reaches it by a chain of foreign keys.
    let under = |x: usize, t: usize| !case.chains(x, t).is_empty();
    let meets = |t: usize| {
        members
            .iter()
            .all(|&d| (0..case.n).any(|x| under(x, t) && under(x, d)))
    };
    let meeting: Vec<usize> = between.iter().copied().filter(|&t| meets(t)).collect();
    let lowest: Vec<usize> = meeting
        .iter()
        .copied()
        .filter(|&t| !meeting.iter().any(|&o| o != t && under(o, t)))
        .collect();
    match lowest.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// The oracle's SQL for [`Expect::Meet`]: `t{meet}`, each table on the
/// members' paths inner-joined once, the distinct member values.
fn meet_sql(case: &Case, meet: usize) -> String {
    let mut sql = format!("t{meet} AS a{meet}");
    let mut joined = vec![meet];
    for d in case.members() {
        let path = undirected_paths(case, meet, d).remove(0);
        for w in path.windows(2) {
            let (p, q) = (w[0], w[1]);
            if joined.contains(&q) {
                continue;
            }
            joined.push(q);
            let on = if case.edges.contains(&(p, q)) {
                format!("a{p}.f{q} = a{q}.id")
            } else {
                format!("a{q}.f{p} = a{p}.id")
            };
            sql.push_str(&format!(" JOIN t{q} AS a{q} ON {on}"));
        }
    }
    let items: Vec<String> = case
        .dims
        .iter()
        .map(|&k| format!("a{k}.g AS g{k}"))
        .collect();
    let filter = case
        .filter
        .map(|(k, c)| format!(" WHERE a{k}.g = {c}"))
        .unwrap_or_default();
    format!("SELECT DISTINCT {} FROM {sql}{filter}", items.join(", "))
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
    for (i, order) in case.orders.iter().enumerate() {
        let def = build_def(case, order, i == 1);
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
            (Expect::Meet(meet), Ok(got)) => {
                let oracle = meet_sql(case, meet);
                let want = rows(&conn, &oracle)
                    .map_err(|e| TestCaseError::fail(format!("oracle fails: {e}\n{oracle}")))?;
                prop_assert_eq!(got, &want, "order {:?}\n--- oracle:\n{}", order, oracle);
            }
            (Expect::Rows(_) | Expect::Meet(_), Err(e)) => {
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
    // With a doubled relationship more fences can refuse one query (a fan
    // trap relative to the base table, a filter that names no role), and
    // which speaks first may follow the TABLES order: the two orders must
    // agree on the rows, and on refusing, but not on the message.
    let (a, b) = (&answers[0], &answers[1]);
    if case.doubled.is_some() && a.is_err() && b.is_err() {
        return Ok(());
    }
    prop_assert_eq!(
        a,
        b,
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 4096, ..ProptestConfig::default() })]

    /// Members no table reaches by chains, with no metric, are rare in
    /// [`arb_case`] (about one draw in fifty), so this runs only those: the
    /// meeting table's answer (Phase 7 PR 4d) and the refusal when there is
    /// none. The other draws are skipped unchecked.
    #[test]
    fn members_no_table_reaches_meet_or_are_unrelated(case in arb_case()) {
        let shape = match expect(&case) {
            Expect::Meet(_) => true,
            Expect::Refused(texts) => texts == ["not related"],
            _ => false,
        };
        if case.metric.is_none() && shape {
            check_case(&case)?;
        }
    }
}

/// How `SHOW SEMANTIC DIMENSIONS ... FOR METRIC` and the query agree on one
/// metric and one dimension of `case`'s graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Listing {
    /// Listed, and a query of the metric grouped by it alone is answered.
    Answered,
    /// Answered, and the dimension's table is reached from the metric's by
    /// no chain of foreign keys: a fan-out (Phase 7 PR 4e, fixture 8 S1-S3).
    AnsweredFanned,
    /// Not listed, and refused.
    Refused,
    /// Listed and refused, reached two ways (two paths, or across the doubled
    /// relationship), so a query needs `USING` to pick one.
    ListedTwoWays,
    /// Listed and refused, with the metric's or the dimension's table the
    /// role-played one or past it. The query refuses some of these that
    /// Snowflake answers, a metric on the role-played table by its own
    /// dimension among them (TECH-DEBT #111, Phase 7 PR 4b), so the listing
    /// is not held to the query there.
    ListedNearRoles,
}

/// Checks every metric against every dimension in `case`'s graph (the first
/// TABLES order): each dimension a query of the metric grouped by it alone
/// answers is listed, and a listed one the query refuses is reached two ways
/// (or is near the role-played table, [`Listing::ListedNearRoles`]).
/// So the listing follows the query, which the other properties check
/// against Snowflake's rules.
fn check_listing(case: &Case) -> Result<Vec<Listing>, TestCaseError> {
    let def = build_def(case, &case.orders[0], false);
    let mut out = Vec::new();
    for met in &def.metrics {
        let listed: Vec<&str> = dimensions_for_metric(&def, met)
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        let m: usize = met.source_table.as_deref().expect("a base metric")[1..]
            .parse()
            .expect("t{k}");
        for d in 0..case.n {
            let req = QueryRequest {
                dimensions: vec![DimensionName::new(format!("g{d}"))],
                metrics: vec![MetricName::new(met.name.clone())],
                ..Default::default()
            };
            let answered = expand("dag", &def, &req);
            let is_listed = listed.contains(&format!("g{d}").as_str());
            let crosses_roles = case.pair().is_some_and(|(a, b)| {
                undirected_paths(case, m, d)
                    .iter()
                    .any(|p| p.windows(2).any(|w| w == [a, b] || w == [b, a]))
            });
            let two_ways = undirected_paths(case, m, d).len() > 1 || crosses_roles;
            let outcome = match (&answered, is_listed) {
                (Ok(_), true) if case.chains(m, d).is_empty() => Listing::AnsweredFanned,
                (Ok(_), true) => Listing::Answered,
                (Err(_), false) => Listing::Refused,
                (Err(_), true) if two_ways => Listing::ListedTwoWays,
                (Err(_), true) if case.past_roles(m) || case.past_roles(d) => {
                    Listing::ListedNearRoles
                }
                (Ok(_), false) => {
                    return Err(TestCaseError::fail(format!(
                        "{} by g{d} is answered but not listed: {listed:?}\n{case:?}",
                        met.name
                    )))
                }
                (Err(e), true) => {
                    return Err(TestCaseError::fail(format!(
                        "{} by g{d} is listed, reached one way, and refused: {e}\n{case:?}",
                        met.name
                    )))
                }
            };
            out.push(outcome);
        }
    }
    Ok(out)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn show_lists_the_dimensions_a_query_of_the_metric_answers(case in arb_case()) {
        check_listing(&case)?;
    }
}

/// The listing property sees every kind of outcome, fanned answers and
/// two-way listings included, so none of its branches is vacuous.
#[test]
fn listing_reaches_every_outcome() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let mut seen: std::collections::HashMap<Listing, usize> = std::collections::HashMap::new();
    for _ in 0..300 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        for outcome in check_listing(&case).expect("the listing property holds") {
            *seen.entry(outcome).or_default() += 1;
        }
    }
    for outcome in [
        Listing::Answered,
        Listing::AnsweredFanned,
        Listing::Refused,
        Listing::ListedTwoWays,
    ] {
        assert!(
            seen.get(&outcome).copied().unwrap_or(0) >= 20,
            "{outcome:?} reached {} times: {seen:?}",
            seen.get(&outcome).copied().unwrap_or(0)
        );
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
    // 3000 draws: a query with no metric whose members no table reaches by
    // chains (meeting table or unrelated) is about one draw in fifty.
    for _ in 0..3000 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        let across = case.metric.is_some_and(|m| {
            case.members()
                .iter()
                .any(|&d| case.role_chains(m, d).iter().any(|(_, r)| r.is_some()))
        });
        if across {
            let key = match (case.role(), expect(&case)) {
                (None, Expect::Refused(_)) => "role refused",
                (Some(_), Expect::Rows(_)) => "role answered",
                _ => "role other",
            };
            *seen.entry(key).or_default() += 1;
            continue;
        }
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
            (Some(_), Expect::Meet(_)) => unreachable!("a metric query never meets"),
            (None, Expect::Rows(_)) => "dims answered",
            (None, Expect::Meet(_)) => "dims meeting table",
            (None, Expect::Refused(t)) if t == ["more than one path"] => "dims multi-path",
            (None, Expect::Refused(t)) if t == ["many-to-many"] => "dims two bridges",
            (None, Expect::Refused(_)) => "dims unrelated",
            (None, Expect::Either) => "dims near a role-played table",
        };
        *seen.entry(key).or_default() += 1;
    }
    for key in [
        "role refused",
        "role answered",
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
        "dims meeting table",
    ] {
        assert!(
            seen.get(key).copied().unwrap_or(0) >= 10,
            "outcome '{key}' reached {} times in 3000 cases: {seen:?}",
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
        one_to_one: vec![false; 4],
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
        doubled: None,
        using: None,
    };
    let def = build_def(&case, &case.orders[0], false);
    println!("{:?}", expand("dag", &def, &request(&case)));
    println!("{:?}", check_listing(&case));
}
