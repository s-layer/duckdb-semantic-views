//! Differential proptest for the window-metric expansion path (code-review
//! 2026-07-18 PBT-1 follow-up — the window facet of "no randomized coverage of
//! the hardest expansion semantics").
//!
//! `expand_window_metrics` (src/expand/window.rs) turns a window metric into a
//! two-level query: a CTE (`__sv_agg`) that aggregates the inner metric by ALL
//! queried dimensions, then an outer SELECT that applies the window function
//! over that CTE with a computed `PARTITION BY`. The partition is derived two
//! ways — `PARTITION BY EXCLUDING <dims>` (all queried dims MINUS the excluded
//! set) or an explicit `PARTITION BY <dims>` — and that derivation, together
//! with the pre-aggregation grain, is the path's real correctness surface. The
//! example tests pin a handful of fixed shapes (three dims, one EXCLUDING set);
//! nothing exercised the partition math over random dim subsets with NULL keys.
//!
//! Shape: a single table `s(c0, c1, c2, v)`, three dimensions `d{i} = s.c{i}`,
//! and one window metric `w = <FUNC>(SUM(s.v)) OVER (PARTITION BY …)`. The
//! declared partition config (EXCLUDING vs explicit PARTITION, and which dims)
//! and the queried dim subset (always a superset of the required dims) are
//! randomized, as is `<FUNC>`.
//!
//! For every query, the extension's SQL is compared against an **independently
//! written** oracle via the same symmetric `EXCEPT ALL` multiset diff the
//! sibling harnesses use. The oracle computes the window value with a
//! **correlated subquery** grouped on the partition key (`IS NOT DISTINCT FROM`,
//! so NULL keys partition together exactly as `PARTITION BY` does) rather than
//! the extension's `OVER (PARTITION BY …)` clause — so a wrong partition set (an
//! excluded dim left in, or the wrong dim excluded) or a wrong pre-aggregation
//! grain surfaces as a non-zero diff.
//!
//! Scope: the window functions under test are the exact-over-integers set
//! (`SUM`/`COUNT`/`MIN`/`MAX`) so the multiset diff needs no float tolerance,
//! and the windows are partition-only. `ORDER BY`/frame passthrough is left to
//! the example tests (and the semi-additive harness already stresses ordered,
//! NULLS-placed RANK windows); the partition/grain derivation is the piece with
//! no prior randomized coverage.
//!
//! # PBT-6 / TECH-DEBT #41 — the `where_clause` axis
//!
//! The generated query now also carries a randomized pre-aggregation
//! `where_clause`. On this path the predicate is injected into `__sv_agg` —
//! *before* the window function runs — so it does two things at once: it
//! changes the inner `SUM` of each surviving group, and, when it empties a
//! group entirely, it removes that row from the partition the window then
//! aggregates over. A filter applied after the window would do neither.
//!
//! The oracle mirrors that by filtering the `agg` CTE and nothing else: the
//! correlated subquery reads from the already-filtered CTE, so partition
//! membership follows automatically. Filter members (`LABELS = (FILTER)`) with
//! compound `OR` expressions keep member substitution non-identity and its
//! parenthesization load-bearing, as in the sibling harnesses.

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, MetricName, QueryRequest};
use semantic_views::model::{
    AccessModifier, Dimension, Metric, SemanticViewDefinition, TableRef, WindowSpec,
};

/// Number of dimensions (physical `s.c{i}` ↔ logical `d{i}` by index).
const NDIMS: usize = 3;

/// A generated pre-aggregation predicate over the dimensions (PBT-6).
///
/// `Cmp` names a dimension member by index; `Filter` names the filter member
/// `f{i}`, whose declared expression is the compound `s.c{i} = 0 OR s.c{i} = 2`.
#[derive(Debug, Clone)]
enum Pred {
    Cmp(usize, &'static str, i64),
    Filter(usize),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
}

/// The expression declared for filter member `f{i}`.
fn filter_expr(i: usize) -> String {
    format!("{c} = 0 OR {c} = 2", c = dim_col(i))
}

impl Pred {
    /// The `where_clause` text: member NAMES, composites parenthesized so the
    /// SQL parse matches this AST, filter members left BARE.
    fn to_member_sql(&self) -> String {
        match self {
            Pred::Cmp(i, op, k) => format!("{} {op} {k}", dim_name(*i)),
            Pred::Filter(i) => format!("f{i}"),
            Pred::And(a, b) => format!("({} AND {})", a.to_member_sql(), b.to_member_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_member_sql(), b.to_member_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_member_sql()),
        }
    }

    /// The oracle's independent rendering: raw columns, filter members expanded,
    /// every operand explicitly parenthesized.
    fn to_raw_sql(&self) -> String {
        match self {
            Pred::Cmp(i, op, k) => format!("({} {op} {k})", dim_col(*i)),
            Pred::Filter(i) => format!("({})", filter_expr(*i)),
            Pred::And(a, b) => format!("({} AND {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_raw_sql()),
        }
    }

    /// Whether any named member is a filter member (generator-coverage guard).
    fn references_filter(&self) -> bool {
        match self {
            Pred::Cmp(..) => false,
            Pred::Filter(_) => true,
            Pred::And(a, b) | Pred::Or(a, b) => a.references_filter() || b.references_filter(),
            Pred::Not(a) => a.references_filter(),
        }
    }

    /// The dimension indices this predicate constrains (generator-coverage
    /// guard): used to check that predicates on dims *outside* the effective
    /// partition are generated, which change the inner aggregate without
    /// changing the partition key.
    fn dims(&self, out: &mut Vec<usize>) {
        match self {
            Pred::Cmp(i, _, _) | Pred::Filter(i) => out.push(*i),
            Pred::And(a, b) | Pred::Or(a, b) => {
                a.dims(out);
                b.dims(out);
            }
            Pred::Not(a) => a.dims(out),
        }
    }
}

/// Predicates over the three dimensions. Literals span `-1..=3` against a
/// dimension domain of `0..3` + NULL, so always-true, selective and
/// everything-filtered-away cases all occur — the last emptying a group and
/// removing it from its partition.
fn arb_pred() -> impl Strategy<Value = Pred> {
    let leaf = prop_oneof![
        3 => (
            0..NDIMS,
            prop_oneof![Just("<"), Just("<="), Just("="), Just("<>"), Just(">="), Just(">")],
            -1i64..=3,
        ).prop_map(|(i, op, k)| Pred::Cmp(i, op, k)),
        2 => (0..NDIMS).prop_map(Pred::Filter),
    ];
    leaf.prop_recursive(3, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            inner.prop_map(|a| Pred::Not(Box::new(a))),
        ]
    })
}

/// Logical dimension name for index `i` (`d0`/`d1`/`d2`).
fn dim_name(i: usize) -> String {
    format!("d{i}")
}

/// Physical column expression for dimension index `i` (`s.c0`/`s.c1`/`s.c2`).
fn dim_col(i: usize) -> String {
    format!("s.c{i}")
}

/// A generated instance: rows of `(c0, c1, c2, v)`. `None` is a SQL NULL. Small
/// dim domains force duplicate dim-tuples (so partitions hold several rows) and
/// NULL partition keys; the value domain includes NULL so the inner `SUM` can
/// itself be NULL for a group (exercising `COUNT`/`MIN`/`MAX` NULL handling).
#[derive(Debug, Clone)]
struct Instance {
    rows: Vec<(Option<i64>, Option<i64>, Option<i64>, Option<i64>)>,
}

/// The window function under test — all exact over integer inputs, so the
/// symmetric `EXCEPT ALL` diff needs no float tolerance.
#[derive(Debug, Clone, Copy)]
enum WFunc {
    Sum,
    Count,
    Min,
    Max,
}

impl WFunc {
    fn name(self) -> &'static str {
        match self {
            WFunc::Sum => "SUM",
            WFunc::Count => "COUNT",
            WFunc::Min => "MIN",
            WFunc::Max => "MAX",
        }
    }
}

/// How the metric declares its partition. Dim indices are into `0..NDIMS`.
#[derive(Debug, Clone)]
enum PartMode {
    /// `PARTITION BY EXCLUDING <dims>` — effective partition is queried dims
    /// minus these (may be empty).
    Excluding(Vec<usize>),
    /// Explicit `PARTITION BY <dims>` — non-empty; effective partition is
    /// exactly these.
    Partition(Vec<usize>),
}

/// A full case: an instance, the window function, the declared partition config,
/// and the queried dim subset (always a superset of the config's required dims).
#[derive(Debug, Clone)]
struct Case {
    inst: Instance,
    func: WFunc,
    mode: PartMode,
    sel_dims: Vec<usize>,
    /// The pre-aggregation predicate (PBT-6). `None` one case in four, keeping
    /// the original no-predicate coverage rather than replacing it.
    where_pred: Option<Pred>,
}

fn arb_instance() -> impl Strategy<Value = Instance> {
    let v_cell = prop_oneof![
        1 => Just(None),
        3 => (-5i64..=5).prop_map(Some),
    ];
    let dim_cell = || {
        prop_oneof![
            1 => Just(None),
            4 => (0i64..3).prop_map(Some),
        ]
    };
    let row = (dim_cell(), dim_cell(), dim_cell(), v_cell);
    prop::collection::vec(row, 0..=20).prop_map(|rows| Instance { rows })
}

fn arb_func() -> impl Strategy<Value = WFunc> {
    prop_oneof![
        Just(WFunc::Sum),
        Just(WFunc::Count),
        Just(WFunc::Min),
        Just(WFunc::Max),
    ]
}

fn arb_case() -> impl Strategy<Value = Case> {
    // Pick the metric's declared partition config first (the required dims),
    // then a query dim-set that is a superset of it — so every generated query
    // satisfies the required-dimension check and `expand` accepts it.
    (arb_instance(), arb_func(), any::<bool>()).prop_flat_map(|(inst, func, explicit)| {
        let all: Vec<usize> = (0..NDIMS).collect();
        // Explicit PARTITION BY must be non-empty; EXCLUDING may be empty.
        let req_size = if explicit { 1..=NDIMS } else { 0..=NDIMS };
        let req = prop::sample::subsequence(all, req_size);
        (Just(inst), Just(func), Just(explicit), req).prop_flat_map(
            |(inst, func, explicit, req)| {
                let rest: Vec<usize> = (0..NDIMS).filter(|d| !req.contains(d)).collect();
                let rest_len = rest.len();
                let extra = prop::sample::subsequence(rest, 0..=rest_len);
                let where_pred = prop_oneof![
                    1 => Just(None),
                    3 => arb_pred().prop_map(Some),
                ];
                (
                    Just(inst),
                    Just(func),
                    Just(explicit),
                    Just(req),
                    extra,
                    where_pred,
                )
                    .prop_map(|(inst, func, explicit, req, extra, where_pred)| {
                        let mut sel_dims: Vec<usize> = req.iter().copied().chain(extra).collect();
                        sel_dims.sort_unstable();
                        let mode = if explicit {
                            PartMode::Partition(req)
                        } else {
                            PartMode::Excluding(req)
                        };
                        Case {
                            inst,
                            func,
                            mode,
                            sel_dims,
                            where_pred,
                        }
                    })
            },
        )
    })
}

/// Build the semantic-view definition: single table `s`, three dimensions, and
/// one window metric `w` wrapping its own inner `SUM(s.v)` with the given
/// function and partition config.
fn build_def(func: WFunc, mode: &PartMode) -> SemanticViewDefinition {
    let tables = vec![TableRef {
        alias: "s".to_string(),
        table: "s".to_string(),
        definition: None,
        pk_columns: vec![],
        unique_constraints: vec![],
        comment: None,
        synonyms: vec![],
        tags: Vec::new(),
    }];
    let dimensions = (0..NDIMS)
        .map(|i| Dimension {
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
            name: dim_name(i),
            expr: dim_col(i),
            source_table: Some("s".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            tags: Vec::new(),
        })
        // PBT-6 filter members: never selected as output, never part of the
        // partition config, named only by a generated `where_clause`. Compound
        // expressions, so a splice that loses its parentheses changes which
        // rows reach the pre-aggregation.
        .chain((0..NDIMS).map(|i| Dimension {
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
            name: format!("f{i}"),
            expr: filter_expr(i),
            source_table: Some("s".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: true,
            tags: Vec::new(),
        }))
        .collect();
    let (excluding_dims, partition_dims) = match mode {
        PartMode::Excluding(d) => (d.iter().map(|&i| dim_name(i)).collect(), vec![]),
        PartMode::Partition(d) => (vec![], d.iter().map(|&i| dim_name(i)).collect()),
    };
    let metrics = vec![Metric {
        name: "w".to_string(),
        expr: "SUM(s.v)".to_string(),
        source_table: Some("s".to_string()),
        output_type: None,
        using_relationships: vec![],
        comment: None,
        synonyms: vec![],
        access: AccessModifier::Public,
        non_additive_by: vec![],
        window_spec: Some(WindowSpec {
            window_function: func.name().to_string(),
            // Self-referential inner metric (the metric wraps its own aggregate),
            // matching how the window example tests model a window metric.
            inner_metric: "w".to_string(),
            extra_args: vec![],
            excluding_dims,
            partition_dims,
            order_by: vec![],
            frame_clause: None,
        }),
        tags: Vec::new(),
    }];
    SemanticViewDefinition {
        variables: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
        tables,
        dimensions,
        metrics,
        joins: vec![],
        // PIN: a window metric is an aggregate shape; FACTS returns unaggregated rows. TECH-DEBT #66
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
    }
}

/// Create the physical table and insert the generated rows.
fn make_db(inst: &Instance) -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch("CREATE TABLE s (c0 INTEGER, c1 INTEGER, c2 INTEGER, v INTEGER);")
        .expect("create table");
    let cell = |c: &Option<i64>| c.map_or_else(|| "NULL".to_string(), |v| v.to_string());
    if !inst.rows.is_empty() {
        let values: Vec<String> = inst
            .rows
            .iter()
            .map(|(a, b, c, d)| format!("({},{},{},{})", cell(a), cell(b), cell(c), cell(d)))
            .collect();
        conn.execute_batch(&format!("INSERT INTO s VALUES {};", values.join(",")))
            .expect("insert rows");
    }
    conn
}

/// Effective partition dim indices for a case: queried dims minus the EXCLUDING
/// set, or the explicit PARTITION list.
fn partition_dims(case: &Case) -> Vec<usize> {
    match &case.mode {
        PartMode::Excluding(excl) => case
            .sel_dims
            .iter()
            .copied()
            .filter(|d| !excl.contains(d))
            .collect(),
        PartMode::Partition(p) => p.clone(),
    }
}

/// Independent oracle SQL. Structurally different from the extension's
/// `OVER (PARTITION BY …)`: the inner metric is pre-aggregated by all queried
/// dims (the `agg` CTE), then the window value is a **correlated subquery**
/// aggregating over the rows sharing the partition key (`IS NOT DISTINCT FROM`,
/// so NULL keys group together exactly as `PARTITION BY` does).
fn oracle_sql(case: &Case) -> String {
    let sel = &case.sel_dims;

    // PBT-6: the predicate belongs to the PRE-aggregation CTE and nowhere else.
    // Filtering here changes each surviving group's inner SUM and, when it
    // empties a group, removes that row from the partition the correlated
    // subquery then aggregates over — because that subquery reads from this
    // already-filtered CTE. Applying it after the window instead would leave
    // both effects out.
    let where_sql = case
        .where_pred
        .as_ref()
        .map_or_else(String::new, |p| format!(" WHERE {}", p.to_raw_sql()));

    // The pre-aggregation CTE: SUM(s.v) grouped by all queried dims.
    let agg = if sel.is_empty() {
        format!("SELECT SUM(s.v) AS w FROM s{where_sql}")
    } else {
        let agg_select = sel
            .iter()
            .map(|&i| format!("{} AS {}", dim_col(i), dim_name(i)))
            .chain(std::iter::once("SUM(s.v) AS w".to_string()))
            .collect::<Vec<_>>()
            .join(", ");
        let group_by = (1..=sel.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("SELECT {agg_select} FROM s{where_sql} GROUP BY {group_by}")
    };

    let part = partition_dims(case);
    let func = case.func.name();
    let corr_where = if part.is_empty() {
        String::new()
    } else {
        let preds = part
            .iter()
            .map(|&i| {
                let n = dim_name(i);
                format!("a2.{n} IS NOT DISTINCT FROM a1.{n}")
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        format!(" WHERE {preds}")
    };
    let window_col = format!("(SELECT {func}(a2.w) FROM agg a2{corr_where}) AS w");
    let outer_select = sel
        .iter()
        .map(|&i| format!("a1.{}", dim_name(i)))
        .chain(std::iter::once(window_col))
        .collect::<Vec<_>>()
        .join(", ");

    format!("WITH agg AS ({agg}) SELECT {outer_select} FROM agg a1")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 192, ..ProptestConfig::default() })]

    #[test]
    fn window_metric_matches_independent_oracle(case in arb_case()) {
        let def = build_def(case.func, &case.mode);
        let req = QueryRequest {
            where_clause: case.where_pred.as_ref().map(Pred::to_member_sql),
            dimensions: case.sel_dims.iter().map(|&i| DimensionName::new(dim_name(i))).collect(),
            // Always query the (single) window metric so the window path fires.
            metrics: vec![MetricName::new("w")],
            // PIN: a window metric is an aggregate shape; FACTS returns unaggregated rows. TECH-DEBT #66
            facts: vec![],
        };

        // Single table, no joins -> no fan trap; every generated query (whose
        // dim-set is a superset of the required dims) is accepted.
        let expanded = match expand("win", &def, &req) {
            Ok(sql) => sql,
            Err(e) => {
                prop_assert!(false, "single-table window query unexpectedly rejected: {e}\ncase: {case:?}");
                unreachable!()
            }
        };
        let oracle = oracle_sql(&case);

        // Canonical projection (columns sorted by name) so a column-order
        // difference between the two formulations is not a false diff.
        let mut proj_cols: Vec<String> = case.sel_dims.iter().map(|&i| dim_name(i)).collect();
        proj_cols.push("w".to_string());
        proj_cols.sort();
        let proj = proj_cols.join(", ");

        let cmp = format!(
            "SELECT \
               (SELECT count(*) FROM (SELECT {proj} FROM ({expanded}) qa \
                                      EXCEPT ALL \
                                      SELECT {proj} FROM ({oracle}) qb) e1) \
             + (SELECT count(*) FROM (SELECT {proj} FROM ({oracle}) qc \
                                      EXCEPT ALL \
                                      SELECT {proj} FROM ({expanded}) qd) e2) AS diff"
        );

        let conn = make_db(&case.inst);
        let diff: i64 = conn.query_row(&cmp, [], |r| r.get(0)).unwrap_or_else(|e| {
            panic!("differential comparison query failed: {e}\n--- expanded:\n{expanded}\n--- oracle:\n{oracle}")
        });

        prop_assert_eq!(
            diff, 0,
            "window-metric expansion disagrees with the independent partition oracle \
             (symmetric multiset diff = {}); func={:?} mode={:?} dims={:?}\n--- expanded:\n{}\n--- oracle:\n{}",
            diff, case.func, case.mode, case.sel_dims, expanded, oracle
        );
    }
}

/// PBT-6 guard: prove the generator varies the predicate and reaches the shape
/// that makes this path distinctive.
///
/// The interesting one here is a predicate constraining a dimension that is
/// **not** in the effective partition: it changes the inner aggregate of rows
/// inside a partition without changing the partition key, so the window value
/// moves while the grouping does not. If the generator stopped producing those,
/// the property would still pass while covering only the easy case.
#[test]
fn generator_varies_the_predicate_and_reaches_non_partition_dims() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::HashSet;

    let mut runner = TestRunner::deterministic();
    let mut with_pred = 0usize;
    let mut without_pred = 0usize;
    let mut with_filter = 0usize;
    let mut pred_off_partition = 0usize;
    let mut distinct: HashSet<String> = HashSet::new();

    for _ in 0..400 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        let part = partition_dims(&case);
        match &case.where_pred {
            None => without_pred += 1,
            Some(p) => {
                with_pred += 1;
                if p.references_filter() {
                    with_filter += 1;
                }
                let mut ds = Vec::new();
                p.dims(&mut ds);
                // A constrained dim that is queried but NOT part of the
                // partition key.
                if ds
                    .iter()
                    .any(|d| case.sel_dims.contains(d) && !part.contains(d))
                {
                    pred_off_partition += 1;
                }
                distinct.insert(p.to_member_sql());
            }
        }
    }

    assert!(
        with_pred > 0 && without_pred > 0,
        "where_clause must be both present and absent across cases \
         (present={with_pred}, absent={without_pred})"
    );
    assert!(
        with_filter > 0,
        "generator never referenced a filter member -- member substitution and \
         its parenthesization are untested"
    );
    assert!(
        pred_off_partition > 0,
        "no case constrained a queried dimension outside the effective partition; \
         that is the shape where filtering moves the window value without moving \
         the partition key, so the extension would cover only the easy case"
    );
    assert!(
        distinct.len() > 50,
        "generator produced only {} distinct predicates; search space collapsed",
        distinct.len()
    );
}
