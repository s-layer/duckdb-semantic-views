//! Differential proptest for the two-table star join / fan-trap fence
//! (code-review 2026-07-18 PBT-1 / "Top-3 new property #1").
//!
//! The single-table [`differential_proptest`] never exercises a join, so the
//! fan-trap safety fence (`expand::fan_trap::check_fan_traps`) — the code that
//! decides *which* metric/dimension combinations may be computed at the
//! root-anchored grain and which must be rejected — had no randomized coverage.
//! Its three leak-throughs (EXP-1/2/3) were exactly there.
//!
//! Shape: a `ManyToOne` star. The ROOT (base) table `t` is the FK/"many" side
//! (`t.fk REFERENCES u.id`); `u` is the parent/"one" side. Generated data
//! includes dangling and NULL foreign keys and NULL group keys / values. `u.id`
//! is generated distinct so the declared PRIMARY KEY holds in the data and the
//! LEFT JOIN never fans `t`.
//!
//! Two invariants are checked per case:
//!
//! 1. **Parent-table metric + child dimension ⇒ rejected.** `SUM(u.w)` grouped
//!    by `t.d` asks for a parent-grain aggregate at a grain below it: each
//!    parent row genuinely fans across the child values. Neither the root-
//!    anchored path (which silently inflated it — EXP-1) nor per-grain
//!    aggregation can define it, so `expand` MUST reject it with a
//!    fan-trap-family error.
//! 2. **Accepted query ⇒ numerically correct.** For every query `expand`
//!    accepts, the result must equal an independently hand-written oracle,
//!    compared as a multiset inside DuckDB via a symmetric `EXCEPT ALL`
//!    difference (the same type-agnostic, order-independent comparator the
//!    single-table harness uses). Since v0.12.0 that set includes queries
//!    carrying the parent metric `SUM(u.w)`: they are computed **at the parent's
//!    own grain** (`FROM u`, never through the join) and joined back to the
//!    child-grain aggregate, so the oracle is a per-grain one. This pins both
//!    halves — no inflation, and no over-rejection of safe queries.

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, MetricName, QueryRequest};
use semantic_views::model::{
    AccessModifier, Cardinality, Dimension, Fact, Join, Metric, SemanticViewDefinition, TableRef,
};
use semantic_views::query::expr_items::lower_expression_items;
use semantic_views::query::items::RequestItems;

/// A generated star-schema instance: `n_u` parent rows and a list of child
/// rows. Parent row `i` has id `i` (distinct, so the declared PK holds),
/// category `ucat`, and value `w`. Child rows carry a foreign key `fk` (a valid
/// parent id, `None` = NULL, or a dangling id), a dimension `d`, and a value
/// `v`. `None` is a SQL NULL throughout.
#[derive(Debug, Clone)]
struct Instance {
    /// Parent rows: `(ucat, w)` for ids `0..n_u`.
    u_rows: Vec<(Option<i64>, Option<i64>)>,
    /// Child rows: `(fk, d, v)`.
    t_rows: Vec<(Option<i64>, Option<i64>, Option<i64>)>,
}

/// The queryable objects, referenced by these stable names in every case.
/// `uwd` (TECH-DEBT #54) is a PARENT-side dimension whose expression is the
/// fact `uw1` rather than a raw column: `u.uw1`, inlining to `u.w + 1`.
/// Dimensions were the one member kind facts were never inlined into, so this
/// grouped by an unbindable `u.uw1` before the fix. Grouping by it also puts
/// the inlined expression in a GROUP BY key rather than only a SELECT item,
/// which is where a mis-splice changes which rows collapse together.
///
/// `etd` and `eucat` (TECH-DEBT #76) are EXPRESSION items rather than declared
/// members ([`item_text`]): `td * 2 + 1` on the child, and
/// `coalesce(ucat, -1) + uw1` on the parent, which mixes a dimension and a fact
/// of one table. Each becomes a dimension of the query's own copy of the
/// definition, so it has to group, merge across grains and meet the fan-trap
/// fence exactly as a declared dimension on its table does.
///
/// `uw1` and `tv1` are FACTS named in `DIMENSIONS`, which Snowflake groups on
/// since its 2026_07 behaviour change: the parent fact `u.w + 1` and the child
/// fact `t.v + 1`. Each is lowered to a dimension on its fact's table, so it
/// has to group and merge across grains like `uwd` and `td`.
const DIMS: [&str; 7] = ["td", "ucat", "uwd", "etd", "eucat", "uw1", "tv1"];
/// `svf` (PAR-6) is a CHILD-grain metric whose expression reaches a fact
/// declared on the PARENT: `sum(t.v - u.uw1)` where `uw1 = u.w + 1` lives on
/// `u`. Before PAR-6 the fact was inlined but `u` was never joined, so this
/// metric could not be generated at all — the emitted SQL did not bind. The
/// fact expression is deliberately compound so a splice that loses its
/// parentheses (`t.v - u.w + 1`) computes a different number than the oracle's
/// `t.v - (u.w + 1)`.
/// `dsv` (EXP-24) is a DERIVED metric whose expression references the base
/// metric `sv` by its OWN-QUALIFIED name — `t.sv * 2`. The derived-metric
/// replacement map was keyed by bare canonical names only, while every
/// detection site matched the qualified spelling too, so the reference
/// contributed `t` to grain/join resolution and was then emitted verbatim as a
/// raw, unaggregated column. `graph/member_refs.rs` documents the qualified
/// cross-table form as legal, so this spelling has to work.
///
/// `esv`, `esw` and `edsv` (TECH-DEBT #76) are EXPRESSION items: `esv` is two
/// aggregates of a child fact and a child dimension under one derived metric,
/// `esw` an aggregate of a parent fact (so a parent-grain metric, like `sw`),
/// `edsv` an expression over the declared metrics `sv` and `ct`, and `esvf`
/// an aggregate with a `FILTER` clause, which belongs to its aggregate.
const METS: [&str; 9] = ["sv", "ct", "sw", "svf", "dsv", "esv", "esw", "edsv", "esvf"];

/// The item each pool name is queried with: a declared member's name, or for
/// the TECH-DEBT #76 entries an expression item named with `AS`.
fn item_text(name: &str) -> String {
    match name {
        "etd" => "td * 2 + 1 AS etd",
        "eucat" => "coalesce(ucat, -1) + uw1 AS eucat",
        "esv" => "sum(tv1) - count(td) AS esv",
        "esw" => "max(uw1) AS esw",
        "edsv" => "sv + ct AS edsv",
        "esvf" => "sum(tv1) FILTER (WHERE td > 0) AS esvf",
        other => other,
    }
    .to_string()
}

/// Whether a metric is aggregated at the PARENT grain (`u`).
fn is_parent_metric(name: &str) -> bool {
    matches!(name, "sw" | "esw")
}

/// Whether a dimension lives on the CHILD (`t`), below the parent's grain.
fn is_child_dim(name: &str) -> bool {
    matches!(name, "td" | "etd" | "tv1")
}

/// Members a generated `where_clause` may name (PBT-6), and which side of the
/// join each lives on. `ftd` / `fucat` are `LABELS = (FILTER)` members whose
/// expressions are compound `OR`s, so a splice that drops the parentheses
/// changes the answer — the same substitution surface the single-table harness
/// exercises, here across a join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WMember {
    /// `t.d` — the CHILD side.
    Td,
    /// `u.ucat` — the PARENT side.
    Ucat,
    /// Filter member on the child: `t.d = 0 OR t.d = 2`.
    Ftd,
    /// Filter member on the parent: `u.ucat = 0 OR u.ucat = 2`.
    Fucat,
    /// EXP-27: a filter member declared on the **parent** whose expression
    /// reaches a fact declared on the **child** — `fchain AS tv1 > 0` over
    /// `t.tv1 AS t.v + 1`. Its declared table does not fan, but resolving it
    /// splices in the child's columns, so the query has to join the child
    /// anyway. That join is what #207 started emitting and what no fence
    /// looked at: `check_where_clause_fan_traps` walked only the member's
    /// declared table (`u`, safe) and `check_referenced_fact_fan_traps` is
    /// handed the queried members, never the predicate's.
    Fchain,
}

impl WMember {
    /// The member's declared name, as written in a `where_clause`.
    fn name(self) -> &'static str {
        match self {
            WMember::Td => "td",
            WMember::Ucat => "ucat",
            WMember::Ftd => "ftd",
            WMember::Fucat => "fucat",
            WMember::Fchain => "fchain",
        }
    }

    /// The raw SQL the member's expression stands for, for the oracle.
    fn raw(self) -> &'static str {
        match self {
            WMember::Td => "t.d",
            WMember::Ucat => "u.ucat",
            WMember::Ftd => "(t.d = 0 OR t.d = 2)",
            WMember::Fucat => "(u.ucat = 0 OR u.ucat = 2)",
            // The fact written out independently, parenthesized at the
            // reference site exactly as the splice must parenthesize it.
            WMember::Fchain => "((t.v + 1) > 0)",
        }
    }

    /// Whether resolving the member forces the CHILD table into the query. A
    /// predicate touching the child cannot be evaluated inside the parent-grain
    /// CTE without joining `t` into it — which fans `u` — so such a query must
    /// be rejected when the parent metric is selected.
    ///
    /// EXP-27: `Fchain` counts here even though it is *declared* on the parent.
    /// What matters is which tables the resolved predicate names, not where the
    /// member's name was written down — that mismatch is the whole bug.
    fn is_child_side(self) -> bool {
        matches!(self, WMember::Td | WMember::Ftd | WMember::Fchain)
    }

    /// Whether the member is a bare column reference (so it can carry a
    /// comparison operator) as opposed to a self-contained boolean filter.
    fn is_comparable(self) -> bool {
        matches!(self, WMember::Td | WMember::Ucat)
    }

    /// Whether the member reaches its tables through a FACT reference (EXP-27)
    /// rather than naming them itself — the generator-coverage guard for the
    /// new arm.
    fn is_fact_chain(self) -> bool {
        matches!(self, WMember::Fchain)
    }
}

/// A generated pre-aggregation predicate over the star's members (PBT-6).
#[derive(Debug, Clone)]
enum Pred {
    /// `<member> <op> <literal>` for a comparable member.
    Cmp(WMember, &'static str, i64),
    /// A bare filter member — the substitution/precedence surface.
    Filter(WMember),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
}

impl Pred {
    /// The `where_clause` text: member NAMES, composites parenthesized so the
    /// SQL parse matches this AST, filter members left BARE.
    fn to_member_sql(&self) -> String {
        match self {
            Pred::Cmp(m, op, k) => format!("{} {op} {k}", m.name()),
            Pred::Filter(m) => m.name().to_string(),
            Pred::And(a, b) => format!("({} AND {})", a.to_member_sql(), b.to_member_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_member_sql(), b.to_member_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_member_sql()),
        }
    }

    /// The oracle's independent rendering: raw physical columns, filter members
    /// expanded, every operand explicitly parenthesized.
    fn to_raw_sql(&self) -> String {
        match self {
            Pred::Cmp(m, op, k) => format!("({} {op} {k})", m.raw()),
            Pred::Filter(m) => format!("({})", m.raw()),
            Pred::And(a, b) => format!("({} AND {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_raw_sql()),
        }
    }

    /// Whether any named member lives on the child table.
    fn touches_child(&self) -> bool {
        match self {
            Pred::Cmp(m, _, _) | Pred::Filter(m) => m.is_child_side(),
            Pred::And(a, b) | Pred::Or(a, b) => a.touches_child() || b.touches_child(),
            Pred::Not(a) => a.touches_child(),
        }
    }

    /// Whether any named member is a filter member (generator-coverage guard).
    fn references_filter(&self) -> bool {
        match self {
            Pred::Cmp(m, _, _) | Pred::Filter(m) => !m.is_comparable(),
            Pred::And(a, b) | Pred::Or(a, b) => a.references_filter() || b.references_filter(),
            Pred::Not(a) => a.references_filter(),
        }
    }

    /// Whether any named member reaches its table through a fact chain
    /// (EXP-27) — the generator-coverage guard for the `Fchain` arm.
    fn references_fact_chain(&self) -> bool {
        match self {
            Pred::Cmp(m, _, _) | Pred::Filter(m) => m.is_fact_chain(),
            Pred::And(a, b) | Pred::Or(a, b) => {
                a.references_fact_chain() || b.references_fact_chain()
            }
            Pred::Not(a) => a.references_fact_chain(),
        }
    }
}

fn arb_pred() -> impl Strategy<Value = Pred> {
    let leaf = prop_oneof![
        3 => (
            prop_oneof![Just(WMember::Td), Just(WMember::Ucat)],
            prop_oneof![Just("<"), Just("<="), Just("="), Just("<>"), Just(">="), Just(">")],
            -1i64..=3,
        ).prop_map(|(m, op, k)| Pred::Cmp(m, op, k)),
        2 => prop_oneof![Just(WMember::Ftd), Just(WMember::Fucat)].prop_map(Pred::Filter),
        // EXP-27: the fact-chain member gets its own weight so it appears in a
        // healthy share of predicates rather than only inside deep compounds.
        1 => Just(Pred::Filter(WMember::Fchain)),
    ];
    leaf.prop_recursive(3, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            inner.prop_map(|a| Pred::Not(Box::new(a))),
        ]
    })
}

/// A full case: an instance plus the non-empty subset of dims + metrics to
/// query (indices into `DIMS` / `METS`), and the optional pre-aggregation
/// predicate (PBT-6).
#[derive(Debug, Clone)]
struct Case {
    inst: Instance,
    sel_dims: Vec<usize>,
    sel_metrics: Vec<usize>,
    where_pred: Option<Pred>,
}

fn arb_instance() -> impl Strategy<Value = Instance> {
    // Small signed value domain + NULL, mirroring the single-table harness.
    let val_cell = prop_oneof![
        1 => Just(None),
        3 => (-5i64..=5).prop_map(Some),
    ];
    let cat_cell = prop_oneof![
        1 => Just(None),
        4 => (0i64..3).prop_map(Some),
    ];
    (1usize..=4).prop_flat_map(move |n_u| {
        let u_row = (cat_cell.clone(), val_cell.clone());
        let u_rows = prop::collection::vec(u_row, n_u);
        // fk: NULL, a valid parent id (0..n_u), or a dangling id (n_u, which is
        // never a generated parent id since ids are 0..n_u).
        let fk_cell = prop_oneof![
            1 => Just(None),
            4 => (0i64..n_u as i64).prop_map(Some),
            1 => Just(Some(n_u as i64)),
        ];
        let t_row = (fk_cell, cat_cell.clone(), val_cell.clone()).prop_map(|(fk, d, v)| (fk, d, v));
        let t_rows = prop::collection::vec(t_row, 0..=20);
        (u_rows, t_rows).prop_map(|(u_rows, t_rows)| Instance { u_rows, t_rows })
    })
}

fn arb_case() -> impl Strategy<Value = Case> {
    arb_instance().prop_flat_map(|inst| {
        let dim_sel =
            prop::sample::subsequence((0..DIMS.len()).collect::<Vec<_>>(), 0..=DIMS.len());
        let met_sel =
            prop::sample::subsequence((0..METS.len()).collect::<Vec<_>>(), 0..=METS.len());
        // PBT-6: one case in four keeps the original no-predicate coverage.
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

/// Build the semantic-view definition: root/child table `t` with `t.fk
/// REFERENCES u.id` (ManyToOne), a dimension + metric on each side, and a
/// `count(*)` base metric at the root grain.
fn build_def() -> SemanticViewDefinition {
    let tables = vec![
        TableRef {
            alias: "t".to_string(),
            table: "t".to_string(),
            pk_columns: vec![],
            unique_constraints: vec![],
            comment: None,
            synonyms: vec![],
            tags: Vec::new(),
        },
        TableRef {
            alias: "u".to_string(),
            table: "u".to_string(),
            pk_columns: vec!["id".to_string()],
            unique_constraints: vec![],
            comment: None,
            synonyms: vec![],
            tags: Vec::new(),
        },
    ];
    let dimensions = vec![
        Dimension {
            name: "td".to_string(),
            expr: "t.d".to_string(),
            source_table: Some("t".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            tags: Vec::new(),
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
        },
        Dimension {
            name: "ucat".to_string(),
            expr: "u.ucat".to_string(),
            source_table: Some("u".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            tags: Vec::new(),
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
        },
        // TECH-DEBT #54: a dimension whose expression names a FACT on its own
        // table, the form Snowflake's validation rules explicitly permit.
        Dimension {
            name: "uwd".to_string(),
            expr: "u.uw1".to_string(),
            source_table: Some("u".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            tags: Vec::new(),
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
        },
        // PBT-6 filter members: never selected as output, named only by a
        // generated `where_clause`. Compound expressions, so a splice that
        // loses its parentheses changes the answer.
        Dimension {
            name: "ftd".to_string(),
            expr: "t.d = 0 OR t.d = 2".to_string(),
            source_table: Some("t".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: true,
            tags: Vec::new(),
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
        },
        Dimension {
            name: "fucat".to_string(),
            expr: "u.ucat = 0 OR u.ucat = 2".to_string(),
            source_table: Some("u".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: true,
            tags: Vec::new(),
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
        },
        // EXP-27: declared on the PARENT, but its expression names a fact on
        // the CHILD. Resolving it joins `t`, which fans `u` — so a predicate
        // naming it must be rejected alongside the parent-grain metric even
        // though nothing about the member's own table says so.
        Dimension {
            name: "fchain".to_string(),
            expr: "tv1 > 0".to_string(),
            source_table: Some("u".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: true,
            tags: Vec::new(),
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
        },
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
        tags: Vec::new(),
    };
    let metrics = vec![
        base_metric("sv", "sum(t.v)", Some("t")),
        base_metric("ct", "count(*)", None),
        base_metric("sw", "sum(u.w)", Some("u")),
        // PAR-6: a child-grain metric reaching a fact on the parent table. The
        // parent is the "one" side, so the join does not fan `t` and the
        // aggregate stays at the child grain.
        base_metric("svf", "sum(t.v - u.uw1)", Some("t")),
        // EXP-24: derived (no source_table), referencing `sv` OWN-QUALIFIED.
        base_metric("dsv", "t.sv * 2", None),
    ];
    let joins = vec![Join {
        from_alias: "t".to_string(),
        table: "u".to_string(),
        fk_columns: vec!["fk".to_string()],
        ref_columns: vec!["id".to_string()],
        name: Some("t_u".to_string()),
        cardinality: Cardinality::ManyToOne,
    }];
    SemanticViewDefinition {
        tables,
        dimensions,
        metrics,
        joins,
        facts: vec![
            Fact {
                name: "uw1".to_string(),
                expr: "u.w + 1".to_string(),
                source_table: Some("u".to_string()),
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                access: AccessModifier::Public,
                tags: Vec::new(),
                sample_values: Vec::new(),
            },
            // EXP-27: the CHILD-side fact the parent-declared `fchain` member
            // reaches. Compound, so a splice that loses the parentheses
            // (`t.v + 1 > 0` vs `(t.v + 1) > 0`) would show up as a diff too.
            Fact {
                name: "tv1".to_string(),
                expr: "t.v + 1".to_string(),
                source_table: Some("t".to_string()),
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                access: AccessModifier::Public,
                tags: Vec::new(),
                sample_values: Vec::new(),
            },
        ],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
    }
}

/// Create the physical tables and insert the generated rows.
fn make_db(inst: &Instance) -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE u (id INTEGER, ucat INTEGER, w INTEGER); \
         CREATE TABLE t (fk INTEGER, d INTEGER, v INTEGER);",
    )
    .expect("create tables");

    let cell = |c: &Option<i64>| c.map_or_else(|| "NULL".to_string(), |v| v.to_string());

    if !inst.u_rows.is_empty() {
        let values: Vec<String> = inst
            .u_rows
            .iter()
            .enumerate()
            .map(|(i, (ucat, w))| format!("({i},{},{})", cell(ucat), cell(w)))
            .collect();
        conn.execute_batch(&format!("INSERT INTO u VALUES {};", values.join(",")))
            .expect("insert u rows");
    }
    if !inst.t_rows.is_empty() {
        let values: Vec<String> = inst
            .t_rows
            .iter()
            .map(|(fk, d, v)| format!("({},{},{})", cell(fk), cell(d), cell(v)))
            .collect();
        conn.execute_batch(&format!("INSERT INTO t VALUES {};", values.join(",")))
            .expect("insert t rows");
    }
    conn
}

/// The child-grain (`t`) half of the oracle: the metrics computed over `t`,
/// grouped by the selected dimensions. The FROM is always `t LEFT JOIN u`:
/// because `u.id` is unique, joining `u` never changes `t`'s multiset for
/// `count(*)` / `sum(t.v)`, and grouping by a parent dimension is a plain group
/// key. Metrics-only ⇒ global aggregate (no GROUP BY); anything with dimensions
/// ⇒ GROUP BY the projected dimension ordinals (multiset-equal to the
/// expansion's SELECT DISTINCT for the dims-only case).
fn child_grain_sql(case: &Case) -> String {
    let dim_items: Vec<String> = case
        .sel_dims
        .iter()
        .map(|&i| match DIMS[i] {
            "td" => "t.d AS td".to_string(),
            "ucat" => "u.ucat AS ucat".to_string(),
            // TECH-DEBT #54: the fact written out independently. A dangling or
            // NULL `fk` leaves `u.w` NULL, so the key is NULL -- which the
            // NULL-safe grouping below has to keep as its own group.
            "uwd" => "u.w + 1 AS uwd".to_string(),
            // TECH-DEBT #76: the expression items, written out independently.
            "etd" => "t.d * 2 + 1 AS etd".to_string(),
            "eucat" => "coalesce(u.ucat, -1) + (u.w + 1) AS eucat".to_string(),
            // Facts grouped on in DIMENSIONS, written out independently.
            "uw1" => "u.w + 1 AS uw1".to_string(),
            "tv1" => "t.v + 1 AS tv1".to_string(),
            other => unreachable!("unexpected dim {other}"),
        })
        .collect();
    let met_items: Vec<String> = case
        .sel_metrics
        .iter()
        .filter(|&&i| !is_parent_metric(METS[i]))
        .map(|&i| match METS[i] {
            "sv" => "sum(t.v) AS sv".to_string(),
            "ct" => "count(*) AS ct".to_string(),
            // PAR-6: the cross-table fact reference, written out independently
            // — the fact's own expression `u.w + 1`, parenthesized at the
            // reference site, aggregated at the child grain through the same
            // LEFT JOIN. A NULL or dangling `fk` makes `u.w + 1` NULL, which
            // `sum` skips, exactly as it does in the expansion.
            "svf" => "sum(t.v - (u.w + 1)) AS svf".to_string(),
            // EXP-24: the oracle writes the derived metric out independently —
            // the base aggregate, doubled. The expansion must inline `t.sv`
            // to the same thing rather than leaving the reference in the SQL.
            "dsv" => "sum(t.v) * 2 AS dsv".to_string(),
            // TECH-DEBT #76: the expression items' aggregates and arithmetic.
            "esv" => "sum(t.v + 1) - count(t.d) AS esv".to_string(),
            "edsv" => "sum(t.v) + count(*) AS edsv".to_string(),
            "esvf" => "sum(t.v + 1) FILTER (WHERE t.d > 0) AS esvf".to_string(),
            other => unreachable!("unexpected child-grain metric {other}"),
        })
        .collect();
    let select_items = dim_items
        .iter()
        .chain(met_items.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let from = "FROM t LEFT JOIN u ON t.fk = u.id";
    // PBT-6: the predicate filters the rows feeding this grain's aggregate,
    // BEFORE the grouping. A parent-side member filters through the LEFT JOIN
    // here, exactly as the expansion's own child-grain CTE does.
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

/// The parent-grain (`u`) half of the oracle: `sum(u.w)` over the parent table
/// **on its own**, never through the join — that is the whole point of
/// computing a metric at its own grain, and the independent statement of what
/// the root-anchored `FROM t LEFT JOIN u` got wrong (each parent row counted
/// once per child row, childless parents dropped entirely).
///
/// Only PARENT-side dimensions can be selected alongside it (`ucat`, and since
/// TECH-DEBT #54 the fact-valued `uwd`): `td` lives below `u`'s grain and the
/// query is rejected before it gets here.
/// PBT-6: the predicate is applied inside THIS grain too, before its
/// aggregate. Only parent-side members can appear — a predicate touching the
/// child is rejected before it reaches the oracle (see the fence branch in the
/// property), because evaluating it here would require joining `t` into the
/// parent CTE and fanning `u`.
fn parent_grain_sql(case: &Case) -> String {
    let where_sql = case
        .where_pred
        .as_ref()
        .map_or_else(String::new, |p| format!(" WHERE {}", p.to_raw_sql()));
    let keys = parent_dim_items(case);
    let metrics = parent_metric_names(case)
        .iter()
        .map(|m| match *m {
            "sw" => "sum(u.w) AS sw",
            // TECH-DEBT #76: `max(uw1)`, the fact written out.
            "esw" => "max(u.w + 1) AS esw",
            other => unreachable!("unexpected parent-grain metric {other}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    if keys.is_empty() {
        format!("SELECT {metrics} FROM u{where_sql}")
    } else {
        let group_by = (1..=keys.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "SELECT {}, {metrics} FROM u{where_sql} GROUP BY {group_by}",
            keys.join(", ")
        )
    }
}

/// The queried dimensions that live on the PARENT, written against `u` alone —
/// the projection the parent-grain aggregate groups by. `uwd` is the #54 case:
/// the fact `uw1` written out independently as `u.w + 1`.
fn parent_dim_items(case: &Case) -> Vec<String> {
    case.sel_dims
        .iter()
        .filter_map(|&i| match DIMS[i] {
            "ucat" => Some("u.ucat AS ucat".to_string()),
            "uwd" => Some("u.w + 1 AS uwd".to_string()),
            "eucat" => Some("coalesce(u.ucat, -1) + (u.w + 1) AS eucat".to_string()),
            "uw1" => Some("u.w + 1 AS uw1".to_string()),
            _ => None,
        })
        .collect()
}

/// The names of those dimensions, in the same order — the keys the two grains
/// are combined on.
fn parent_dim_names(case: &Case) -> Vec<&'static str> {
    case.sel_dims
        .iter()
        .map(|&i| DIMS[i])
        .filter(|d| matches!(*d, "ucat" | "uwd" | "eucat" | "uw1"))
        .collect()
}

/// The selected parent-grain metrics, in request order.
fn parent_metric_names(case: &Case) -> Vec<&'static str> {
    case.sel_metrics
        .iter()
        .map(|&i| METS[i])
        .filter(|m| is_parent_metric(m))
        .collect()
}

/// Independent oracle SQL for a query `expand` should accept.
///
/// Without the parent metric this is the plain child-grain aggregation. With it
/// the two grains are computed separately and combined — `CROSS JOIN` for the
/// dimensionless case (one row each), `FULL OUTER JOIN` on the NULL-safe
/// dimension key otherwise, so a `ucat` present at only one grain survives.
fn oracle_sql(case: &Case) -> String {
    let parent_metrics = parent_metric_names(case);
    if parent_metrics.is_empty() {
        return child_grain_sql(case);
    }
    let child_metrics: Vec<&str> = case
        .sel_metrics
        .iter()
        .map(|&i| METS[i])
        .filter(|&m| !is_parent_metric(m))
        .collect();
    let parent = parent_grain_sql(case);
    if child_metrics.is_empty() {
        // Only the parent metric: one grain, nothing to join it to.
        return parent;
    }
    let mut items: Vec<String> = Vec::new();
    if case.sel_dims.is_empty() {
        for m in &child_metrics {
            items.push(format!("a.{m}"));
        }
        items.extend(parent_metrics.iter().map(|m| format!("b.{m}")));
        let child = child_grain_sql(case);
        return format!(
            "SELECT {} FROM ({child}) a CROSS JOIN ({parent}) b",
            items.join(", ")
        );
    }
    // Every parent-side dimension is a combine key, not just `ucat` (#54 added
    // a second). A group present at only one grain must survive, so the keys
    // are COALESCEd and matched NULL-safely.
    let keys = parent_dim_names(case);
    for k in &keys {
        items.push(format!("COALESCE(a.{k}, b.{k}) AS {k}"));
    }
    for m in &child_metrics {
        items.push(format!("a.{m}"));
    }
    items.extend(parent_metrics.iter().map(|m| format!("b.{m}")));
    let child = child_grain_sql(case);
    let on = keys
        .iter()
        .map(|k| format!("a.{k} IS NOT DISTINCT FROM b.{k}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    format!(
        "SELECT {} FROM ({child}) a FULL OUTER JOIN ({parent}) b ON {on}",
        items.join(", ")
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn star_join_fence_and_aggregation(case in arb_case()) {
        // TECH-DEBT #76: the items go through the same lowering
        // `semantic_view()` gives them, so an expression item is planned as the
        // member it becomes.
        let texts = |sel: &[usize], pool: &[&str]| -> Vec<String> {
            sel.iter().map(|&i| item_text(pool[i])).collect()
        };
        let items = RequestItems::parse(
            &texts(&case.sel_dims, &DIMS),
            &texts(&case.sel_metrics, &METS),
            &[],
            &[],
        )
        .expect("the pool's items parse");
        let stored = build_def();
        let lowered = lower_expression_items("star", &stored, items)
            .unwrap_or_else(|e| panic!("the pool's expression items lower: {e:?}"));
        let def = lowered.def.clone().unwrap_or(stored);
        let req = QueryRequest {
            where_clause: case.where_pred.as_ref().map(Pred::to_member_sql),
            dimensions: lowered
                .items
                .dimensions
                .iter()
                .map(|it| DimensionName::new(it.member.clone()))
                .collect(),
            metrics: lowered
                .items
                .metrics
                .iter()
                .map(|it| MetricName::new(it.member.clone()))
                .collect(),
            // PIN: FACTS is oracled by child_grain_proptest and metamorphic_proptest; this harness oracles the fan-trap fence and per-grain math. TECH-DEBT #66
            facts: vec![],
        };

        let selects_parent_metric = case.sel_metrics.iter().any(|&i| is_parent_metric(METS[i]));
        let selects_child_dim = case.sel_dims.iter().any(|&i| is_child_dim(DIMS[i]));
        let result = expand("star", &def, &req);

        if selects_parent_metric && selects_child_dim {
            // The parent metric grouped by a CHILD dimension stays rejected:
            // `t.d` is below `u`'s grain, so each parent row genuinely fans
            // across the child values. Per-grain aggregation cannot define this
            // query — only the fan-trap error is correct.
            match result {
                Err(e) => {
                    let msg = e.to_string();
                    prop_assert!(
                        msg.contains("fan trap"),
                        "parent metric + child dimension rejected, but not as a fan trap: {msg}"
                    );
                }
                Ok(sql) => prop_assert!(
                    false,
                    "a parent-grain metric grouped by a child dimension must be rejected, \
                     got SQL:\n{sql}"
                ),
            }
            return Ok(());
        }

        // PBT-6: a predicate naming a CHILD-side member alongside the PARENT
        // metric is the where_clause analogue of the case above. Filtering on
        // `t` requires joining it into the parent-grain aggregate, and that
        // join fans `u` — each parent row once per matching child row — so
        // `sum(u.w)` would be inflated. `check_where_clause_fan_traps` must
        // reject it; there is no correct number to return.
        if selects_parent_metric
            && case.where_pred.as_ref().is_some_and(Pred::touches_child)
        {
            match result {
                Err(e) => {
                    let msg = e.to_string();
                    prop_assert!(
                        msg.contains("fan trap"),
                        "parent metric + child-side where_clause member rejected, \
                         but not as a fan trap: {msg}"
                    );
                }
                Ok(sql) => prop_assert!(
                    false,
                    "a where_clause on the child table must not be applied to the \
                     parent-grain metric sum(u.w) -- the join fans it. Got SQL:\n{sql}"
                ),
            }
            return Ok(());
        }

        // Accepted-query branch: must expand and match the independent oracle.
        let expanded = match result {
            Ok(sql) => sql,
            Err(e) => {
                prop_assert!(false, "safe query unexpectedly rejected: {e}");
                unreachable!()
            }
        };
        // The expansion names an expression item's column by its member;
        // name each column as the pool does, as `semantic_view()`'s alias
        // would.
        let renamed = lowered
            .items
            .dimensions
            .iter()
            .chain(&lowered.items.metrics)
            .map(|it| {
                let member = it.member.trim_matches('"');
                let name = it.alias.as_deref().unwrap_or(member);
                format!("\"{member}\" AS {name}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let expanded = format!("SELECT {renamed} FROM ({expanded}) ex");
        let oracle = oracle_sql(&case);

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
        let diff: i64 = conn.query_row(&cmp, [], |r| r.get(0)).unwrap_or_else(|e| {
            panic!("differential comparison query failed: {e}\n--- expanded:\n{expanded}\n--- oracle:\n{oracle}")
        });

        prop_assert_eq!(
            diff, 0,
            "star-join expansion disagrees with hand-written LEFT JOIN aggregation \
             (symmetric multiset diff = {})\n--- expanded:\n{}\n--- oracle:\n{}",
            diff, expanded, oracle
        );
    }
}

/// PBT-6 guard: prove the generated cases actually reach the branches the
/// property distinguishes, rather than the suite passing because a branch is
/// never generated. A `prop_assert!` inside an unreachable branch is exactly as
/// green as a correct one, so the branch counts are asserted directly.
///
/// Also pins that the predicate itself varies (CLAUDE.md: the field being
/// present in a struct literal is not coverage — the generator has to vary it).
#[test]
fn generator_reaches_both_where_clause_branches() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::HashSet;

    let mut runner = TestRunner::deterministic();
    let mut with_pred = 0usize;
    let mut without_pred = 0usize;
    let mut with_filter = 0usize;
    let mut with_fact_chain = 0usize;
    // The two branches the property treats differently under a predicate.
    let mut parent_metric_child_pred = 0usize;
    let mut parent_metric_parent_only_pred = 0usize;
    // EXP-27 specifically: the fact-chain member alongside the parent metric,
    // which is the pair the new fence rejects. Without this the `Fchain` arm
    // could be generated only in accepted queries and the fence stay untested.
    let mut parent_metric_fact_chain_pred = 0usize;
    let mut accepted_fact_chain_pred = 0usize;
    // PBT-15 (code-review 2026-08-08): the two metrics this harness added for
    // specific past defects. Nothing asserted they are ever SELECTED, let alone
    // that they reach the numeric comparison — the counts below were all about
    // the predicate. `differential_proptest` pins its equivalent
    // (`references_chained_fact`); this file's own doctrine is to assert reach,
    // not assume it.
    //   `svf` — PAR-6: a child-grain metric whose expression reaches a fact on
    //           the parent, with a compound fact expression that catches a
    //           paren-losing splice.
    //   `dsv` — EXP-24: a derived metric referencing its base by the
    //           own-qualified spelling `t.sv`.
    // Both are only worth anything on the ORACLED path, so each is counted
    // twice: selected at all, and selected in a case the property actually
    // compares against the oracle.
    let mut svf_selected = 0usize;
    let mut svf_oracled = 0usize;
    let mut dsv_selected = 0usize;
    let mut dsv_oracled = 0usize;
    let mut svf_and_dsv_oracled = 0usize;
    // `uw1` / `tv1`: facts grouped on in DIMENSIONS, lowered like the
    // expression items.
    const EXPRESSION_ITEMS: [&str; 8] =
        ["etd", "eucat", "esv", "esw", "edsv", "esvf", "uw1", "tv1"];
    let mut expression_oracled = [0usize; 8];
    let mut expression_fenced = 0usize;
    let mut distinct: HashSet<String> = HashSet::new();

    for _ in 0..400 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        let selects_parent_metric = case.sel_metrics.iter().any(|&i| is_parent_metric(METS[i]));
        let selects_child_dim = case.sel_dims.iter().any(|&i| is_child_dim(DIMS[i]));

        // The two rejection branches of the property, mirrored: everything else
        // falls through to the differential comparison.
        let pred_touches_child = case.where_pred.as_ref().is_some_and(Pred::touches_child);
        let oracled = !(selects_parent_metric && (selects_child_dim || pred_touches_child));
        let has = |name: &str| case.sel_metrics.iter().any(|&i| METS[i] == name);
        if has("svf") {
            svf_selected += 1;
            if oracled {
                svf_oracled += 1;
            }
        }
        if has("dsv") {
            dsv_selected += 1;
            if oracled {
                dsv_oracled += 1;
            }
        }
        if oracled && has("svf") && has("dsv") {
            svf_and_dsv_oracled += 1;
        }
        // TECH-DEBT #76: each expression item must reach the oracle, and the
        // parent-grain one must also meet the fan-trap fence.
        for (k, name) in EXPRESSION_ITEMS.iter().enumerate() {
            let picked = case.sel_dims.iter().any(|&i| DIMS[i] == *name) || has(name);
            if picked && oracled {
                expression_oracled[k] += 1;
            }
        }
        if has("esw") && selects_child_dim {
            expression_fenced += 1;
        }
        match &case.where_pred {
            None => without_pred += 1,
            Some(p) => {
                with_pred += 1;
                if p.references_filter() {
                    with_filter += 1;
                }
                if p.references_fact_chain() {
                    with_fact_chain += 1;
                    if !(selects_parent_metric && selects_child_dim) && !selects_parent_metric {
                        accepted_fact_chain_pred += 1;
                    }
                }
                distinct.insert(p.to_member_sql());
                // The dim-based fence branch takes precedence in the property,
                // so only count cases that actually reach the where-clause one.
                if selects_parent_metric && !selects_child_dim {
                    if p.touches_child() {
                        parent_metric_child_pred += 1;
                    } else {
                        parent_metric_parent_only_pred += 1;
                    }
                    if p.references_fact_chain() {
                        parent_metric_fact_chain_pred += 1;
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
        "generator never referenced a filter member -- the cross-table \
         member-substitution surface is untested"
    );
    assert!(
        parent_metric_child_pred > 0,
        "no case reached the parent-metric + child-side-predicate branch; the \
         where_clause fan-trap assertion is unreachable and therefore vacuous"
    );
    assert!(
        parent_metric_parent_only_pred > 0,
        "no case reached the parent-metric + parent-only-predicate branch; the \
         per-grain predicate oracle is never compared"
    );
    assert!(
        with_fact_chain > 0,
        "generator never referenced the EXP-27 fact-chain member -- a \
         where_clause member reaching another table THROUGH a fact has no \
         randomized coverage"
    );
    assert!(
        parent_metric_fact_chain_pred > 0,
        "no case paired the fact-chain member with the parent metric; the \
         EXP-27 fence is never exercised and its assertion is vacuous \
         (present={with_fact_chain})"
    );
    assert!(
        accepted_fact_chain_pred > 0,
        "the fact-chain member only ever appeared in REJECTED cases, so the \
         non-fanning half -- that resolving it still produces the right number \
         -- is never oracled"
    );
    // PBT-15: the metric-selection half of the same guard.
    assert!(
        svf_selected > 0 && svf_oracled > 0,
        "the PAR-6 cross-table metric `svf` never reached the numeric \
         comparison (selected={svf_selected}, oracled={svf_oracled}); its \
         parent-fact splice has no randomized coverage here"
    );
    assert!(
        dsv_selected > 0 && dsv_oracled > 0,
        "the EXP-24 derived metric `dsv` never reached the numeric comparison \
         (selected={dsv_selected}, oracled={dsv_oracled}); the own-qualified \
         derived-metric reference has no randomized coverage here"
    );
    assert!(
        svf_and_dsv_oracled > 0,
        "`svf` and `dsv` were never oracled in the SAME query, so the \
         cross-table fact splice and the derived-metric substitution are never \
         exercised against each other"
    );
    for (name, n) in EXPRESSION_ITEMS.iter().zip(expression_oracled) {
        assert!(
            n > 0,
            "the expression item `{name}` never reached the numeric comparison; \
             TECH-DEBT #76 has no randomized coverage for it"
        );
    }
    assert!(
        expression_fenced > 0,
        "the parent-grain expression metric `esw` never met a child dimension, \
         so the fan-trap fence is never exercised on an expression item"
    );
    assert!(
        distinct.len() > 50,
        "generator produced only {} distinct predicates; search space collapsed",
        distinct.len()
    );
}
