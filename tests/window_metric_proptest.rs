//! Differential proptest for the window-metric expansion path (code-review
//! 2026-07-18 PBT-1 follow-up — the window facet of "no randomized coverage of
//! the hardest expansion semantics").
//!
//! `expand_over_result` (src/expand/window.rs) turns a window metric into a
//! two-level query: a CTE (`__sv_agg`) that aggregates the inner metric by ALL
//! queried dimensions, then an outer SELECT that applies the window function
//! over that CTE with a computed `PARTITION BY`. The partition is derived three
//! ways — `PARTITION BY EXCLUDING <dims>` (all queried dims MINUS the excluded
//! set), an explicit `PARTITION BY <dims>`, or no `PARTITION BY` at all (every
//! result row, as in Snowflake) — and that derivation, together with the
//! pre-aggregation grain, is the path's real correctness surface. The
//! example tests pin a handful of fixed shapes (three dims, one EXCLUDING set);
//! nothing exercised the partition math over random dim subsets with NULL keys.
//!
//! Shape: a table `s(id, c0, c1, c2, v, gk)` with three dimensions `d{i} =
//! s.c{i}`, tags `t(t_id, tag)` reached through the many-to-many bridge
//! `st(s_id, t_id)` with a fourth, `d3 = t.tag`, a parent `g(g_id, grp)` that
//! `s.gk` references with a fifth, `d4 = g.grp`, the inner metric `m =
//! SUM(s.v)` and the window metric `w = <FUNC>(m) OVER (…)`. The declared
//! partition config (EXCLUDING, explicit PARTITION or none, and which of `s`'s
//! and `g`'s dims), an optional `ORDER BY` on one of them (direction and NULLS
//! placement drawn),
//! the queried dim subset (always a superset of the required dims, the tag
//! among the extras), whether `m` is queried beside `w`, and the bridge rows
//! are randomized, as is `<FUNC>`.
//!
//! Phase 7 PR 6b: a window clause names the parent's dimension qualified,
//! `g.d4`, as Snowflake requires (fixture 10c, V1/V2/V4: a bare name is for
//! the metric's own table only), and `s`'s own dimensions bare or qualified
//! (`s.d0`), drawn per case.
//!
//! Phase 7 PR 5: Snowflake computes a window over the rows the rest of the
//! query gives, so with the tag queried or filtered each row of `s` counts
//! once per group, however many tags it has (fixture 4, W4-W6), and the inner
//! metric queried beside the window reads the same rows (W2). The oracle keeps
//! one row per (row of `s`, group) with a `DISTINCT` over the bridged join
//! before it sums.
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
//! (`SUM`/`COUNT`/`MIN`/`MAX`) so the multiset diff needs no float tolerance.
//! With an `ORDER BY`, the oracle's correlated subquery also keeps only the
//! rows sorting before the current one or equal to it (its peers), which is
//! the default frame; an explicit frame clause is left to the example tests.
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
    AccessModifier, Cardinality, Dimension, Join, Metric, NullsOrder, SemanticViewDefinition,
    SortOrder, TableRef, WindowOrderBy, WindowSpec,
};

/// The index of `d3 = t.tag`, the dimension across the `st` bridge. The window
/// spec never names it (Snowflake refuses a window clause naming a dimension
/// across a bridge at CREATE, 010211); a query groups or filters on it.
const TAG: usize = 3;

/// The index of `d4 = g.grp`, the dimension of `s`'s parent `g`. A window
/// clause may name it, qualified (Snowflake fixture 10c, V1/V2/V4).
const GRP: usize = 4;

/// The dimensions a window clause may name: `s`'s own and the parent's.
const WDIMS: [usize; 4] = [0, 1, 2, GRP];

/// Every dimension index: `s`'s, the bridged tag and the parent's.
const NALL: usize = 5;

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
            0..NALL,
            prop_oneof![Just("<"), Just("<="), Just("="), Just("<>"), Just(">="), Just(">")],
            -1i64..=3,
        ).prop_map(|(i, op, k)| Pred::Cmp(i, op, k)),
        2 => (0..NALL).prop_map(Pred::Filter),
    ];
    leaf.prop_recursive(3, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            inner.prop_map(|a| Pred::Not(Box::new(a))),
        ]
    })
}

/// Logical dimension name for index `i` (`d0`..`d3`).
fn dim_name(i: usize) -> String {
    format!("d{i}")
}

/// Physical column expression for dimension index `i` (`s.c0`/`s.c1`/`s.c2`,
/// and `t.tag` across the bridge).
fn dim_col(i: usize) -> String {
    match i {
        TAG => "t.tag".to_string(),
        GRP => "g.grp".to_string(),
        _ => format!("s.c{i}"),
    }
}

/// The table dimension index `i` is declared on.
fn dim_table(i: usize) -> &'static str {
    match i {
        TAG => "t",
        GRP => "g",
        _ => "s",
    }
}

/// How a window clause names dimension `i`: the parent's always qualified
/// (Snowflake refuses a bare name of another table's dimension, 000904), the
/// metric's own table's bare or qualified as `qualify` says.
fn window_ref(i: usize, qualify: bool) -> String {
    if i == GRP || qualify {
        format!("{}.{}", dim_table(i), dim_name(i))
    } else {
        dim_name(i)
    }
}

/// The oracle's FROM: every `s` row with its tags, NULL-extended for a row
/// with no tag, as a many-to-many bridge reads.
const BRIDGED_FROM: &str = "s LEFT JOIN st ON st.s_id = s.id LEFT JOIN t ON t.t_id = st.t_id \
                            LEFT JOIN g ON g.g_id = s.gk";

/// A generated instance: rows of `(c0, c1, c2, v, gk)`, whose `id` is the row's
/// index. `None` is a SQL NULL. Small dim domains force duplicate dim-tuples
/// (so partitions hold several rows) and NULL partition keys; the value domain
/// includes NULL so the inner `SUM` can itself be NULL for a group
/// (exercising `COUNT`/`MIN`/`MAX` NULL handling).
///
/// Phase 7 PR 5: `tags` are the three rows of `t` (`t_id` = index), and
/// `links` the `st` bridge rows `(s id, t_id)`, each pair once. A row of `s`
/// with two tags fans out across the bridge; one with none reads a NULL tag.
///
/// Phase 7 PR 6b: `grps` are the three rows of the parent `g` (`g_id` =
/// index), and each row's `gk` one of them or NULL (no parent: a NULL `d4`).
#[derive(Debug, Clone)]
struct Instance {
    rows: Vec<(
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    )>,
    tags: Vec<Option<i64>>,
    grps: Vec<Option<i64>>,
    links: Vec<(usize, usize)>,
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

/// How the metric declares its partition. Dim indices are from `WDIMS`.
#[derive(Debug, Clone)]
enum PartMode {
    /// `PARTITION BY EXCLUDING <dims>` — non-empty; effective partition is
    /// queried dims minus these (empty when it names every queried dim).
    Excluding(Vec<usize>),
    /// Explicit `PARTITION BY <dims>` — non-empty; effective partition is
    /// exactly these.
    Partition(Vec<usize>),
    /// No `PARTITION BY` at all: the window runs over every result row, as in
    /// Snowflake (2026-11-10 fixture 4, W2/W5). We used to partition this by
    /// every queried dimension, one row per partition.
    Whole,
}

/// A window `ORDER BY <dim> [ASC|DESC] NULLS {FIRST|LAST}`, with the default
/// frame: the rows of the partition up to the current row and its peers.
#[derive(Debug, Clone, Copy)]
struct Order {
    dim: usize,
    desc: bool,
    nulls_first: bool,
}

/// A full case: an instance, the window function, the declared partition config,
/// and the queried dim subset (always a superset of the config's required dims).
#[derive(Debug, Clone)]
struct Case {
    inst: Instance,
    func: WFunc,
    mode: PartMode,
    /// The window's `ORDER BY`, if any (its dim is always queried).
    order: Option<Order>,
    sel_dims: Vec<usize>,
    /// The pre-aggregation predicate (PBT-6). `None` one case in four, keeping
    /// the original no-predicate coverage rather than replacing it.
    where_pred: Option<Pred>,
    /// Whether the window's inner metric `m` is queried beside it (Phase 7
    /// PR 5, fixture 4 W2).
    with_inner: bool,
    /// Whether the window clause names `s`'s own dimensions qualified
    /// (`s.d0`) rather than bare (Phase 7 PR 6b).
    qualify: bool,
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
    let row = (dim_cell(), dim_cell(), dim_cell(), v_cell, dim_cell());
    let tags = prop::collection::vec(dim_cell(), 3);
    let grps = prop::collection::vec(dim_cell(), 3);
    (prop::collection::vec(row, 0..=20), tags, grps).prop_flat_map(|(rows, tags, grps)| {
        let n = rows.len();
        // Each (row, tag) pair at most once: a subset of every pair.
        let pairs: Vec<(usize, usize)> = (0..n).flat_map(|r| (0..3).map(move |t| (r, t))).collect();
        let len = pairs.len();
        (
            Just(rows),
            Just(tags),
            Just(grps),
            prop::sample::subsequence(pairs, 0..=len),
        )
            .prop_map(|(rows, tags, grps, links)| Instance {
                rows,
                tags,
                grps,
                links,
            })
    })
}

fn arb_func() -> impl Strategy<Value = WFunc> {
    prop_oneof![
        Just(WFunc::Sum),
        Just(WFunc::Count),
        Just(WFunc::Min),
        Just(WFunc::Max),
    ]
}

fn arb_order() -> impl Strategy<Value = Option<Order>> {
    prop_oneof![
        1 => Just(None),
        1 => (prop::sample::select(WDIMS.to_vec()), any::<bool>(), any::<bool>())
            .prop_map(|(dim, desc, nulls_first)| Some(Order { dim, desc, nulls_first })),
    ]
}

fn arb_case() -> impl Strategy<Value = Case> {
    // Pick the metric's declared partition config and ORDER BY first (the
    // required dims), then a query dim-set that is a superset of them — so
    // every generated query satisfies the required-dimension check and
    // `expand` accepts it. Mode 0 is EXCLUDING, 1 an explicit PARTITION BY,
    // 2 no PARTITION BY.
    (arb_instance(), arb_func(), 0u8..3, arb_order()).prop_flat_map(|(inst, func, mode, order)| {
        let all: Vec<usize> = WDIMS.to_vec();
        // EXCLUDING and PARTITION BY each name at least one dim.
        let req_size = if mode == 2 { 0..=0 } else { 1..=WDIMS.len() };
        let req = prop::sample::subsequence(all, req_size);
        (Just(inst), Just(func), Just(mode), Just(order), req).prop_flat_map(
            |(inst, func, mode, order, req)| {
                let mut required = req.clone();
                if let Some(o) = order {
                    if !required.contains(&o.dim) {
                        required.push(o.dim);
                    }
                }
                let rest: Vec<usize> = (0..NALL).filter(|d| !required.contains(d)).collect();
                let rest_len = rest.len();
                let extra = prop::sample::subsequence(rest, 0..=rest_len);
                let where_pred = prop_oneof![
                    1 => Just(None),
                    3 => arb_pred().prop_map(Some),
                ];
                (
                    Just(inst),
                    Just(func),
                    Just(mode),
                    Just(order),
                    Just(req),
                    Just(required),
                    extra,
                    where_pred,
                    any::<bool>(),
                    any::<bool>(),
                )
                    .prop_map(
                        |(
                            inst,
                            func,
                            mode,
                            order,
                            req,
                            required,
                            extra,
                            where_pred,
                            with_inner,
                            qualify,
                        )| {
                            let mut sel_dims: Vec<usize> =
                                required.into_iter().chain(extra).collect();
                            sel_dims.sort_unstable();
                            let mode = match mode {
                                0 => PartMode::Excluding(req),
                                1 => PartMode::Partition(req),
                                _ => PartMode::Whole,
                            };
                            Case {
                                inst,
                                func,
                                mode,
                                order,
                                sel_dims,
                                where_pred,
                                with_inner,
                                qualify,
                            }
                        },
                    )
            },
        )
    })
}

/// A table `name` with primary key `pk`.
fn table(name: &str, pk: &[&str]) -> TableRef {
    TableRef {
        alias: name.to_string(),
        table: name.to_string(),
        definition: None,
        pk_columns: pk.iter().map(|c| (*c).to_string()).collect(),
        unique_constraints: vec![],
        comment: None,
        synonyms: vec![],
        tags: Vec::new(),
        distinct_ranges: vec![],
    }
}

/// `st(<fk>) REFERENCES <to>(<pk>)`.
fn bridge_rel(name: &str, fk: &str, to: &str, pk: &str) -> Join {
    Join {
        table: to.to_string(),
        from_alias: "st".to_string(),
        fk_columns: vec![fk.to_string()],
        ref_columns: vec![pk.to_string()],
        name: Some(name.to_string()),
        cardinality: Cardinality::ManyToOne,
        ..Default::default()
    }
}

/// Build the semantic-view definition: table `s` with three dimensions, the
/// tags `t` across the `st` bridge with a fourth, the inner metric `m =
/// SUM(s.v)`, and the window metric `w` over `m` with the given function,
/// partition config and `ORDER BY`.
fn build_def(
    func: WFunc,
    mode: &PartMode,
    order: Option<Order>,
    qualify: bool,
) -> SemanticViewDefinition {
    let tables = vec![
        table("s", &["id"]),
        table("st", &["s_id", "t_id"]),
        table("t", &["t_id"]),
        table("g", &["g_id"]),
    ];
    let dimensions = (0..NALL)
        .map(|i| Dimension {
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
            name: dim_name(i),
            expr: dim_col(i),
            source_table: Some(dim_table(i).to_string()),
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
        .chain((0..NALL).map(|i| Dimension {
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
            name: format!("f{i}"),
            expr: filter_expr(i),
            source_table: Some(dim_table(i).to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: true,
            tags: Vec::new(),
        }))
        .collect();
    let (excluding_dims, partition_dims) = match mode {
        PartMode::Excluding(d) => (d.iter().map(|&i| window_ref(i, qualify)).collect(), vec![]),
        PartMode::Partition(d) => (vec![], d.iter().map(|&i| window_ref(i, qualify)).collect()),
        PartMode::Whole => (vec![], vec![]),
    };
    let order_by = order
        .map(|o| {
            vec![WindowOrderBy {
                expr: window_ref(o.dim, qualify),
                order: if o.desc {
                    SortOrder::Desc
                } else {
                    SortOrder::Asc
                },
                nulls: if o.nulls_first {
                    NullsOrder::First
                } else {
                    NullsOrder::Last
                },
            }]
        })
        .unwrap_or_default();
    let spec = WindowSpec {
        window_function: func.name().to_string(),
        inner_metric: "m".to_string(),
        extra_args: vec![],
        excluding_dims,
        partition_dims,
        order_by,
        frame_clause: None,
    };
    let metrics = vec![
        Metric {
            name: "m".to_string(),
            expr: "SUM(s.v)".to_string(),
            source_table: Some("s".to_string()),
            output_type: None,
            using_relationships: vec![],
            comment: None,
            synonyms: vec![],
            access: AccessModifier::Public,
            non_additive_by: vec![],
            window_spec: None,
            tags: Vec::new(),
        },
        Metric {
            name: "w".to_string(),
            expr: format!("{}(m) OVER (...)", func.name()),
            source_table: Some("s".to_string()),
            output_type: None,
            using_relationships: vec![],
            comment: None,
            synonyms: vec![],
            access: AccessModifier::Public,
            non_additive_by: vec![],
            window_spec: Some(spec),
            tags: Vec::new(),
        },
    ];
    SemanticViewDefinition {
        variables: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
        tables,
        dimensions,
        metrics,
        joins: vec![
            bridge_rel("st_s", "s_id", "s", "id"),
            bridge_rel("st_t", "t_id", "t", "t_id"),
            Join {
                table: "g".to_string(),
                from_alias: "s".to_string(),
                fk_columns: vec!["gk".to_string()],
                ref_columns: vec!["g_id".to_string()],
                name: Some("s_g".to_string()),
                cardinality: Cardinality::ManyToOne,
                ..Default::default()
            },
        ],
        // PIN: a window metric is an aggregate shape; FACTS returns unaggregated rows. TECH-DEBT #66
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        table_columns: std::collections::BTreeMap::new(),
        comment: None,
        tags: Vec::new(),
    }
}

/// Create the physical table and insert the generated rows.
fn make_db(inst: &Instance) -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE s (id INTEGER, c0 INTEGER, c1 INTEGER, c2 INTEGER, v INTEGER, gk INTEGER);
         CREATE TABLE st (s_id INTEGER, t_id INTEGER);
         CREATE TABLE t (t_id INTEGER, tag INTEGER);
         CREATE TABLE g (g_id INTEGER, grp INTEGER);",
    )
    .expect("create tables");
    let cell = |c: &Option<i64>| c.map_or_else(|| "NULL".to_string(), |v| v.to_string());
    if !inst.rows.is_empty() {
        let values: Vec<String> = inst
            .rows
            .iter()
            .enumerate()
            .map(|(id, (a, b, c, d, gk))| {
                format!(
                    "({id},{},{},{},{},{})",
                    cell(a),
                    cell(b),
                    cell(c),
                    cell(d),
                    cell(gk)
                )
            })
            .collect();
        conn.execute_batch(&format!("INSERT INTO s VALUES {};", values.join(",")))
            .expect("insert rows");
    }
    let tags: Vec<String> = inst
        .tags
        .iter()
        .enumerate()
        .map(|(id, tag)| format!("({id},{})", cell(tag)))
        .collect();
    conn.execute_batch(&format!("INSERT INTO t VALUES {};", tags.join(",")))
        .expect("insert tags");
    let grps: Vec<String> = inst
        .grps
        .iter()
        .enumerate()
        .map(|(id, grp)| format!("({id},{})", cell(grp)))
        .collect();
    conn.execute_batch(&format!("INSERT INTO g VALUES {};", grps.join(",")))
        .expect("insert parents");
    if !inst.links.is_empty() {
        let links: Vec<String> = inst
            .links
            .iter()
            .map(|(s_id, t_id)| format!("({s_id},{t_id})"))
            .collect();
        conn.execute_batch(&format!("INSERT INTO st VALUES {};", links.join(",")))
            .expect("insert links");
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
        PartMode::Whole => Vec::new(),
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

    // The pre-aggregation CTE: SUM(s.v) grouped by all queried dims, each row
    // of `s` once per group however many tags it has (Snowflake, fixture 4
    // W4-W6): the DISTINCT keeps one row per (s row, group) of the bridged
    // join before the SUM, a formulation independent of the extension's.
    let rows_sql = std::iter::once("s.id AS __id, s.v AS __v".to_string())
        .chain(
            sel.iter()
                .map(|&i| format!("{} AS {}", dim_col(i), dim_name(i))),
        )
        .collect::<Vec<_>>()
        .join(", ");
    let rows = format!("SELECT DISTINCT {rows_sql} FROM {BRIDGED_FROM}{where_sql}");
    let agg = if sel.is_empty() {
        format!("SELECT SUM(__v) AS w FROM ({rows}) r")
    } else {
        let agg_select = sel
            .iter()
            .map(|&i| dim_name(i))
            .chain(std::iter::once("SUM(__v) AS w".to_string()))
            .collect::<Vec<_>>()
            .join(", ");
        let group_by = (1..=sel.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("SELECT {agg_select} FROM ({rows}) r GROUP BY {group_by}")
    };

    let part = partition_dims(case);
    let func = case.func.name();
    let mut preds: Vec<String> = part
        .iter()
        .map(|&i| {
            let n = dim_name(i);
            format!("a2.{n} IS NOT DISTINCT FROM a1.{n}")
        })
        .collect();
    // With an `ORDER BY`, the default frame holds the partition's rows up to
    // the current one and its peers: those whose key sorts before it or equal
    // to it, a NULL key sorting at the end its `NULLS` says.
    if let Some(o) = case.order {
        let n = dim_name(o.dim);
        let cmp = if o.desc { ">=" } else { "<=" };
        preds.push(if o.nulls_first {
            format!("(a2.{n} IS NULL OR (a1.{n} IS NOT NULL AND a2.{n} {cmp} a1.{n}))")
        } else {
            format!("(a1.{n} IS NULL OR (a2.{n} IS NOT NULL AND a2.{n} {cmp} a1.{n}))")
        });
    }
    let corr_where = if preds.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", preds.join(" AND "))
    };
    let window_col = format!("(SELECT {func}(a2.w) FROM agg a2{corr_where}) AS w");
    let outer_select = sel
        .iter()
        .map(|&i| format!("a1.{}", dim_name(i)))
        .chain(case.with_inner.then(|| "a1.w AS m".to_string()))
        .chain(std::iter::once(window_col))
        .collect::<Vec<_>>()
        .join(", ");

    format!("WITH agg AS ({agg}) SELECT {outer_select} FROM agg a1")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 192, ..ProptestConfig::default() })]

    #[test]
    fn window_metric_matches_independent_oracle(case in arb_case()) {
        let def = build_def(case.func, &case.mode, case.order, case.qualify);
        // Every generated window is one CREATE accepts (Phase 7 PR 6b), so the
        // oracle covers the shapes a user can declare.
        if let Err(e) = semantic_views::graph::validate_window_dimensions(&def) {
            prop_assert!(false, "CREATE would refuse the generated window: {e}\ncase: {case:?}");
        }
        let req = QueryRequest {
            where_clause: case.where_pred.as_ref().map(Pred::to_member_sql),
            dimensions: case.sel_dims.iter().map(|&i| DimensionName::new(dim_name(i))).collect(),
            // Always query the window metric so the window path fires, and its
            // inner metric beside it in half the cases.
            metrics: if case.with_inner {
                vec![MetricName::new("m"), MetricName::new("w")]
            } else {
                vec![MetricName::new("w")]
            },
            // PIN: a window metric is an aggregate shape; FACTS returns unaggregated rows. TECH-DEBT #66
            facts: vec![],
        };

        // Every generated query (whose dim-set is a superset of the required
        // dims) is accepted: a tag across the bridge counts each row of `s`
        // once per group.
        let expanded = match expand("win", &def, &req) {
            Ok(sql) => sql,
            Err(e) => {
                prop_assert!(false, "window query unexpectedly rejected: {e}\ncase: {case:?}");
                unreachable!()
            }
        };
        let oracle = oracle_sql(&case);

        // Canonical projection (columns sorted by name) so a column-order
        // difference between the two formulations is not a false diff.
        let mut proj_cols: Vec<String> = case.sel_dims.iter().map(|&i| dim_name(i)).collect();
        proj_cols.push("w".to_string());
        if case.with_inner {
            proj_cols.push("m".to_string());
        }
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
             (symmetric multiset diff = {}); func={:?} mode={:?} order={:?} dims={:?} \
             with_inner={}\n--- expanded:\n{}\n--- oracle:\n{}",
            diff, case.func, case.mode, case.order, case.sel_dims, case.with_inner, expanded, oracle
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
    // Phase 7: a window with no PARTITION BY over several result rows (also
    // with an ORDER BY), and an ordered window whose order key is shared by
    // rows of one partition (peers), so neither the whole-result rule nor the
    // peer frame is vacuous. Counted over the rows the predicate keeps.
    let (mut whole_multi_row, mut ordered, mut ordered_with_peers) = (0usize, 0usize, 0usize);
    let mut whole_ordered = 0usize;
    // Phase 7 PR 5: the bridge and the inner metric beside the window.
    let (mut tag_queried, mut tag_filtered, mut fanned, mut with_inner) = (0, 0, 0, 0);
    // Phase 7 PR 6b: the parent's dimension named (qualified) in the window
    // clause, and the metric's own dimensions named qualified.
    let (mut grp_in_window, mut own_qualified) = (0usize, 0usize);

    for _ in 0..400 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        let part = partition_dims(&case);
        let mut pred_dims = Vec::new();
        if let Some(p) = &case.where_pred {
            p.dims(&mut pred_dims);
        }
        let reads_tag = case.sel_dims.contains(&TAG) || pred_dims.contains(&TAG);
        tag_queried += usize::from(case.sel_dims.contains(&TAG));
        tag_filtered += usize::from(pred_dims.contains(&TAG));
        with_inner += usize::from(case.with_inner);
        let window_dims: Vec<usize> = match &case.mode {
            PartMode::Excluding(d) | PartMode::Partition(d) => d.clone(),
            PartMode::Whole => Vec::new(),
        }
        .into_iter()
        .chain(case.order.map(|o| o.dim))
        .collect();
        grp_in_window += usize::from(window_dims.contains(&GRP));
        own_qualified += usize::from(case.qualify && window_dims.iter().any(|&d| d != GRP));
        // A row of `s` with two tags, in a query that reads the tag: the
        // fan-out the once-per-group rule must undo.
        let rows_tags = |r: usize| {
            case.inst
                .links
                .iter()
                .filter(|(s_id, _)| *s_id == r)
                .count()
        };
        if reads_tag && (0..case.inst.rows.len()).any(|r| rows_tags(r) > 1) {
            fanned += 1;
        }
        // The result's groups: the queried dims' distinct values over the rows
        // the predicate keeps, read from DuckDB so NULLs filter as in SQL.
        let groups: HashSet<Vec<Option<i64>>> = {
            let conn = make_db(&case.inst);
            let cols = if case.sel_dims.is_empty() {
                "1".to_string()
            } else {
                case.sel_dims
                    .iter()
                    .map(|&d| dim_col(d))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let where_sql = case
                .where_pred
                .as_ref()
                .map_or_else(String::new, |p| format!(" WHERE {}", p.to_raw_sql()));
            let n = case.sel_dims.len();
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT DISTINCT {cols} FROM {BRIDGED_FROM}{where_sql}"
                ))
                .expect("group query binds");
            stmt.query_map([], |r| (0..n).map(|i| r.get::<_, Option<i64>>(i)).collect())
                .expect("group query runs")
                .map(Result::unwrap)
                .collect()
        };
        if matches!(case.mode, PartMode::Whole) && groups.len() > 1 {
            whole_multi_row += 1;
            if case.order.is_some() {
                // W2/W5's cell: an ordered window over the whole result.
                whole_ordered += 1;
            }
        }
        if let Some(o) = case.order {
            ordered += 1;
            // Two groups in one partition with the same order key.
            let key = |g: &Vec<Option<i64>>, dims: &[usize]| -> Vec<Option<i64>> {
                dims.iter()
                    .map(|d| g[case.sel_dims.iter().position(|s| s == d).expect("queried")])
                    .collect()
            };
            let mut seen: HashSet<(Vec<Option<i64>>, Option<i64>)> = HashSet::new();
            if groups
                .iter()
                .any(|g| !seen.insert((key(g, &part), key(g, &[o.dim])[0])))
            {
                ordered_with_peers += 1;
            }
        }
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
        whole_multi_row >= 20,
        "too few cases queried a window with no PARTITION BY over more than one row \
         ({whole_multi_row})"
    );
    assert!(
        whole_ordered >= 20,
        "only {whole_ordered}/400 cases ordered a window with no PARTITION BY over more than one row"
    );
    assert!(
        ordered > 0 && ordered_with_peers > 0,
        "ordered windows: {ordered}, with peers in a partition: {ordered_with_peers}"
    );
    assert!(
        tag_queried >= 100 && tag_filtered >= 50 && fanned >= 100,
        "the bridge is barely reached: tag queried {tag_queried}, filtered {tag_filtered}, \
         a row fanned across it {fanned} (of 400)"
    );
    assert!(
        grp_in_window >= 100 && own_qualified >= 50,
        "window clauses name the parent's dimension in {grp_in_window}/400 cases and the \
         metric's own dimensions qualified in {own_qualified}/400"
    );
    assert!(
        (100..=300).contains(&with_inner),
        "the inner metric beside the window in {with_inner}/400 cases"
    );
    assert!(
        distinct.len() > 50,
        "generator produced only {} distinct predicates; search space collapsed",
        distinct.len()
    );
}
