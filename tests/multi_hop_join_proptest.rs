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
//! 1. **Dimension below a metric's grain ⇒ once per group, or refused across
//!    a temporal hop.** `SUM(u.uw)` grouped by `t.d`, or `SUM(w.ww)` grouped by
//!    `u.ucat` / `t.d`, asks for an ancestor's aggregate at a grain below it:
//!    those rows fan across the descendant's values, at one hop or two. Since
//!    Phase 7 PR 4c each ancestor row counts once in every group it reaches,
//!    as Snowflake counts it (fixture 7, N1-N7), and the oracle states that as
//!    the DISTINCT (ancestor row, group) pairs ([`fanned_grain_sql`]). A
//!    fan-out that walks the ASOF or range hop backwards stays refused with a
//!    fan-trap-family error, as Snowflake refuses the ASOF one (fixture 1, A5);
//!    the root-anchored path once silently inflated all of these (EXP-1).
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
//!
//! Phase 7.1 makes the first hop an **ASOF** relationship in about half the
//! cases: `t (fk_u, ts) REFERENCES u (uk, ASOF us)`, `u` keyed by `(uk, us)`
//! with two start values per key. Each `t` row then joins the `u` row of its
//! key with the greatest `us <= t.ts`, or none. The oracle states that as a
//! correlated `max(us)` over `u`, never with an ASOF join, so the expansion's
//! `ASOF LEFT JOIN` is checked against an independent formulation. The hop is
//! still many-to-one, so the fence and the per-grain oracle are unchanged.
//!
//! Phase 7.2 makes it a **range** relationship in about a third of the cases
//! instead: `t (ts) REFERENCES u (BETWEEN rs AND re EXCLUSIVE)`, each `u` row
//! a half-open period `[rs, re)`. The periods never overlap (the declared
//! `DISTINCT RANGE` holds), leave gaps, and the first may have no start and
//! the last no end. The oracle gives each `t` row the id of the period holding
//! its `ts` by a correlated `min(id)` with the open bounds written as
//! `COALESCE(.., TRUE)`, never with the expansion's join condition.

//! Phase 7 adds a **second path** to `w` in about half the cases: a diamond
//! `t → x → w` beside `t → u → w`, declared after it, so following the
//! first-declared relationship would go through `u`. A metric on `t` with no
//! `USING` grouped or filtered by `w` must then be refused as multi-path (as
//! Snowflake refuses it, 010246), and so must a query with no metrics that
//! reads `t`'s rows and names `w`. `svu USING (t_u)` and `svx USING (t_x)`
//! sum `t.v` along their own path, which the oracle states by joining `w`
//! through `u` or through `x`. The diamond cases query only the metrics on
//! `t` (see [`metric_exists`]). The diamond is declared with `w` first in
//! about half of those cases and with `t` first in the rest; CREATE accepts
//! both, and each case checks that it does.
//!
//! Phase 7 PR 5b adds `lvu USING (t_u)` and `lvx USING (t_x)` to the diamond
//! cases: `sum(t.v) NON ADDITIVE BY (wcat)`, so their snapshot ranks `t`'s rows
//! by a `w` value read along their own path, whether or not `w` is queried.
//! The oracle keeps, per group, the rows whose `wcat` along that path is the
//! group's greatest (all of them when every one is NULL), by a correlated
//! `max` rather than the expansion's `RANK`. Queried by `wcat` they are plain
//! sums along their path.

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, FactName, MetricName, QueryRequest};
use semantic_views::model::{
    AccessModifier, Cardinality, Dimension, DistinctRange, Join, Metric, SemanticViewDefinition,
    TableRef,
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
    /// Which relationship `t → u` is: plain `t (fk_u) REFERENCES u (id)`,
    /// the ASOF `t (fk_u, ts) REFERENCES u (uk, ASOF us)` (Phase 7.1) or the
    /// range `t (ts) REFERENCES u (BETWEEN rs AND re EXCLUSIVE)` (Phase 7.2).
    hop: Hop,
    /// Per `u` row, the raw start offset [`u_key`] turns into `us`.
    u_starts: Vec<i64>,
    /// Per `t` row, its ASOF value `ts` (missing entries are NULL).
    t_ts: Vec<Option<i64>>,
    /// Phase 7: whether the view has the second path `t → x → w`.
    diamond: bool,
    /// The second path's middle rows: `fk_w` for ids `0..n_x`.
    x_rows: Vec<Option<i64>>,
    /// Per `t` row, its foreign key `fk_x` into `x` (missing entries are NULL).
    t_fk_x: Vec<Option<i64>>,
    /// With the second path, whether `w` (the shared table) is declared first.
    /// CREATE accepts the diamond either way.
    w_first: bool,
}

/// The kind of the `t → u` relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hop {
    Plain,
    Asof,
    Range,
}

/// The period `[rs, re)` of `u` row `i` under the range relationship, `None`
/// an open bound. Row `i` starts at `3i` and ends one to three later, so the
/// periods never overlap and some leave a gap before the next; the first has
/// no start, and the last no end, when their drawn offset says so. A single
/// period can be open at both ends, which a NULL `ts` must still not fall in.
fn u_period(inst: &Instance, i: usize) -> (Option<i64>, Option<i64>) {
    let offset = inst.u_starts.get(i).copied().unwrap_or(0);
    let start = 3 * i as i64;
    let rs = (!(i == 0 && offset == 0)).then_some(start);
    let re = (!(i + 1 == inst.u_rows.len() && offset % 3 == 0)).then_some(start + 1 + offset % 3);
    (rs, re)
}

/// The ASOF key `(uk, us)` of `u` row `i`. Rows `2j` and `2j + 1` share the
/// key `j`, the second starting strictly later, so `(uk, us)` is unique (the
/// declared PRIMARY KEY holds) and a key has one or two periods.
fn u_key(inst: &Instance, i: usize) -> (i64, i64) {
    let start = |i: usize| inst.u_starts.get(i).copied().unwrap_or(0);
    let us = if i % 2 == 0 {
        start(i)
    } else {
        start(i - 1) + 1 + start(i)
    };
    ((i / 2) as i64, us)
}

/// Queryable objects, by stable name. `td`/`ucat`/`wcat` are dimensions at the
/// three grains; `sv`/`ct` are root-grain metrics; `su`/`sw` are the
/// parent/grandparent (ancestor) metrics, each computed at its own grain.
const DIMS: [&str; 3] = ["td", "ucat", "wcat"];
/// `svu` / `svx` (Phase 7, diamond cases only) sum `t.v` like `sv`, with a
/// `USING` naming the first relationship of the path through `u` or `x`.
/// `lvu` / `lvx` (Phase 7 PR 5b, diamond cases only) are the same with
/// `NON ADDITIVE BY (wcat)`.
const METS: [&str; 8] = ["sv", "ct", "su", "sw", "svu", "svx", "lvu", "lvx"];

/// Whether metric `METS[i]` is queried in a view of `inst`'s shape. With the
/// second path `t` references both `u` and `x`, which makes it a bridge
/// between them (Phase 7.3), so a metric on `u` or `w` grouped or filtered by
/// `t` is answered once per group rather than refused; `bridge_proptest`
/// oracles that. The diamond cases therefore query only the metrics on `t`,
/// whose question is which path to `w` they follow.
///
/// With `w` declared first, the source-less `count(*)` (`ct`) counts the base
/// table `w`, and grouped by `t` it would cross the same bridge, so those
/// cases leave it out too.
fn metric_exists(inst: &Instance, i: usize) -> bool {
    if inst.diamond && inst.w_first {
        !matches!(METS[i], "su" | "sw" | "ct")
    } else if inst.diamond {
        !matches!(METS[i], "su" | "sw")
    } else {
        !matches!(METS[i], "svu" | "svx" | "lvu" | "lvx")
    }
}

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
        let u_starts = prop::collection::vec(0i64..4, n_u);
        let ts_cell = prop_oneof![1 => Just(None), 5 => (-1i64..9).prop_map(Some)];
        let t_ts = prop::collection::vec(ts_cell, 20);
        let hop = prop_oneof![Just(Hop::Plain), Just(Hop::Asof), Just(Hop::Range)];
        // The second path: `x` rows reference `w` (NULL, valid or dangling),
        // and each `t` row references an `x` row the same way.
        let x_fk_w = prop_oneof![
            1 => Just(None),
            4 => (0i64..n_w as i64).prop_map(Some),
            1 => Just(Some(n_w as i64)),
        ];
        let x_rows = (1usize..=3).prop_flat_map(move |n_x| {
            let t_fk_x_cell = prop_oneof![
                1 => Just(None),
                4 => (0i64..n_x as i64).prop_map(Some),
                1 => Just(Some(n_x as i64)),
            ];
            (
                prop::collection::vec(x_fk_w.clone(), n_x),
                prop::collection::vec(t_fk_x_cell, 20),
            )
        });
        (
            w_rows,
            u_rows,
            t_rows,
            hop,
            u_starts,
            t_ts,
            any::<bool>(),
            x_rows,
            any::<bool>(),
        )
            .prop_map(
                |(
                    w_rows,
                    u_rows,
                    t_rows,
                    hop,
                    u_starts,
                    t_ts,
                    diamond,
                    (x_rows, t_fk_x),
                    w_first,
                )| {
                    Instance {
                        w_rows,
                        u_rows,
                        t_rows,
                        hop,
                        u_starts,
                        t_ts,
                        diamond,
                        x_rows,
                        t_fk_x,
                        w_first: diamond && w_first,
                    }
                },
            )
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
            .prop_map(|(inst, sel_dims, sel_metrics, where_pred)| {
                let sel_metrics: Vec<usize> = sel_metrics
                    .into_iter()
                    .filter(|&i| metric_exists(&inst, i))
                    .collect();
                (inst, sel_dims, sel_metrics, where_pred)
            })
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
fn build_def(hop: Hop) -> SemanticViewDefinition {
    build_def_shaped(hop, false, false)
}

/// [`build_def`], plus (with `diamond`) the second path `t → x → w` and the
/// two `USING` metrics, `svu` along `t_u` and `svx` along `t_x`. With
/// `w_first`, `w` is declared first, so it is the base table.
fn build_def_shaped(hop: Hop, diamond: bool, w_first: bool) -> SemanticViewDefinition {
    let asof = hop == Hop::Asof;
    let table = |alias: &str, pk: &[&str]| TableRef {
        alias: alias.to_string(),
        table: alias.to_string(),
        definition: None,
        pk_columns: pk.iter().map(|s| (*s).to_string()).collect(),
        unique_constraints: vec![],
        comment: None,
        synonyms: vec![],
        tags: Vec::new(),
        distinct_ranges: vec![],
    };
    // `t` is listed first: base_table() == the first declared table, and the
    // FROM is anchored there with LEFT JOINs outward along the chain.
    let u_pk: &[&str] = if asof { &["uk", "us"] } else { &["id"] };
    let mut tables = vec![table("t", &[]), table("u", u_pk), table("w", &["id"])];
    if diamond {
        tables.push(table("x", &["id"]));
    }
    if hop == Hop::Range {
        tables[1].distinct_ranges = vec![DistinctRange {
            name: "u_period".to_string(),
            start: "rs".to_string(),
            end: "re".to_string(),
        }];
    }
    if w_first {
        // `w, t, u, x`: the diamond's shared table is the base table.
        tables.swap(0, 2);
        tables.swap(1, 2);
    }
    let dim = |name: &str, expr: &str, source: &str| Dimension {
        cortex_search_service: None,
        is_enum: false,
        sample_values: Vec::new(),
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(source.to_string()),
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: false,
        tags: Vec::new(),
    };
    // PBT-6: the three filter members (`LABELS = (FILTER)`), one per grain.
    // Never selected as output — they exist to be named by a generated
    // `where_clause`, and their compound expressions make the member splice's
    // parenthesization load-bearing.
    let filter_dim = |name: &str, expr: &str, source: &str| Dimension {
        cortex_search_service: None,
        is_enum: false,
        sample_values: Vec::new(),
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(source.to_string()),
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: true,
        tags: Vec::new(),
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
        tags: Vec::new(),
    };
    let mut metrics = vec![
        base_metric("sv", "sum(t.v)", Some("t")),
        base_metric("ct", "count(*)", None),
        base_metric("su", "sum(u.uw)", Some("u")),
        base_metric("sw", "sum(w.ww)", Some("w")),
    ];
    if diamond {
        for (name, rel) in [("svu", "t_u"), ("svx", "t_x")] {
            let mut m = base_metric(name, "sum(t.v)", Some("t"));
            m.using_relationships = vec![rel.to_string()];
            metrics.push(m);
        }
        for (name, rel) in [("lvu", "t_u"), ("lvx", "t_x")] {
            let mut m = base_metric(name, "sum(t.v)", Some("t"));
            m.using_relationships = vec![rel.to_string()];
            m.non_additive_by = vec![semantic_views::model::NonAdditiveDim {
                dimension: "wcat".to_string(),
                ..Default::default()
            }];
            metrics.push(m);
        }
    }
    let mut joins = vec![
        if hop == Hop::Range {
            Join {
                from_alias: "t".to_string(),
                table: "u".to_string(),
                fk_columns: vec!["ts".to_string()],
                ref_columns: vec!["rs".to_string()],
                name: Some("t_u".to_string()),
                cardinality: Cardinality::ManyToOne,
                asof: None,
                range_end: Some("re".to_string()),
            }
        } else if asof {
            Join {
                from_alias: "t".to_string(),
                table: "u".to_string(),
                fk_columns: vec!["fk_u".to_string(), "ts".to_string()],
                ref_columns: vec!["uk".to_string(), "us".to_string()],
                name: Some("t_u".to_string()),
                cardinality: Cardinality::ManyToOne,
                asof: Some(1),
                range_end: None,
            }
        } else {
            Join {
                from_alias: "t".to_string(),
                table: "u".to_string(),
                fk_columns: vec!["fk_u".to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some("t_u".to_string()),
                cardinality: Cardinality::ManyToOne,
                asof: None,
                range_end: None,
            }
        },
        Join {
            from_alias: "u".to_string(),
            table: "w".to_string(),
            fk_columns: vec!["fk_w".to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some("u_w".to_string()),
            cardinality: Cardinality::ManyToOne,
            asof: None,
            range_end: None,
        },
    ];
    if diamond {
        // Declared after the `u` path, so the first-declared path to `w` is
        // the one through `u`, which the expansion used to follow silently.
        for (name, from, to, fk) in [("t_x", "t", "x", "fk_x"), ("x_w", "x", "w", "fk_w")] {
            joins.push(Join {
                from_alias: from.to_string(),
                table: to.to_string(),
                fk_columns: vec![fk.to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some(name.to_string()),
                cardinality: Cardinality::ManyToOne,
                asof: None,
                range_end: None,
            });
        }
    }
    SemanticViewDefinition {
        variables: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
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
        tags: Vec::new(),
    }
}

/// Create the physical tables and insert the generated rows.
fn make_db(inst: &Instance) -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE w (id INTEGER, wcat INTEGER, ww INTEGER); \
         CREATE TABLE u (id INTEGER, uk INTEGER, us INTEGER, rs INTEGER, re INTEGER, fk_w INTEGER, ucat INTEGER, uw INTEGER); \
         CREATE TABLE x (id INTEGER, fk_w INTEGER); \
         CREATE TABLE t (fk_u INTEGER, ts INTEGER, d INTEGER, v INTEGER, fk_x INTEGER);",
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
                let (uk, us) = u_key(inst, i);
                let (rs, re) = u_period(inst, i);
                format!(
                    "({i},{uk},{us},{},{},{},{},{})",
                    cell(&rs),
                    cell(&re),
                    cell(fk_w),
                    cell(ucat),
                    cell(uw)
                )
            })
            .collect();
        conn.execute_batch(&format!("INSERT INTO u VALUES {};", values.join(",")))
            .expect("insert u rows");
    }
    if !inst.x_rows.is_empty() {
        let values: Vec<String> = inst
            .x_rows
            .iter()
            .enumerate()
            .map(|(i, fk_w)| format!("({i},{})", cell(fk_w)))
            .collect();
        conn.execute_batch(&format!("INSERT INTO x VALUES {};", values.join(",")))
            .expect("insert x rows");
    }
    if !inst.t_rows.is_empty() {
        let values: Vec<String> = inst
            .t_rows
            .iter()
            .enumerate()
            .map(|(i, (fk_u, d, v))| {
                let ts = inst.t_ts.get(i).copied().flatten();
                let fk_x = inst.t_fk_x.get(i).copied().flatten();
                format!(
                    "({},{},{},{},{})",
                    cell(fk_u),
                    cell(&ts),
                    cell(d),
                    cell(v),
                    cell(&fk_x)
                )
            })
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
        "sv" | "ct" | "svu" | "svx" | "lvu" | "lvx" => "t",
        "su" => "u",
        "sw" => "w",
        other => unreachable!("unexpected metric {other}"),
    }
}

/// The oracle's grain for a metric: its table, except that `svx` reads `t`
/// with `w` joined through `x`, a grain of its own (`tx`), and the snapshots
/// `lvu` / `lvx` are grains of their own (`tlu`, `tlx`).
fn grain_key(metric: &str) -> &'static str {
    match metric {
        "svx" => "tx",
        "lvu" => "tlu",
        "lvx" => "tlx",
        other => metric_table(other),
    }
}

/// `FROM t` joined up to `w`, through `u` or (`via_x`) through `x`. `u` is
/// joined either way, for `ucat`.
///
/// Phase 7.1: under the ASOF relationship each `t` row's period is the
/// greatest `us` of its key at or before `ts`, found by a correlated `max`,
/// never by an ASOF join; `t` then joins that one `u` row by equality.
/// Phase 7.2: under the range relationship each `t` row's period is the one
/// holding its `ts`, found by id with a correlated `min`.
fn t_path_from(case: &Case, via_x: bool) -> String {
    let t_from = match case.inst.hop {
        Hop::Asof => {
            "FROM (SELECT t.*, (SELECT max(u2.us) FROM u u2 WHERE u2.uk = t.fk_u AND u2.us <= t.ts) \
             AS asof_us FROM t) t LEFT JOIN u ON t.fk_u = u.uk AND t.asof_us = u.us \
             LEFT JOIN w ON u.fk_w = w.id"
        }
        Hop::Range => {
            "FROM (SELECT t.*, (SELECT min(u2.id) FROM u u2 WHERE t.ts IS NOT NULL \
             AND COALESCE(u2.rs <= t.ts, TRUE) AND COALESCE(t.ts < u2.re, TRUE)) AS range_id \
             FROM t) t LEFT JOIN u ON u.id = t.range_id LEFT JOIN w ON u.fk_w = w.id"
        }
        Hop::Plain => "FROM t LEFT JOIN u ON t.fk_u = u.id LEFT JOIN w ON u.fk_w = w.id",
    };
    // Phase 7: through `x` the `w` join is replaced.
    if via_x {
        t_from.replace(
            "LEFT JOIN w ON u.fk_w = w.id",
            "LEFT JOIN x ON t.fk_x = x.id LEFT JOIN w ON x.fk_w = w.id",
        )
    } else {
        t_from.to_string()
    }
}

/// A snapshot grain's half of the oracle (Phase 7 PR 5b): `lvu` (`tlu`) or
/// `lvx` (`tlx`), `sum(t.v) NON ADDITIVE BY (wcat)` along its path. The
/// filtered rows of `t`, each with its `wcat` along that path; then, per
/// group of the queried dimensions, the rows whose `wcat` is the group's
/// greatest, or every row when all are NULL (`NON ADDITIVE BY` ascending,
/// NULLs last: the latest non-NULL snapshot). Grouped by `wcat` the metric is
/// a plain sum.
fn snapshot_grain_sql(case: &Case, anchor: &str) -> String {
    let (via_x, metric) = match anchor {
        "tlu" => (false, "lvu"),
        "tlx" => (true, "lvx"),
        other => unreachable!("{other} is not a snapshot grain"),
    };
    let dims: Vec<&str> = case.sel_dims.iter().map(|&i| DIMS[i]).collect();
    let raw = |d: &str| match d {
        "td" => "t.d",
        "ucat" => "u.ucat",
        "wcat" => "w.wcat",
        other => unreachable!("unexpected dim {other}"),
    };
    let row_items: String = dims
        .iter()
        .map(|d| format!("{} AS {d}, ", raw(d)))
        .collect();
    let where_sql = case
        .where_pred
        .as_ref()
        .map_or_else(String::new, |p| format!(" WHERE {}", p.to_raw_sql()));
    let rows = format!(
        "SELECT {row_items}t.v AS v, w.wcat AS na {}{where_sql}",
        t_path_from(case, via_x)
    );
    let snapshot = if dims.contains(&"wcat") {
        "TRUE".to_string()
    } else {
        let same_group: Vec<String> = dims
            .iter()
            .map(|d| format!("r2.{d} IS NOT DISTINCT FROM r.{d}"))
            .collect();
        let same_group = if same_group.is_empty() {
            "TRUE".to_string()
        } else {
            same_group.join(" AND ")
        };
        format!(
            "r.na IS NOT DISTINCT FROM (SELECT max(r2.na) FROM snap_rows r2 WHERE {same_group})"
        )
    };
    let items: Vec<String> = dims
        .iter()
        .map(|d| format!("r.{d} AS {d}"))
        .chain(std::iter::once(format!("sum(r.v) AS {metric}")))
        .collect();
    let group_by = if dims.is_empty() {
        String::new()
    } else {
        format!(
            " GROUP BY {}",
            (1..=dims.len())
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "WITH snap_rows AS ({rows}) SELECT {} FROM snap_rows r WHERE {snapshot}{group_by}",
        items.join(", ")
    )
}

/// One grain's half of the oracle: the metrics that live on `anchor`, computed
/// over `anchor`'s own rows and grouped by the selected dimensions, which are
/// reached by chaining LEFT JOINs **upward** from the anchor.
///
/// This is the independent statement of "at its own grain": `su` is summed over
/// `u` itself, never over `t LEFT JOIN u` (where each parent row appears once
/// per child row and childless parents vanish).
fn grain_sql(case: &Case, anchor: &str) -> String {
    if fans_below(case, anchor) {
        return fanned_grain_sql(case, anchor);
    }
    if matches!(anchor, "tlu" | "tlx") {
        return snapshot_grain_sql(case, anchor);
    }
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
        .filter(|&&i| grain_key(METS[i]) == anchor)
        .map(|&i| match METS[i] {
            "sv" => "sum(t.v) AS sv".to_string(),
            "svu" => "sum(t.v) AS svu".to_string(),
            "svx" => "sum(t.v) AS svx".to_string(),
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
    // row multiset. The `tx` grain reaches `w` through `x` instead.
    let from = match anchor {
        "t" => t_path_from(case, false),
        "tx" => t_path_from(case, true),
        "u" => "FROM u LEFT JOIN w ON u.fk_w = w.id".to_string(),
        "w" => "FROM w".to_string(),
        other => unreachable!("unexpected anchor {other}"),
    };
    // PBT-6: the predicate is evaluated inside EVERY grain's CTE, filtering the
    // rows that feed that grain's aggregate. Predicate members at or above the
    // anchor are reachable along the up-chain already joined above; members
    // BELOW it fan the anchor, and `fanned_grain_sql` answers that grain.
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

/// One fanned-out grain's half of the oracle (Phase 7 PR 4c): `anchor` is `u`
/// or `w`, and a queried dimension or `where_clause` member sits below it.
/// Snowflake counts each anchor row once in every group it reaches (fixture 7,
/// N1-N7), so the anchor's rows are joined DOWN to their descendants (an
/// anchor row with none kept once, NULL-extended), filtered by the predicate,
/// and cut to the DISTINCT (anchor row, group) pairs before the sum. The
/// expansion keys its deduplicating subquery on the anchor's PRIMARY KEY and
/// joins it back; this states the same answer as one flat DISTINCT. A fan-out
/// to `t` gets here only over the plain hop: across the temporal one it is
/// refused first. `w` fanned out to `u` alone needs no `t` at all.
fn fanned_grain_sql(case: &Case, anchor: &str) -> String {
    // `t` is joined only when a member is on it, and then the hop is plain.
    let to_t = if member_tables(case).contains(&"t") {
        assert_eq!(
            case.inst.hop,
            Hop::Plain,
            "a temporal fan-out is refused before the oracle"
        );
        " LEFT JOIN t ON t.fk_u = u.id"
    } else {
        ""
    };
    let (input, from, metric) = match anchor {
        "u" => (
            "u.uw AS uw",
            format!("FROM u LEFT JOIN w ON u.fk_w = w.id{to_t}"),
            "sum(f.uw) AS su",
        ),
        "w" => (
            "w.ww AS ww",
            format!("FROM w LEFT JOIN u ON u.fk_w = w.id{to_t}"),
            "sum(f.ww) AS sw",
        ),
        other => unreachable!("{other} has nothing above it to fan out from"),
    };
    let dims: Vec<&str> = case.sel_dims.iter().map(|&i| DIMS[i]).collect();
    let dim_cols: String = dims
        .iter()
        .map(|d| match *d {
            "td" => ", t.d AS td",
            "ucat" => ", u.ucat AS ucat",
            "wcat" => ", w.wcat AS wcat",
            other => unreachable!("unexpected dim {other}"),
        })
        .collect();
    let where_sql = case
        .where_pred
        .as_ref()
        .map_or_else(String::new, |p| format!(" WHERE {}", p.to_raw_sql()));
    let pairs = format!("SELECT DISTINCT {anchor}.id AS rid, {input}{dim_cols} {from}{where_sql}");
    let has_metric = case
        .sel_metrics
        .iter()
        .any(|&i| grain_key(METS[i]) == anchor);
    let items: Vec<String> = dims
        .iter()
        .map(|d| format!("f.{d} AS {d}"))
        .chain(has_metric.then(|| metric.to_string()))
        .collect();
    if dims.is_empty() {
        format!("SELECT {} FROM ({pairs}) f", items.join(", "))
    } else {
        let group_by = (1..=dims.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "SELECT {} FROM ({pairs}) f GROUP BY {group_by}",
            items.join(", ")
        )
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
        let table = grain_key(METS[i]);
        if !anchors.contains(&table) {
            anchors.push(table);
        }
    }
    if anchors.is_empty() {
        // Dimensions-only: the rows of the lowest table any queried dimension
        // or filter member lives on, joined up from there (Snowflake's answer
        // in the 2026-11-09 dims-only fixture). `ucat` alone lists `u`'s own
        // values, the `u` rows no `t` row reaches included and no NULL for an
        // orphan `t` row.
        let mut members: Vec<&str> = case.sel_dims.iter().map(|&d| dim_table(DIMS[d])).collect();
        if let Some(p) = &case.where_pred {
            p.tables(&mut members);
        }
        let lowest = members.into_iter().min_by_key(|t| level(t)).unwrap_or("t");
        anchors.push(lowest);
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
            .position(|a| *a == grain_key(metric))
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
    let def = build_def_shaped(case.inst.hop, case.inst.diamond, case.inst.w_first);
    // CREATE accepts the diamond in either TABLES order, as Snowflake does
    // (it used to refuse it unless `w`, its shared table, came first).
    let creatable = semantic_views::graph::validate_graph(&def);
    prop_assert!(
        creatable.is_ok(),
        "CREATE's graph rule: {:?}",
        creatable.err().map(|e| e.to_string())
    );
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

    let result = expand("multihop", &def, &req);

    // Phase 7: with the second path, a query that reads `t`'s rows and names
    // `w` has two paths to it. It is refused, before anything else, unless
    // every metric reading `t` names its path in a `USING`.
    if reaches_w_two_ways(case) {
        match result {
            Err(e) => {
                let msg = e.to_string();
                prop_assert!(
                    msg.contains("by more than one path"),
                    "a query with two paths to w was rejected, but not as multi-path: {msg}"
                );
            }
            Ok(sql) => prop_assert!(
                false,
                "a query with two paths to w must be refused, got SQL:\n{sql}"
            ),
        }
        return Ok(());
    }

    // Phase 7 PR 4c: a dimension or `where_clause` member BELOW some selected
    // metric's grain fans that metric out. It is answered once per group
    // (`fanned_grain_sql`), except across the ASOF or range hop walked
    // backwards: a metric on `u` or `w` reaching `t` when that hop is
    // temporal. That stays refused as a fan trap.
    if fans_across_a_temporal_hop(case) {
        match result {
            Err(e) => {
                let msg = e.to_string();
                prop_assert!(
                    msg.contains("fan trap"),
                    "a fan-out across the temporal hop was rejected, but not as a fan trap: {msg}"
                );
            }
            Ok(sql) => prop_assert!(
                false,
                "a fan-out across the temporal hop must be refused, got SQL:\n{sql}"
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

/// Whether `case` reaches `w` from `t` along a path it does not choose: the
/// view has the second path, a dimension or filter member is on `w`, and some
/// selected metric reads `t` with no `USING` (`sv`, `ct`), or there is no
/// metric and the lowest member is on `t`, so the query reads `t`'s rows.
fn reaches_w_two_ways(case: &Case) -> bool {
    if !case.inst.diamond {
        return false;
    }
    let mut members: Vec<&str> = case.sel_dims.iter().map(|&d| dim_table(DIMS[d])).collect();
    if let Some(p) = &case.where_pred {
        p.tables(&mut members);
    }
    if !members.contains(&"w") {
        return false;
    }
    if case.sel_metrics.is_empty() {
        return members.contains(&"t");
    }
    case.sel_metrics
        .iter()
        .any(|&m| matches!(METS[m], "sv" | "ct"))
}

/// The tables the query's dimensions and `where_clause` members live on.
fn member_tables(case: &Case) -> Vec<&'static str> {
    let mut members: Vec<&'static str> =
        case.sel_dims.iter().map(|&d| dim_table(DIMS[d])).collect();
    if let Some(p) = &case.where_pred {
        p.tables(&mut members);
    }
    members
}

/// Whether some member sits below `anchor`'s grain, so the anchor's metrics
/// fan out to reach it.
fn fans_below(case: &Case, anchor: &str) -> bool {
    matches!(anchor, "u" | "w")
        && member_tables(case)
            .iter()
            .any(|&t| level(t) < level(anchor))
}

/// Whether a metric on `u` or `w` reaches `t` across the ASOF or range hop
/// walked backwards: the fan-out Snowflake refuses (fixture 1's A5 for ASOF,
/// fixture 8's F1 and F2 for range).
fn fans_across_a_temporal_hop(case: &Case) -> bool {
    case.inst.hop != Hop::Plain
        && member_tables(case).contains(&"t")
        && case
            .sel_metrics
            .iter()
            .any(|&m| matches!(metric_table(METS[m]), "u" | "w"))
}

/// Phase 7 guard: the diamond cases reach both outcomes, a refusal and an
/// answer along a chosen path (`svu` / `svx` beside a member on `w`), and the
/// two paths give different numbers in some of them, so the oracle comparison
/// is not vacuous.
#[test]
fn generator_reaches_both_diamond_branches() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let (mut refused, mut chosen, mut plain, mut differ) = (0usize, 0usize, 0usize, 0usize);
    // The same two outcomes with `w` declared first.
    let (mut refused_w_first, mut chosen_w_first) = (0usize, 0usize);
    // Phase 7 PR 5b: a snapshot along its path, active (`wcat` not queried),
    // with `w` named by no member (only the snapshot reads it), and filtered.
    let (mut snap_active, mut snap_unnamed, mut snap_filtered) = (0usize, 0usize, 0usize);
    for _ in 0..400 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        if !case.inst.diamond {
            plain += 1;
            continue;
        }
        if reaches_w_two_ways(&case) {
            refused += 1;
            refused_w_first += usize::from(case.inst.w_first);
            continue;
        }
        let names_w = case.sel_dims.iter().any(|&d| DIMS[d] == "wcat")
            || case.where_pred.as_ref().is_some_and(|p| {
                let mut tables = Vec::new();
                p.tables(&mut tables);
                tables.contains(&"w")
            });
        let snapshot = case
            .sel_metrics
            .iter()
            .any(|&m| matches!(METS[m], "lvu" | "lvx"));
        if snapshot && !case.sel_dims.iter().any(|&d| DIMS[d] == "wcat") {
            snap_active += 1;
            snap_unnamed += usize::from(!names_w);
            snap_filtered += usize::from(case.where_pred.is_some());
        }
        if names_w
            && case
                .sel_metrics
                .iter()
                .any(|&m| matches!(METS[m], "svu" | "svx"))
        {
            chosen += 1;
            chosen_w_first += usize::from(case.inst.w_first);
            // Whether the two paths give different sums by `wcat` here (the
            // `u` path read by plain key, a close enough stand-in for this).
            let conn = make_db(&case.inst);
            let diff: i64 = conn
                .query_row(
                    "SELECT count(*) FROM (\
                       (SELECT w.wcat, sum(t.v) FROM t LEFT JOIN u ON t.fk_u = u.id \
                        LEFT JOIN w ON u.fk_w = w.id GROUP BY 1) \
                       EXCEPT ALL \
                       (SELECT w.wcat, sum(t.v) FROM t LEFT JOIN x ON t.fk_x = x.id \
                        LEFT JOIN w ON x.fk_w = w.id GROUP BY 1))",
                    [],
                    |r| r.get(0),
                )
                .expect("path comparison runs");
            differ += usize::from(diff > 0);
        }
    }
    assert!(plain > 0, "no case without the second path");
    assert!(
        snap_active >= 20 && snap_unnamed >= 10 && snap_filtered >= 10,
        "the snapshot along a USING path is too rare: {snap_active} active, \
         {snap_unnamed} with w named only by NON ADDITIVE BY, {snap_filtered} filtered"
    );
    assert!(
        differ > 0,
        "the two paths to w never give different sums; following the wrong one \
         would go unnoticed"
    );
    assert!(refused > 0, "no diamond case is refused as multi-path");
    assert!(
        chosen > 0,
        "no diamond case follows a USING path to w; the path oracle is never compared"
    );
    assert!(
        refused_w_first > 0 && chosen_w_first > 0,
        "the diamond declared w-first (the shape CREATE accepts) misses an outcome: \
         {refused_w_first} refused, {chosen_w_first} answered along a USING path"
    );
    assert!(
        refused > refused_w_first && chosen > chosen_w_first,
        "the diamond declared t-first never reaches both outcomes"
    );
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
    // Phase 7 PR 4c: fan-outs answered once per group (the plain hop), and
    // refused across the temporal one.
    let (mut fanned_answered, mut fanned_refused) = (0usize, 0usize);
    let mut distinct: HashSet<String> = HashSet::new();

    for _ in 0..400 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        if !reaches_w_two_ways(&case) {
            if fans_across_a_temporal_hop(&case) {
                fanned_refused += 1;
            } else if ["u", "w"].iter().any(|a| {
                case.sel_metrics
                    .iter()
                    .any(|&m| metric_table(METS[m]) == *a)
                    && fans_below(&case, a)
            }) {
                fanned_answered += 1;
            }
        }
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
         once-per-group oracle for that branch is unreachable and therefore vacuous"
    );
    assert!(
        fanned_answered >= 40 && fanned_refused >= 20,
        "fan-outs answered {fanned_answered}, refused across the temporal hop \
         {fanned_refused}: one branch is barely reached"
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
        hop: Hop::Plain,
        u_starts: vec![],
        t_ts: vec![],
        diamond: false,
        w_first: false,
        x_rows: vec![],
        t_fk_x: vec![],
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

/// Phase 7.1: a fixed ASOF instance with the answer written out, so the
/// generated ASOF cases are known to compare real matches rather than an
/// oracle and an expansion that agree about NULLs. `u` rows 0 and 1 share key
/// 0 and start at 2 and 4: `ts` 1 matches neither, 2 and 3 match the first, 4
/// and 9 the second, and a NULL `ts` none.
#[test]
fn pinned_asof_hop_takes_the_latest_period() {
    let inst = Instance {
        w_rows: vec![(Some(0), Some(1))],
        u_rows: vec![(None, Some(0), Some(10)), (None, Some(1), Some(20))],
        t_rows: [1, 2, 4, 8, 16, 32]
            .iter()
            .map(|&v| (Some(0), Some(0), Some(v)))
            .collect(),
        hop: Hop::Asof,
        u_starts: vec![2, 1],
        t_ts: vec![Some(1), Some(2), Some(3), Some(4), Some(9), None],
        diamond: false,
        w_first: false,
        x_rows: vec![],
        t_fk_x: vec![],
    };
    let case = Case {
        inst: inst.clone(),
        sel_dims: vec![1],
        sel_metrics: vec![0],
        where_pred: None,
    };
    check_case(&case).unwrap();

    let req = QueryRequest {
        // PIN: a fixed query with its answer written out; the property above varies the filter.
        where_clause: None,
        dimensions: vec![DimensionName::new("ucat")],
        metrics: vec![MetricName::new("sv")],
        // PIN: FACTS is a row-level shape with no per-grain meaning. TECH-DEBT #66
        facts: vec![],
    };
    let sql = expand("multihop", &build_def(Hop::Asof), &req).unwrap();
    let conn = make_db(&inst);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT CAST(ucat AS BIGINT), CAST(sv AS BIGINT) FROM ({sql}) ORDER BY 1 NULLS LAST"
        ))
        .unwrap();
    let rows: Vec<(Option<i64>, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![(Some(0), Some(6)), (Some(1), Some(24)), (None, Some(33))],
        "{sql}"
    );
}

/// A dimension that names no table, always true: a filter on it keeps a query
/// on the base table (`member_anchor` cannot place its rows).
fn unplaced_dimension() -> Dimension {
    Dimension {
        name: "always".to_string(),
        expr: "TRUE".to_string(),
        source_table: None,
        ..Default::default()
    }
}

/// Phase 7.1: the ASOF hop walked against its direction. With `u` listed
/// first and a filter on a member that names no table, a FACTS list starts
/// from `u` and joins `t` onto it, so each `u` row
/// must get exactly the `t` rows whose ASOF match it is, one output row per
/// pair (a `u` row with none keeps one row with NULL `t` columns). The
/// oracle finds each `t` row's match by the same correlated `max` as
/// [`grain_sql`] and joins it to `u` by equality.
fn reverse_asof_oracle(case: &Case) -> String {
    let dims: Vec<String> = case
        .sel_dims
        .iter()
        .map(|&i| match DIMS[i] {
            "td" => "t.d AS td".to_string(),
            "ucat" => "u.ucat AS ucat".to_string(),
            "wcat" => "w.wcat AS wcat".to_string(),
            other => unreachable!("unexpected dim {other}"),
        })
        .collect();
    // `t` is joined only when `td` is asked for, as the expansion does: a
    // FACTS list keeps one row per row of the joined set, so an unasked
    // `t` would multiply the `u` rows.
    let t_join = if case.sel_dims.contains(&0) {
        "LEFT JOIN (SELECT t.*, (SELECT max(u2.us) FROM u u2 WHERE u2.uk = t.fk_u \
                    AND u2.us <= t.ts) AS asof_us FROM t) t \
           ON t.fk_u = u.uk AND t.asof_us = u.us "
    } else {
        ""
    };
    format!(
        "SELECT {} FROM u {t_join}LEFT JOIN w ON u.fk_w = w.id",
        dims.join(", ")
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn asof_hop_walked_backwards_matches_oracle(
        inst in arb_instance(),
        sel_dims in prop::sample::subsequence(vec![0usize, 1, 2], 1..=3),
    ) {
        let mut inst = inst;
        inst.hop = Hop::Asof;
        let case = Case { inst, sel_dims, sel_metrics: vec![], where_pred: None };
        let mut def = build_def(Hop::Asof);
        def.tables.swap(0, 1); // `u` first: the base table, so `t` is joined onto it.
        def.dimensions.push(unplaced_dimension());
        let req = QueryRequest {
            // A query reads the table under its members (`t` whenever `td` is
            // asked for), so the one shape left that walks the hop backwards is
            // a filter on a member that names no table: its rows can't be
            // placed, so the query reads the base table's. The filter is always
            // true, so it changes no row.
            where_clause: Some("always".to_string()),
            dimensions: vec![],
            metrics: vec![],
            facts: case.sel_dims.iter().map(|&i| FactName::new(DIMS[i])).collect(),
        };
        let expanded = expand("multihop", &def, &req)
            .map_err(|e| TestCaseError::fail(format!("rejected: {e}")))?;
        // `t` is joined only when one of its dimensions is asked for.
        prop_assert_eq!(
            expanded.contains("ASOF JOIN"),
            case.sel_dims.contains(&0),
            "the reverse ASOF join is emitted exactly when `td` is queried:\n{}",
            expanded
        );
        let oracle = reverse_asof_oracle(&case);
        let proj = case.sel_dims.iter().map(|&i| DIMS[i]).collect::<Vec<_>>().join(", ");
        let cmp = format!(
            "SELECT \
               (SELECT count(*) FROM (SELECT {proj} FROM ({expanded}) qa EXCEPT ALL \
                                      SELECT {proj} FROM ({oracle}) qb) e1) \
             + (SELECT count(*) FROM (SELECT {proj} FROM ({oracle}) qc EXCEPT ALL \
                                      SELECT {proj} FROM ({expanded}) qd) e2)"
        );
        let conn = make_db(&case.inst);
        let diff: i64 = conn.query_row(&cmp, [], |r| r.get(0)).unwrap_or_else(|e| {
            panic!("comparison failed: {e}\n--- expanded:\n{expanded}\n--- oracle:\n{oracle}")
        });
        prop_assert_eq!(diff, 0, "--- expanded:\n{}\n--- oracle:\n{}", expanded, oracle);
    }
}

/// Phase 7.2: a fixed range instance with the answer written out. `u` rows
/// are the periods `[NULL, 1)`, `[3, 5)` and `[6, NULL)`: `ts` -1 falls in the
/// first, 3 in the second (its start), 6 and 9 in the third; 1 and 5 (ends)
/// and a NULL `ts` fall in none.
#[test]
fn pinned_range_hop_takes_the_period_holding_the_value() {
    let inst = Instance {
        w_rows: vec![(Some(0), Some(1))],
        u_rows: vec![
            (None, Some(0), Some(10)),
            (None, Some(1), Some(20)),
            (None, Some(2), Some(30)),
        ],
        t_rows: [1, 2, 4, 8, 16, 32, 64]
            .iter()
            .map(|&v| (None, Some(0), Some(v)))
            .collect(),
        hop: Hop::Range,
        u_starts: vec![0, 1, 3],
        t_ts: vec![Some(-1), Some(1), Some(3), Some(5), Some(6), Some(9), None],
        diamond: false,
        w_first: false,
        x_rows: vec![],
        t_fk_x: vec![],
    };
    assert_eq!(u_period(&inst, 0), (None, Some(1)));
    assert_eq!(u_period(&inst, 1), (Some(3), Some(5)));
    assert_eq!(u_period(&inst, 2), (Some(6), None));
    let case = Case {
        inst: inst.clone(),
        sel_dims: vec![1],
        sel_metrics: vec![0],
        where_pred: None,
    };
    check_case(&case).unwrap();

    let req = QueryRequest {
        // PIN: a fixed query with its answer written out; the property above varies the filter.
        where_clause: None,
        dimensions: vec![DimensionName::new("ucat")],
        metrics: vec![MetricName::new("sv")],
        // PIN: FACTS is a row-level shape with no per-grain meaning. TECH-DEBT #66
        facts: vec![],
    };
    let sql = expand("multihop", &build_def(Hop::Range), &req).unwrap();
    let conn = make_db(&inst);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT CAST(ucat AS BIGINT), CAST(sv AS BIGINT) FROM ({sql}) ORDER BY 1 NULLS LAST"
        ))
        .unwrap();
    let rows: Vec<(Option<i64>, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![
            (Some(0), Some(1)),
            (Some(1), Some(4)),
            (Some(2), Some(48)),
            (None, Some(74))
        ],
        "{sql}"
    );
}

/// Phase 7.2: the range hop walked against its direction, as
/// [`reverse_asof_oracle`] for ASOF: with `u` first, each period gets exactly
/// the `t` rows it holds, found by the same correlated `min(id)` as
/// [`grain_sql`].
fn reverse_range_oracle(case: &Case) -> String {
    let dims: Vec<String> = case
        .sel_dims
        .iter()
        .map(|&i| match DIMS[i] {
            "td" => "t.d AS td".to_string(),
            "ucat" => "u.ucat AS ucat".to_string(),
            "wcat" => "w.wcat AS wcat".to_string(),
            other => unreachable!("unexpected dim {other}"),
        })
        .collect();
    // `t` is joined only when `td` is asked for, as the expansion does: a
    // FACTS list keeps one row per row of the joined set, so an unasked
    // `t` would multiply the `u` rows.
    let t_join = if case.sel_dims.contains(&0) {
        "LEFT JOIN (SELECT t.*, (SELECT min(u2.id) FROM u u2 WHERE t.ts IS NOT NULL \
                    AND COALESCE(u2.rs <= t.ts, TRUE) AND COALESCE(t.ts < u2.re, TRUE)) \
                    AS range_id FROM t) t \
           ON t.range_id = u.id "
    } else {
        ""
    };
    format!(
        "SELECT {} FROM u {t_join}LEFT JOIN w ON u.fk_w = w.id",
        dims.join(", ")
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn range_hop_walked_backwards_matches_oracle(
        inst in arb_instance(),
        sel_dims in prop::sample::subsequence(vec![0usize, 1, 2], 1..=3),
    ) {
        let mut inst = inst;
        inst.hop = Hop::Range;
        let case = Case { inst, sel_dims, sel_metrics: vec![], where_pred: None };
        let mut def = build_def(Hop::Range);
        def.tables.swap(0, 1); // `u` first: the base table, so `t` is joined onto it.
        def.dimensions.push(unplaced_dimension());
        let req = QueryRequest {
            // A query reads the table under its members (`t` whenever `td` is
            // asked for), so the one shape left that walks the hop backwards is
            // a filter on a member that names no table: its rows can't be
            // placed, so the query reads the base table's. The filter is always
            // true, so it changes no row.
            where_clause: Some("always".to_string()),
            dimensions: vec![],
            metrics: vec![],
            facts: case.sel_dims.iter().map(|&i| FactName::new(DIMS[i])).collect(),
        };
        let expanded = expand("multihop", &def, &req)
            .map_err(|e| TestCaseError::fail(format!("rejected: {e}")))?;
        let oracle = reverse_range_oracle(&case);
        let proj = case.sel_dims.iter().map(|&i| DIMS[i]).collect::<Vec<_>>().join(", ");
        let cmp = format!(
            "SELECT \
               (SELECT count(*) FROM (SELECT {proj} FROM ({expanded}) qa EXCEPT ALL \
                                      SELECT {proj} FROM ({oracle}) qb) e1) \
             + (SELECT count(*) FROM (SELECT {proj} FROM ({oracle}) qc EXCEPT ALL \
                                      SELECT {proj} FROM ({expanded}) qd) e2)"
        );
        let conn = make_db(&case.inst);
        let diff: i64 = conn.query_row(&cmp, [], |r| r.get(0)).unwrap_or_else(|e| {
            panic!("comparison failed: {e}\n--- expanded:\n{expanded}\n--- oracle:\n{oracle}")
        });
        prop_assert_eq!(diff, 0, "--- expanded:\n{}\n--- oracle:\n{}", expanded, oracle);
    }
}
