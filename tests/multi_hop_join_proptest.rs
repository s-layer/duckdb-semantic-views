//! Differential proptest for **multi-hop** join trees (code-review 2026-07-18
//! PBT-1 follow-up — the multi-hop facet of "no randomized coverage of the
//! hardest expansion semantics").
//!
//! The two-table [`star_schema_proptest`] exercises a single `ManyToOne` hop.
//! Nothing exercised a chain of hops — where the join resolver must pull in an
//! **intermediate** table to reach a far dimension, topologically order two
//! joins from the root, and the fan-trap fence must reject a metric on a table
//! **two hops** above the root (not just the immediate parent).
//!
//! Shape: a `ManyToOne` **chain** rooted at the base/"many"-most table.
//! `t.fk_u REFERENCES u.id` and `u.fk_w REFERENCES w.id`, so the join tree is
//! `t → u → w` (t at the root grain, u its parent, w its grandparent). `u.id`
//! and `w.id` are generated distinct so the declared PKs hold and the chained
//! LEFT JOINs never fan `t`. Foreign keys at both levels include NULL and
//! dangling ids; group keys and values include NULLs.
//!
//! Two invariants are checked per case:
//!
//! 1. **Dimension below a metric's grain ⇒ rejected.** `SUM(u.uw)` grouped by
//!    `t.d`, or `SUM(w.ww)` grouped by `u.ucat` / `t.d`, asks for an ancestor's
//!    aggregate at a grain below it: those rows genuinely fan across the
//!    descendant's values, at one hop or two. `expand` MUST reject it with a
//!    fan-trap-family error — neither the root-anchored path (which silently
//!    inflated it, EXP-1) nor per-grain aggregation can define it.
//! 2. **Accepted query ⇒ numerically correct.** For every query `expand`
//!    accepts, the result must equal an independently hand-written oracle,
//!    compared as a multiset inside DuckDB via a symmetric `EXCEPT ALL`
//!    difference (the same comparator the single-table and star harnesses use).
//!    Since v0.12.0 that set includes the **ancestor metrics** `SUM(u.uw)` /
//!    `SUM(w.ww)`, alone or mixed with root-grain metrics: each is computed at
//!    its own grain (`FROM u` / `FROM w`, never through the descendant join) and
//!    the grains are joined back together, so the oracle is a per-grain one
//!    spanning up to three grains at once. Selecting a grandparent dimension
//!    (`wcat`) without the parent dimension still forces the resolver to include
//!    the intermediate `u` to reach `w`; a multi-hop resolution bug (dropped
//!    intermediate, wrong join order, wrong ON columns) surfaces as invalid SQL
//!    or a non-zero diff.

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, MetricName, QueryRequest};
use semantic_views::model::{
    AccessModifier, Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef,
};

/// A generated chain instance. Grandparent rows `w` have ids `0..n_w`; parent
/// rows `u` have ids `0..n_u` and a foreign key `fk_w` into `w`; child rows `t`
/// carry a foreign key `fk_u` into `u`. `None` is a SQL NULL throughout.
#[derive(Debug, Clone)]
struct Instance {
    /// Grandparent rows: `(wcat, ww)` for ids `0..n_w`.
    w_rows: Vec<(Option<i64>, Option<i64>)>,
    /// Parent rows: `(fk_w, ucat, uw)` for ids `0..n_u`.
    u_rows: Vec<(Option<i64>, Option<i64>, Option<i64>)>,
    /// Child rows: `(fk_u, d, v)`.
    t_rows: Vec<(Option<i64>, Option<i64>, Option<i64>)>,
}

/// Queryable objects, by stable name. `td`/`ucat`/`wcat` are dimensions at the
/// three grains; `sv`/`ct` are root-grain metrics; `su`/`sw` are the
/// parent/grandparent (ancestor) metrics, each computed at its own grain.
const DIMS: [&str; 3] = ["td", "ucat", "wcat"];
const METS: [&str; 4] = ["sv", "ct", "su", "sw"];

/// Members a generated `where_clause` may name (PBT-6), one per grain plus a
/// `LABELS = (FILTER)` member per grain whose expression is a compound `OR`.
/// `raw` is the SQL the member stands for; `table` is the grain it lives on,
/// which is what decides whether a predicate fans a co-queried metric.
const WMEMBERS: [(&str, &str, &str); 6] = [
    ("td", "t.d", "t"),
    ("ucat", "u.ucat", "u"),
    ("wcat", "w.wcat", "w"),
    ("ftd", "(t.d = 0 OR t.d = 2)", "t"),
    ("fucat", "(u.ucat = 0 OR u.ucat = 2)", "u"),
    ("fwcat", "(w.wcat = 0 OR w.wcat = 2)", "w"),
];

/// Indices into `WMEMBERS` that are plain columns (can carry a comparison) as
/// opposed to self-contained boolean filter members. "Plain" is about the
/// member's *expression*; it says nothing about the bare-vs-qualified spelling
/// the predicate names it by, which is an independent axis (see [`member_ref`]).
const COMPARABLE: [usize; 3] = [0, 1, 2];
/// Indices into `WMEMBERS` that are filter members.
const FILTERS: [usize; 3] = [3, 4, 5];

/// A generated pre-aggregation predicate over the chain's members (PBT-6).
#[derive(Debug, Clone)]
enum Pred {
    /// `<member> <op> <literal>` for a comparable member (index into `WMEMBERS`).
    /// The `bool` is EXP-14's surface: when true the member is named by its
    /// `<table>.<name>` qualified form instead of bare.
    Cmp(usize, &'static str, i64, bool),
    /// A filter member — the substitution/precedence surface. The `bool` is the
    /// same qualified/bare choice as `Cmp`.
    Filter(usize, bool),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
}

/// Whether this predicate names any member qualified, and whether any bare.
/// Used by the anti-vacuity guard to prove the generator reaches both spellings.
fn reference_spellings_of(p: &Pred, qualified: &mut bool, bare: &mut bool) {
    match p {
        Pred::Cmp(_, _, _, q) | Pred::Filter(_, q) => {
            if *q {
                *qualified = true;
            } else {
                *bare = true;
            }
        }
        Pred::And(a, b) | Pred::Or(a, b) => {
            reference_spellings_of(a, qualified, bare);
            reference_spellings_of(b, qualified, bare);
        }
        Pred::Not(a) => reference_spellings_of(a, qualified, bare),
    }
}

/// How a generated predicate names member `m`: bare (`ftd`) or qualified by the
/// member's own table (`t.ftd`).
///
/// EXP-14: the qualified spelling was keyed nowhere, so it matched no member and
/// survived into the emitted SQL as a raw column — never substituted, and never
/// metric-checked. Both spellings denote the same member, so both must produce
/// the same number as the oracle; that is what varying this flag tests.
fn member_ref(m: usize, qualified: bool) -> String {
    let (name, _, table) = WMEMBERS[m];
    if qualified {
        format!("{table}.{name}")
    } else {
        name.to_string()
    }
}

impl Pred {
    /// The `where_clause` text: member NAMES, composites parenthesized so the
    /// SQL parse matches this AST.
    ///
    /// Each reference is spelled bare (`ftd`) or qualified by its own table
    /// (`t.ftd`) according to the generated flag — see [`member_ref`]. A filter
    /// member is still emitted as a name rather than expanded, which is the
    /// substitution/precedence surface; EXP-14 added the spelling axis on top.
    fn to_member_sql(&self) -> String {
        match self {
            Pred::Cmp(m, op, k, q) => format!("{} {op} {k}", member_ref(*m, *q)),
            Pred::Filter(m, q) => member_ref(*m, *q),
            Pred::And(a, b) => format!("({} AND {})", a.to_member_sql(), b.to_member_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_member_sql(), b.to_member_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_member_sql()),
        }
    }

    /// `(any_qualified, any_bare)` across this predicate's member references.
    fn reference_spellings(&self) -> (bool, bool) {
        let (mut q, mut b) = (false, false);
        reference_spellings_of(self, &mut q, &mut b);
        (q, b)
    }

    /// The oracle's independent rendering: raw columns, filter members expanded,
    /// every operand explicitly parenthesized.
    fn to_raw_sql(&self) -> String {
        match self {
            Pred::Cmp(m, op, k, _) => format!("({} {op} {k})", WMEMBERS[*m].1),
            Pred::Filter(m, _) => format!("({})", WMEMBERS[*m].1),
            Pred::And(a, b) => format!("({} AND {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_raw_sql()),
        }
    }

    /// The chain tables this predicate names — the grains that must be joined
    /// into whichever CTE evaluates it.
    fn tables(&self, out: &mut Vec<&'static str>) {
        match self {
            Pred::Cmp(m, _, _, _) | Pred::Filter(m, _) => out.push(WMEMBERS[*m].2),
            Pred::And(a, b) | Pred::Or(a, b) => {
                a.tables(out);
                b.tables(out);
            }
            Pred::Not(a) => a.tables(out),
        }
    }

    /// Whether any named member is a filter member (generator-coverage guard).
    fn references_filter(&self) -> bool {
        match self {
            Pred::Cmp(..) => false,
            Pred::Filter(..) => true,
            Pred::And(a, b) | Pred::Or(a, b) => a.references_filter() || b.references_filter(),
            Pred::Not(a) => a.references_filter(),
        }
    }
}

fn arb_pred() -> impl Strategy<Value = Pred> {
    let comparable = prop::sample::select(COMPARABLE.to_vec());
    let filters = prop::sample::select(FILTERS.to_vec());
    let leaf = prop_oneof![
        3 => (
            comparable,
            prop_oneof![Just("<"), Just("<="), Just("="), Just("<>"), Just(">="), Just(">")],
            -1i64..=3,
            prop::bool::ANY,
        ).prop_map(|(m, op, k, q)| Pred::Cmp(m, op, k, q)),
        2 => (filters, prop::bool::ANY).prop_map(|(m, q)| Pred::Filter(m, q)),
    ];
    leaf.prop_recursive(3, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            inner.prop_map(|a| Pred::Not(Box::new(a))),
        ]
    })
}

/// A full case: an instance plus the non-empty subset of dims + metrics to query,
/// and the optional pre-aggregation predicate (PBT-6).
#[derive(Debug, Clone)]
struct Case {
    inst: Instance,
    sel_dims: Vec<usize>,
    sel_metrics: Vec<usize>,
    where_pred: Option<Pred>,
}

fn arb_instance() -> impl Strategy<Value = Instance> {
    let val_cell = prop_oneof![
        1 => Just(None),
        3 => (-5i64..=5).prop_map(Some),
    ];
    let cat_cell = prop_oneof![
        1 => Just(None),
        4 => (0i64..3).prop_map(Some),
    ];
    // Grandparent and parent counts kept small so parent/child fan-in is common.
    (1usize..=3, 1usize..=4).prop_flat_map(move |(n_w, n_u)| {
        let w_row = (cat_cell.clone(), val_cell.clone());
        let w_rows = prop::collection::vec(w_row, n_w);
        // u.fk_w: NULL, a valid w id (0..n_w), or a dangling id (n_w).
        let fk_w_cell = prop_oneof![
            1 => Just(None),
            4 => (0i64..n_w as i64).prop_map(Some),
            1 => Just(Some(n_w as i64)),
        ];
        let u_row = (fk_w_cell, cat_cell.clone(), val_cell.clone());
        let u_rows = prop::collection::vec(u_row, n_u);
        // t.fk_u: NULL, a valid u id (0..n_u), or a dangling id (n_u).
        let fk_u_cell = prop_oneof![
            1 => Just(None),
            4 => (0i64..n_u as i64).prop_map(Some),
            1 => Just(Some(n_u as i64)),
        ];
        let t_row = (fk_u_cell, cat_cell.clone(), val_cell.clone());
        let t_rows = prop::collection::vec(t_row, 0..=20);
        (w_rows, u_rows, t_rows).prop_map(|(w_rows, u_rows, t_rows)| Instance {
            w_rows,
            u_rows,
            t_rows,
        })
    })
}

fn arb_case() -> impl Strategy<Value = Case> {
    arb_instance().prop_flat_map(|inst| {
        let dim_sel =
            prop::sample::subsequence((0..DIMS.len()).collect::<Vec<_>>(), 0..=DIMS.len());
        let met_sel =
            prop::sample::subsequence((0..METS.len()).collect::<Vec<_>>(), 0..=METS.len());
        let where_pred = prop_oneof![
            1 => Just(None),
            3 => arb_pred().prop_map(Some),
        ];
        (Just(inst), dim_sel, met_sel, where_pred)
            .prop_filter(
                "at least one of dimensions/metrics must be selected",
                |(_, sel_dims, sel_metrics, _)| !sel_dims.is_empty() || !sel_metrics.is_empty(),
            )
            .prop_map(|(inst, sel_dims, sel_metrics, where_pred)| Case {
                inst,
                sel_dims,
                sel_metrics,
                where_pred,
            })
    })
}

/// Build the semantic-view definition: a `ManyToOne` chain `t → u → w`, a
/// dimension at each grain, root-grain safe metrics (`sum(t.v)`, `count(*)`),
/// and ancestor metrics on `u` and `w`.
fn build_def() -> SemanticViewDefinition {
    let table = |alias: &str, pk: &[&str]| TableRef {
        alias: alias.to_string(),
        table: alias.to_string(),
        pk_columns: pk.iter().map(|s| (*s).to_string()).collect(),
        unique_constraints: vec![],
        comment: None,
        synonyms: vec![],
    };
    // `t` is listed first: base_table() == the first declared table, and the
    // FROM is anchored there with LEFT JOINs outward along the chain.
    let tables = vec![table("t", &[]), table("u", &["id"]), table("w", &["id"])];
    let dim = |name: &str, expr: &str, source: &str| Dimension {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(source.to_string()),
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: false,
    };
    // PBT-6: the three filter members (`LABELS = (FILTER)`), one per grain.
    // Never selected as output — they exist to be named by a generated
    // `where_clause`, and their compound expressions make the member splice's
    // parenthesization load-bearing.
    let filter_dim = |name: &str, expr: &str, source: &str| Dimension {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(source.to_string()),
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: true,
    };
    let dimensions = vec![
        dim("td", "t.d", "t"),
        dim("ucat", "u.ucat", "u"),
        dim("wcat", "w.wcat", "w"),
        filter_dim("ftd", "t.d = 0 OR t.d = 2", "t"),
        filter_dim("fucat", "u.ucat = 0 OR u.ucat = 2", "u"),
        filter_dim("fwcat", "w.wcat = 0 OR w.wcat = 2", "w"),
    ];
    let base_metric = |name: &str, expr: &str, source: Option<&str>| Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: source.map(str::to_string),
        output_type: None,
        using_relationships: vec![],
        comment: None,
        synonyms: vec![],
        access: AccessModifier::Public,
        non_additive_by: vec![],
        window_spec: None,
    };
    let metrics = vec![
        base_metric("sv", "sum(t.v)", Some("t")),
        base_metric("ct", "count(*)", None),
        base_metric("su", "sum(u.uw)", Some("u")),
        base_metric("sw", "sum(w.ww)", Some("w")),
    ];
    let joins = vec![
        Join {
            from_alias: "t".to_string(),
            table: "u".to_string(),
            fk_columns: vec!["fk_u".to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some("t_u".to_string()),
            cardinality: Cardinality::ManyToOne,
        },
        Join {
            from_alias: "u".to_string(),
            table: "w".to_string(),
            fk_columns: vec!["fk_w".to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some("u_w".to_string()),
            cardinality: Cardinality::ManyToOne,
        },
    ];
    SemanticViewDefinition {
        tables,
        dimensions,
        metrics,
        joins,
        // PIN: FACTS is a row-level shape with no per-grain meaning; this harness oracles the three-table per-grain chain. TECH-DEBT #66
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
    }
}

/// Create the physical tables and insert the generated rows.
fn make_db(inst: &Instance) -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE w (id INTEGER, wcat INTEGER, ww INTEGER); \
         CREATE TABLE u (id INTEGER, fk_w INTEGER, ucat INTEGER, uw INTEGER); \
         CREATE TABLE t (fk_u INTEGER, d INTEGER, v INTEGER);",
    )
    .expect("create tables");

    let cell = |c: &Option<i64>| c.map_or_else(|| "NULL".to_string(), |v| v.to_string());

    if !inst.w_rows.is_empty() {
        let values: Vec<String> = inst
            .w_rows
            .iter()
            .enumerate()
            .map(|(i, (wcat, ww))| format!("({i},{},{})", cell(wcat), cell(ww)))
            .collect();
        conn.execute_batch(&format!("INSERT INTO w VALUES {};", values.join(",")))
            .expect("insert w rows");
    }
    if !inst.u_rows.is_empty() {
        let values: Vec<String> = inst
            .u_rows
            .iter()
            .enumerate()
            .map(|(i, (fk_w, ucat, uw))| {
                format!("({i},{},{},{})", cell(fk_w), cell(ucat), cell(uw))
            })
            .collect();
        conn.execute_batch(&format!("INSERT INTO u VALUES {};", values.join(",")))
            .expect("insert u rows");
    }
    if !inst.t_rows.is_empty() {
        let values: Vec<String> = inst
            .t_rows
            .iter()
            .map(|(fk_u, d, v)| format!("({},{},{})", cell(fk_u), cell(d), cell(v)))
            .collect();
        conn.execute_batch(&format!("INSERT INTO t VALUES {};", values.join(",")))
            .expect("insert t rows");
    }
    conn
}

/// Independent oracle SQL for a query `expand` should accept (only root-grain
/// metrics). The FROM is always the full chain `t LEFT JOIN u LEFT JOIN w`:
/// because `u.id`/`w.id` are unique, joining the parents never changes `t`'s
/// multiset for `count(*)`/`sum(t.v)`, and grouping by an ancestor dimension is
/// a plain group key. Metrics-only ⇒ global aggregate (no GROUP BY); anything
/// with dimensions ⇒ GROUP BY the projected dimension ordinals (multiset-equal
/// to the expansion's SELECT DISTINCT for the dims-only case).
/// The chain position of a table: `t` (the base/"many"-most) is 0, its parent
/// `u` is 1, its grandparent `w` is 2. A grain can be grouped by a dimension at
/// its own level or ABOVE it (each hop up is many-to-one, so the join adds no
/// rows); a dimension BELOW it fans it, which is the one shape that stays
/// rejected.
fn level(table: &str) -> usize {
    match table {
        "t" => 0,
        "u" => 1,
        "w" => 2,
        other => unreachable!("unexpected table {other}"),
    }
}

/// The table each dimension lives on.
fn dim_table(dim: &str) -> &'static str {
    match dim {
        "td" => "t",
        "ucat" => "u",
        "wcat" => "w",
        other => unreachable!("unexpected dim {other}"),
    }
}

/// The table each metric aggregates — its **grain**. `ct` is `count(*)` with no
/// declared source table, which sits at the base grain.
fn metric_table(metric: &str) -> &'static str {
    match metric {
        "sv" | "ct" => "t",
        "su" => "u",
        "sw" => "w",
        other => unreachable!("unexpected metric {other}"),
    }
}

/// One grain's half of the oracle: the metrics that live on `anchor`, computed
/// over `anchor`'s own rows and grouped by the selected dimensions, which are
/// reached by chaining LEFT JOINs **upward** from the anchor.
///
/// This is the independent statement of "at its own grain": `su` is summed over
/// `u` itself, never over `t LEFT JOIN u` (where each parent row appears once
/// per child row and childless parents vanish).
fn grain_sql(case: &Case, anchor: &str) -> String {
    let dim_items: Vec<String> = case
        .sel_dims
        .iter()
        .map(|&i| match DIMS[i] {
            "td" => "t.d AS td".to_string(),
            "ucat" => "u.ucat AS ucat".to_string(),
            "wcat" => "w.wcat AS wcat".to_string(),
            other => unreachable!("unexpected dim {other}"),
        })
        .collect();
    let met_items: Vec<String> = case
        .sel_metrics
        .iter()
        .filter(|&&i| metric_table(METS[i]) == anchor)
        .map(|&i| match METS[i] {
            "sv" => "sum(t.v) AS sv".to_string(),
            "ct" => "count(*) AS ct".to_string(),
            "su" => "sum(u.uw) AS su".to_string(),
            "sw" => "sum(w.ww) AS sw".to_string(),
            other => unreachable!("unexpected metric {other}"),
        })
        .collect();
    let select_items = dim_items
        .iter()
        .chain(met_items.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    // The up-chain from the anchor. Joining the whole remaining chain is
    // harmless — every hop is many-to-one, so none of them changes the anchor's
    // row multiset.
    let from = match anchor {
        "t" => "FROM t LEFT JOIN u ON t.fk_u = u.id LEFT JOIN w ON u.fk_w = w.id",
        "u" => "FROM u LEFT JOIN w ON u.fk_w = w.id",
        "w" => "FROM w",
        other => unreachable!("unexpected anchor {other}"),
    };
    // PBT-6: the predicate is evaluated inside EVERY grain's CTE, filtering the
    // rows that feed that grain's aggregate. Predicate members at or above the
    // anchor are reachable along the up-chain already joined above; members
    // BELOW it would fan the anchor, and those queries are rejected before
    // reaching the oracle (see the fence branch in the property).
    let where_sql = case
        .where_pred
        .as_ref()
        .map_or_else(String::new, |p| format!(" WHERE {}", p.to_raw_sql()));
    if case.sel_dims.is_empty() {
        format!("SELECT {select_items} {from}{where_sql}")
    } else {
        let group_by = (1..=case.sel_dims.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("SELECT {select_items} {from}{where_sql} GROUP BY {group_by}")
    }
}

/// Independent oracle SQL: each selected grain aggregated on its own, then the
/// grains combined — `CROSS JOIN` when the query has no dimensions (one row per
/// grain), otherwise `FULL OUTER JOIN` on the NULL-safe dimension keys so a
/// group present at one grain and absent at another is preserved.
fn oracle_sql(case: &Case) -> String {
    // Grains in first-selected order.
    let mut anchors: Vec<&str> = Vec::new();
    for &i in &case.sel_metrics {
        let table = metric_table(METS[i]);
        if !anchors.contains(&table) {
            anchors.push(table);
        }
    }
    if anchors.is_empty() {
        anchors.push("t"); // Dimensions-only: the base-anchored DISTINCT shape.
    }
    let mut sql = format!(
        "SELECT {} FROM ({}) g0",
        oracle_projection(case, &anchors),
        grain_sql(case, anchors[0])
    );
    for (n, anchor) in anchors.iter().enumerate().skip(1) {
        let joined = grain_sql(case, anchor);
        if case.sel_dims.is_empty() {
            sql.push_str(&format!(" CROSS JOIN ({joined}) g{n}"));
        } else {
            let conditions: Vec<String> = case
                .sel_dims
                .iter()
                .map(|&d| {
                    let dim = DIMS[d];
                    format!("{} IS NOT DISTINCT FROM g{n}.{dim}", coalesced(dim, n))
                })
                .collect();
            sql.push_str(&format!(
                " FULL OUTER JOIN ({joined}) g{n} ON {}",
                conditions.join(" AND ")
            ));
        }
    }
    sql
}

/// `g0.<dim>` for one grain, `COALESCE(g0.<dim>, …, g{n-1}.<dim>)` beyond —
/// the key must be read from whichever grain supplied the row.
fn coalesced(dim: &str, groups: usize) -> String {
    if groups == 1 {
        format!("g0.{dim}")
    } else {
        let refs: Vec<String> = (0..groups).map(|g| format!("g{g}.{dim}")).collect();
        format!("COALESCE({})", refs.join(", "))
    }
}

/// The oracle's output projection: each dimension coalesced across every grain,
/// each metric read from the grain that computed it.
fn oracle_projection(case: &Case, anchors: &[&str]) -> String {
    let mut items: Vec<String> = case
        .sel_dims
        .iter()
        .map(|&d| format!("{} AS {}", coalesced(DIMS[d], anchors.len()), DIMS[d]))
        .collect();
    for &m in &case.sel_metrics {
        let metric = METS[m];
        let group = anchors
            .iter()
            .position(|a| *a == metric_table(metric))
            .expect("every selected metric's grain is an anchor");
        items.push(format!("g{group}.{metric}"));
    }
    items.join(", ")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn multi_hop_fence_and_aggregation(case in arb_case()) {
        check_case(&case)?;
    }
}

/// The setting that sidesteps the DuckDB bug behind TECH-DEBT #84.
const DUCKDB_BIND_BUG_WORKAROUND: &str = "SET disabled_optimizers = 'compressed_materialization'";

/// Whether `msg` is the DuckDB internal error of TECH-DEBT #84.
fn is_duckdb_group_by_bind_bug(msg: &str) -> bool {
    msg.contains("INTERNAL Error: Failed to bind column reference")
}

/// The property, for one case: a dimension or `where_clause` member below a
/// selected metric's grain is rejected as a fan trap; every other query expands
/// and matches the independent per-grain oracle. Shared by the proptest and the
/// pinned cases below so a pinned case runs exactly the property's checks.
fn check_case(case: &Case) -> Result<(), TestCaseError> {
    let def = build_def();
    let req = QueryRequest {
        where_clause: case.where_pred.as_ref().map(Pred::to_member_sql),
        dimensions: case
            .sel_dims
            .iter()
            .map(|&i| DimensionName::new(DIMS[i]))
            .collect(),
        metrics: case
            .sel_metrics
            .iter()
            .map(|&i| MetricName::new(METS[i]))
            .collect(),
        // PIN: FACTS is a row-level shape with no per-grain meaning; this harness oracles the three-table per-grain chain. TECH-DEBT #66
        facts: vec![],
    };

    // A dimension BELOW some selected metric's grain is the one shape that
    // must stay rejected: those parent rows genuinely fan across the
    // descendant's values, at one hop or two.
    let dimension_below_a_metric_grain = case.sel_metrics.iter().any(|&m| {
        case.sel_dims
            .iter()
            .any(|&d| level(dim_table(DIMS[d])) < level(metric_table(METS[m])))
    });
    let result = expand("multihop", &def, &req);

    if dimension_below_a_metric_grain {
        match result {
            Err(e) => {
                let msg = e.to_string();
                prop_assert!(
                    msg.contains("fan trap"),
                    "dimension below a metric's grain rejected, but not as a fan trap: {msg}"
                );
            }
            Ok(sql) => prop_assert!(
                false,
                "a dimension below a metric's grain must be rejected, got SQL:\n{sql}"
            ),
        }
        return Ok(());
    }

    // PBT-6: a `where_clause` member BELOW some selected metric's grain is
    // the same hazard through a different door. Filtering on `t` while
    // aggregating `sum(u.uw)` means joining `t` into the u-grain CTE, and
    // that join fans `u` — one copy per matching child row — so the sum
    // would be inflated. There is no correct number to return, so the
    // fence must reject it exactly as it rejects a dimension below the
    // grain. Checked after the dimension rule, which takes precedence.
    let mut pred_tables: Vec<&'static str> = Vec::new();
    if let Some(p) = case.where_pred.as_ref() {
        p.tables(&mut pred_tables);
    }
    let where_member_below_a_metric_grain = case.sel_metrics.iter().any(|&m| {
        pred_tables
            .iter()
            .any(|&pt| level(pt) < level(metric_table(METS[m])))
    });

    if where_member_below_a_metric_grain {
        match result {
            Err(e) => {
                let msg = e.to_string();
                prop_assert!(
                    msg.contains("fan trap"),
                    "where_clause member below a metric's grain rejected, but not \
                     as a fan trap: {msg}"
                );
            }
            Ok(sql) => prop_assert!(
                false,
                "a where_clause member below a metric's grain must be rejected \
                 -- joining it in fans that metric. Got SQL:\n{sql}"
            ),
        }
        return Ok(());
    }

    // Accepted-query branch: must expand and match the independent oracle.
    let expanded = match result {
        Ok(sql) => sql,
        Err(e) => {
            prop_assert!(false, "safe multi-hop query unexpectedly rejected: {e}");
            unreachable!()
        }
    };
    let oracle = oracle_sql(case);

    // Canonical projection (output columns sorted by name) so a column-order
    // difference between the two formulations is not a false diff.
    let mut proj_cols: Vec<String> = case
        .sel_dims
        .iter()
        .map(|&i| DIMS[i].to_string())
        .chain(case.sel_metrics.iter().map(|&i| METS[i].to_string()))
        .collect();
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
    let diff: i64 = match conn.query_row(&cmp, [], |r| r.get(0)) {
        Ok(diff) => diff,
        // TECH-DEBT #84: DuckDB 1.4/1.5 raise this internal error while
        // optimizing a GROUP BY over the chained join under some OR-of-ANDs
        // filters, for the expansion and the oracle alike. It is an engine
        // bug, not a planner or oracle answer, so the comparison is re-run
        // with the optimizer pass that trips it switched off. That pass only
        // compresses intermediate columns, so it cannot change a result; the
        // numbers must still agree. Any other error still fails the case, and
        // `duckdb_group_by_bind_bug_still_present` fails once DuckDB fixes the
        // bug, so this branch cannot outlive it.
        Err(e) if is_duckdb_group_by_bind_bug(&e.to_string()) => {
            conn.execute_batch(DUCKDB_BIND_BUG_WORKAROUND)
                .expect("disable compressed_materialization");
            conn.query_row(&cmp, [], |r| r.get(0)).unwrap_or_else(|e| {
                panic!("differential comparison query failed with compressed_materialization off: {e}\n--- expanded:\n{expanded}\n--- oracle:\n{oracle}")
            })
        }
        Err(e) => {
            panic!("differential comparison query failed: {e}\n--- expanded:\n{expanded}\n--- oracle:\n{oracle}")
        }
    };

    prop_assert_eq!(
        diff, 0,
        "multi-hop expansion disagrees with hand-written chained LEFT JOIN aggregation \
         (symmetric multiset diff = {}); dims={:?} metrics={:?}\n--- expanded:\n{}\n--- oracle:\n{}",
        diff, case.sel_dims, case.sel_metrics, expanded, oracle
    );
    Ok(())
}

/// PBT-6 guard: prove the generated cases reach both `where_clause` branches
/// the property distinguishes, rather than the suite passing because one is
/// never generated. A `prop_assert!` inside an unreachable branch is exactly as
/// green as a correct one.
#[test]
fn generator_reaches_both_where_clause_branches() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::HashSet;

    let mut runner = TestRunner::deterministic();
    let mut with_pred = 0usize;
    let mut without_pred = 0usize;
    let mut with_filter = 0usize;
    let mut qualified_ref = 0usize;
    let mut bare_ref = 0usize;
    let mut pred_below_grain = 0usize;
    let mut pred_at_or_above_grain = 0usize;
    let mut multi_grain_with_pred = 0usize;
    let mut distinct: HashSet<String> = HashSet::new();

    for _ in 0..400 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        // The dimension rule takes precedence in the property, so only count
        // cases that actually reach the where-clause branch.
        let dim_below = case.sel_metrics.iter().any(|&m| {
            case.sel_dims
                .iter()
                .any(|&d| level(dim_table(DIMS[d])) < level(metric_table(METS[m])))
        });
        match &case.where_pred {
            None => without_pred += 1,
            Some(p) => {
                with_pred += 1;
                if p.references_filter() {
                    with_filter += 1;
                }
                let (q, b) = p.reference_spellings();
                qualified_ref += usize::from(q);
                bare_ref += usize::from(b);
                distinct.insert(p.to_member_sql());
                if dim_below || case.sel_metrics.is_empty() {
                    continue;
                }
                let mut tables = Vec::new();
                p.tables(&mut tables);
                let below = case.sel_metrics.iter().any(|&m| {
                    tables
                        .iter()
                        .any(|&pt| level(pt) < level(metric_table(METS[m])))
                });
                if below {
                    pred_below_grain += 1;
                } else {
                    pred_at_or_above_grain += 1;
                    // Several grains in one query, each filtered by the same
                    // predicate -- the multi-grain injection this harness adds.
                    let grains: HashSet<&str> = case
                        .sel_metrics
                        .iter()
                        .map(|&m| metric_table(METS[m]))
                        .collect();
                    if grains.len() > 1 {
                        multi_grain_with_pred += 1;
                    }
                }
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
    // EXP-14 guard. `where_clause: None` in all five harnesses is how EXP-9/EXP-10
    // reached main; a qualification flag pinned to `false` would be the same
    // mistake one level down -- the field present, the generator never varying it.
    assert!(
        qualified_ref > 0 && bare_ref > 0,
        "where_clause members must be named BOTH bare and <table>.<name> across \
         cases (qualified={qualified_ref}, bare={bare_ref}); with either at zero \
         the qualified-reference resolution EXP-14 fixed is untested"
    );
    assert!(
        pred_below_grain > 0,
        "no case put a where_clause member below a selected metric's grain; the \
         fan-trap assertion for that branch is unreachable and therefore vacuous"
    );
    assert!(
        pred_at_or_above_grain > 0,
        "no case put a where_clause member at or above every selected metric's \
         grain; the per-grain predicate oracle is never compared"
    );
    assert!(
        multi_grain_with_pred > 0,
        "no accepted case spanned several grains while carrying a predicate; \
         the multi-grain injection this harness exists to cover is untested"
    );
    assert!(
        distinct.len() > 50,
        "generator produced only {} distinct predicates; search space collapsed",
        distinct.len()
    );
}

/// The instance CI's run of `multi_hop_fence_and_aggregation` shrank to (PR #250,
/// job 115465282382). Every `u` row points at `w` id 0, and the queries pinned
/// below filter on `((ucat < 0 AND wcat < 0) OR (wcat < 1 AND ucat < 1))`.
fn ci_or_of_ands_instance() -> Instance {
    Instance {
        w_rows: vec![(None, Some(-4)), (Some(2), Some(5))],
        u_rows: vec![
            (Some(0), Some(0), Some(1)),
            (Some(0), None, Some(5)),
            (Some(0), Some(2), Some(-2)),
        ],
        t_rows: vec![
            (None, Some(0), Some(-2)),
            (Some(3), Some(1), Some(-5)),
            (Some(1), Some(1), Some(1)),
            (Some(1), Some(0), Some(4)),
            (Some(2), Some(0), Some(4)),
            (Some(2), None, None),
            (None, Some(0), Some(1)),
            (Some(1), Some(2), Some(1)),
            (Some(3), Some(2), Some(-3)),
            (Some(1), Some(1), Some(1)),
        ],
    }
}

/// `((ucat < 0 AND wcat < 0) OR (wcat < 1 AND ucat < 1))`, both members bare.
fn ci_or_of_ands_pred() -> Pred {
    let cmp = |m: usize, k: i64| Box::new(Pred::Cmp(m, "<", k, false));
    Pred::Or(
        Box::new(Pred::And(cmp(1, 0), cmp(2, 0))),
        Box::new(Pred::And(cmp(2, 1), cmp(1, 1))),
    )
}

/// CI's minimal failing input: dimensions `ucat, wcat`, no metric. The
/// expansion is right (it returns no rows, as it should); the oracle's
/// `GROUP BY` over the chained join raised DuckDB 1.5.6's
/// `INTERNAL Error: Failed to bind column reference "wcat"`, an engine bug.
/// See TECH-DEBT #84.
#[test]
fn pinned_ci_or_of_ands_two_grains_dimensions_only() {
    let case = Case {
        inst: ci_or_of_ands_instance(),
        sel_dims: vec![1, 2],
        sel_metrics: vec![],
        where_pred: Some(ci_or_of_ands_pred()),
    };
    check_case(&case).unwrap();
}

/// The same data and filter with the root-grain metric `sv`: here the
/// EXPANSION's own `GROUP BY` hits the same DuckDB bug, so a user running this
/// query gets the internal error too (TECH-DEBT #84). The harness must still
/// check its number rather than fail on the engine error.
#[test]
fn pinned_ci_or_of_ands_two_grains_with_root_metric() {
    let case = Case {
        inst: ci_or_of_ands_instance(),
        sel_dims: vec![1, 2],
        sel_metrics: vec![0],
        where_pred: Some(ci_or_of_ands_pred()),
    };
    check_case(&case).unwrap();
}

/// TECH-DEBT #84 canary: the smallest data and query found that make DuckDB
/// raise the internal error the comparison above works around. Plain SQL, no
/// expansion involved. When a DuckDB upgrade makes this pass, remove the
/// workaround branch in `check_case` and resolve #84.
#[test]
fn duckdb_group_by_bind_bug_still_present() {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE w (id INTEGER, wcat INTEGER); \
         CREATE TABLE u (id INTEGER, fk_w INTEGER, ucat INTEGER); \
         CREATE TABLE t (fk_u INTEGER); \
         INSERT INTO u VALUES (0, 0, 0), (2, 0, 2); \
         INSERT INTO t VALUES (3), (1);",
    )
    .expect("setup");
    let query = "SELECT count(*) FROM (SELECT u.ucat, w.wcat FROM t \
                 JOIN u ON t.fk_u = u.id JOIN w ON u.fk_w = w.id \
                 WHERE (u.ucat = 0 AND w.wcat = 0) OR (u.ucat = 1 AND w.wcat = 1) \
                 GROUP BY 1, 2) g";
    let err = conn
        .query_row(query, [], |r| r.get::<_, i64>(0))
        .expect_err(
            "DuckDB no longer raises the TECH-DEBT #84 internal error: remove the \
             workaround in check_case and resolve #84",
        );
    assert!(
        is_duckdb_group_by_bind_bug(&err.to_string()),
        "unexpected error: {err}"
    );
    conn.execute_batch(DUCKDB_BIND_BUG_WORKAROUND)
        .expect("disable compressed_materialization");
    let rows: i64 = conn
        .query_row(query, [], |r| r.get(0))
        .expect("the workaround setting avoids the bug");
    assert_eq!(rows, 0);
}
