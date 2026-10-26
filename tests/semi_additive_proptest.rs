//! Differential proptest for the semi-additive (`NON ADDITIVE BY`) snapshot
//! path (code-review 2026-07-18 PBT-1 / "Top-3 new property #2").
//!
//! `expand_semi_additive` is the crate's longest function and the file where
//! the most recent behavioural bugs landed (the F-1 snapshot-polarity inversion,
//! the #30 dotted/quoted NA-dim resolution), yet the snapshot *math* — which
//! rows a `SUM(x) NON ADDITIVE BY ts` selects, how ties at the snapshot value
//! aggregate, and how NULL timestamps rank — had no randomized coverage. The
//! example tests pin a handful of fixed shapes; this exercises it over random
//! data with **duplicate timestamps** (RANK ties) and **NULLs** in every column.
//!
//! Shape: a single table `s(entity, ts, balance)`, a dimension on `entity` and
//! on `ts`, and a semi-additive metric `bal = SUM(s.balance) NON ADDITIVE BY
//! (ts)`. The declared sort direction is randomized (`ASC`/`DESC`), which flips
//! *which* snapshot is selected — the exact axis the F-1 bug inverted.
//!
//! For every query `expand` accepts, the result must equal an **independently
//! written** oracle, compared as a multiset inside DuckDB via a symmetric
//! `EXCEPT ALL` difference (the same comparator the single-table and star-join
//! harnesses use). The oracle computes the snapshot with `MAX`/`MIN` + `IS NOT
//! DISTINCT FROM` rather than the extension's `RANK() OVER (...)` CTE, so a bug
//! in the RANK formulation (wrong reversal, wrong NULLS placement, ties dropped)
//! surfaces as a non-zero diff.
//!
//! Snapshot semantics being pinned (verified against `semi_additive.rs`, which
//! emits the *reverse* of the declared direction and picks `RANK() = 1`):
//! - **`NON ADDITIVE BY (ts)`** (default `ASC` / `NULLS LAST`): the LATEST
//!   snapshot — rows at `MAX(ts)`; a NULL `ts` is selected only when the whole
//!   partition is NULL.
//! - **`NON ADDITIVE BY (ts DESC)`** (`DESC` / `NULLS FIRST`): the EARLIEST
//!   snapshot — NULL-`ts` rows when any exist (NULLS FIRST), otherwise `MIN(ts)`.
//! - The snapshot is per-partition = the queried dims minus the NA dim; when the
//!   NA dim (`ts`) is itself queried the metric is effectively regular (a plain
//!   `GROUP BY` aggregate), which the oracle mirrors.
//!
//! # PBT-6 / TECH-DEBT #41 — the `where_clause` axis
//!
//! The generated query now also carries a randomized pre-aggregation
//! `where_clause`. This harness is the one that matters most for that
//! parameter, and it is the reason the remaining three were split out of the
//! first pass: here the predicate is applied **before the `RANK`**, inside
//! `__sv_snapshot`, so filtering does not merely reduce what is summed — it
//! changes *which row wins the snapshot*. Filtering away the `MAX(ts)` row
//! promotes the next timestamp to be the snapshot, and the metric reports a
//! different partition's balance entirely.
//!
//! That is what the oracle has to mirror, and why this could not be a copied
//! `WHERE` clause: the predicate is applied to **every** reference to `s`,
//! including the subquery that determines the snapshot timestamp. An oracle
//! that filtered only the outer aggregate would encode post-snapshot filtering
//! — a different query, and one that would agree with a buggy implementation
//! that made the same mistake.
//!
//! A predicate over the NA dimension `ts` is therefore the highest-value shape
//! here, and the generator-coverage guard asserts it is actually produced.
//! Filter members (`LABELS = (FILTER)`) with compound `OR` expressions are
//! declared so member substitution is not identity and its parenthesization is
//! load-bearing, matching the sibling harnesses.
//!
//! # Expression keys — `date_trunc` buckets as the snapshot dimension
//!
//! A snapshot dimension is usually an expression over a date column, such as
//! `date_trunc('month', s.d)`, not the column itself. The table therefore also
//! has a `DATE` column `d`, and the view a dimension `tsk = date_trunc('<unit>',
//! s.d)`, with the unit drawn from `week` / `month` / `quarter`. Each case picks
//! whether the metric is `NON ADDITIVE BY (ts)` or `NON ADDITIVE BY (tsk)`. With
//! `tsk`, the snapshot is the latest (or earliest) *bucket*, and every row in
//! it is summed, although the rows fall on different days: `d`'s small domain
//! is chosen so that each unit merges different days into one bucket. Either
//! way the other dimension may be queried too, which makes it a partition of
//! the snapshot. The oracle ranks the same key expression with `MAX`/`MIN`.

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, MetricName, QueryRequest};
use semantic_views::model::{
    AccessModifier, Dimension, Metric, NonAdditiveDim, NullsOrder, SemanticViewDefinition,
    SortOrder, TableRef, WindowSpec,
};

/// Members a generated `where_clause` may name (PBT-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WMember {
    /// `s.entity` — a partition dimension.
    Ent,
    /// `s.ts` — the NA dimension itself. Filtering on it moves the snapshot.
    Ts,
    /// Filter member over `entity`: `s.entity = 0 OR s.entity = 2`.
    Fent,
    /// Filter member over `ts`: `s.ts = 0 OR s.ts = 2`.
    Fts,
}

impl WMember {
    fn name(self) -> &'static str {
        match self {
            WMember::Ent => "ent",
            WMember::Ts => "ts",
            WMember::Fent => "fent",
            WMember::Fts => "fts",
        }
    }

    /// The raw SQL the member's expression stands for, for the oracle.
    fn raw(self) -> &'static str {
        match self {
            WMember::Ent => "s.entity",
            WMember::Ts => "s.ts",
            WMember::Fent => "(s.entity = 0 OR s.entity = 2)",
            WMember::Fts => "(s.ts = 0 OR s.ts = 2)",
        }
    }

    /// Whether the member is a bare column (so it can carry a comparison) as
    /// opposed to a self-contained boolean filter.
    fn is_comparable(self) -> bool {
        matches!(self, WMember::Ent | WMember::Ts)
    }

    /// Whether the member constrains the NA dimension `ts` — the shape that
    /// can move the snapshot rather than merely shrink the summed set.
    fn touches_ts(self) -> bool {
        matches!(self, WMember::Ts | WMember::Fts)
    }
}

/// A generated pre-aggregation predicate.
#[derive(Debug, Clone)]
enum Pred {
    Cmp(WMember, &'static str, i64),
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

    /// The oracle's independent rendering: raw columns, filter members expanded,
    /// every operand explicitly parenthesized.
    fn to_raw_sql(&self) -> String {
        match self {
            Pred::Cmp(m, op, k) => format!("({} {op} {k})", m.raw()),
            Pred::Filter(m) => format!("({})", m.raw()),
            Pred::And(a, b) => format!("({} AND {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_raw_sql()),
        }
    }

    /// Whether any named member constrains `ts` (generator-coverage guard).
    fn touches_ts(&self) -> bool {
        match self {
            Pred::Cmp(m, _, _) | Pred::Filter(m) => m.touches_ts(),
            Pred::And(a, b) | Pred::Or(a, b) => a.touches_ts() || b.touches_ts(),
            Pred::Not(a) => a.touches_ts(),
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
}

/// Predicates over the two dimensions. Literals span `-1..=3` against domains of
/// `0..3` + NULL, so the generated set includes always-true, selective, and
/// everything-filtered-away cases — the last of which empties a partition and
/// leaves the snapshot undefined, which the oracle must agree about.
fn arb_pred() -> impl Strategy<Value = Pred> {
    let leaf = prop_oneof![
        3 => (
            prop_oneof![Just(WMember::Ent), Just(WMember::Ts)],
            prop_oneof![Just("<"), Just("<="), Just("="), Just("<>"), Just(">="), Just(">")],
            -1i64..=3,
        ).prop_map(|(m, op, k)| Pred::Cmp(m, op, k)),
        2 => prop_oneof![Just(WMember::Fent), Just(WMember::Fts)].prop_map(Pred::Filter),
    ];
    leaf.prop_recursive(3, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            inner.prop_map(|a| Pred::Not(Box::new(a))),
        ]
    })
}

/// A generated instance: rows of `(entity, ts, balance, day)`, where `day`
/// indexes [`DAY_OFFSETS`] for the row's `d`. `None` is a SQL NULL throughout.
/// Small `entity`/`ts` domains force duplicate `(entity, ts)` pairs (RANK ties
/// within a snapshot) and repeated snapshot timestamps.
#[derive(Debug, Clone)]
struct Instance {
    rows: Vec<(Option<i64>, Option<i64>, Option<i64>, Option<usize>)>,
}

/// The days `d` takes, as offsets from 2024-01-01 (a Monday): Jan 1, Jan 4,
/// Jan 11, Feb 10 and Apr 5. Every unit merges some of them into one bucket:
/// the first two share a week, the first three a month, the first four a
/// quarter.
const DAY_OFFSETS: [i64; 5] = [0, 3, 10, 40, 95];

/// The `date_trunc` units the key dimension `tsk` is drawn from.
const UNITS: [&str; 3] = ["week", "month", "quarter"];

/// Each day's bucket under each unit (indexed like [`UNITS`]), so the generator
/// guard can tell when an instance puts different days in one bucket without
/// asking DuckDB.
const DAY_BUCKETS: [[u8; 5]; 3] = [[0, 0, 1, 2, 3], [0, 0, 0, 1, 3], [0, 0, 0, 0, 1]];

/// Queryable dimensions and the single semi-additive metric, by stable name.
const DIMS: [&str; 3] = ["ent", "ts", "tsk"];
const METS: [&str; 1] = ["bal"];
/// Index of the `ts` dimension within `DIMS`.
const TS_DIM: usize = 1;
/// Index of the `date_trunc` key dimension `tsk` within `DIMS`.
const TSK_DIM: usize = 2;

/// Which dimension the metric is `NON ADDITIVE BY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NaKey {
    /// The raw column `ts`.
    Ts,
    /// The expression key `tsk = date_trunc('<unit>', s.d)`.
    Trunc,
}

impl NaKey {
    /// The NA dimension's index within `DIMS`.
    fn dim(self) -> usize {
        match self {
            NaKey::Ts => TS_DIM,
            NaKey::Trunc => TSK_DIM,
        }
    }
}

/// A full case: an instance, the declared NA sort direction, and the non-empty
/// subset of dims + metrics to query (indices into `DIMS` / `METS`).
#[derive(Debug, Clone)]
struct Case {
    inst: Instance,
    order: SortOrder,
    /// The dimension the metric is `NON ADDITIVE BY`.
    na: NaKey,
    /// The `date_trunc` unit of `tsk`, whether or not it is the NA dimension.
    unit: &'static str,
    sel_dims: Vec<usize>,
    sel_metrics: Vec<usize>,
    /// The pre-aggregation predicate (PBT-6). `None` one case in four, keeping
    /// the original no-predicate coverage rather than replacing it.
    where_pred: Option<Pred>,
}

fn arb_instance() -> impl Strategy<Value = Instance> {
    // Small signed value domain + NULL, mirroring the sibling harnesses.
    let bal_cell = prop_oneof![
        1 => Just(None),
        3 => (-5i64..=5).prop_map(Some),
    ];
    // Small entity/ts domains so ties and repeated snapshot timestamps are common.
    let ent_cell = prop_oneof![
        1 => Just(None),
        4 => (0i64..3).prop_map(Some),
    ];
    let ts_cell = prop_oneof![
        1 => Just(None),
        4 => (0i64..3).prop_map(Some),
    ];
    let day_cell = prop_oneof![
        1 => Just(None),
        4 => (0..DAY_OFFSETS.len()).prop_map(Some),
    ];
    let row = (ent_cell, ts_cell, bal_cell, day_cell);
    prop::collection::vec(row, 0..=20).prop_map(|rows| Instance { rows })
}

fn arb_case() -> impl Strategy<Value = Case> {
    let order = prop_oneof![Just(SortOrder::Asc), Just(SortOrder::Desc)];
    let na = prop_oneof![Just(NaKey::Ts), Just(NaKey::Trunc)];
    let unit = prop::sample::select(UNITS.to_vec());
    (arb_instance(), order, na, unit).prop_flat_map(|(inst, order, na, unit)| {
        let dim_sel =
            prop::sample::subsequence((0..DIMS.len()).collect::<Vec<_>>(), 0..=DIMS.len());
        let met_sel =
            prop::sample::subsequence((0..METS.len()).collect::<Vec<_>>(), 0..=METS.len());
        let where_pred = prop_oneof![
            1 => Just(None),
            3 => arb_pred().prop_map(Some),
        ];
        (Just(inst), Just(order), dim_sel, met_sel, where_pred)
            .prop_filter(
                "at least one of dimensions/metrics must be selected",
                |(_, _, sel_dims, sel_metrics, _)| !sel_dims.is_empty() || !sel_metrics.is_empty(),
            )
            .prop_map(
                move |(inst, order, sel_dims, sel_metrics, where_pred)| Case {
                    inst,
                    order,
                    na,
                    unit,
                    sel_dims,
                    sel_metrics,
                    where_pred,
                },
            )
    })
}

/// The SQL of the key dimension `tsk` for `unit`.
fn tsk_expr(unit: &str) -> String {
    format!("date_trunc('{unit}', s.d)")
}

/// Build the semantic-view definition for the given NA sort direction, NA
/// dimension and `tsk` unit: single table `s`, dimensions on `entity`, `ts` and
/// `tsk = date_trunc('<unit>', s.d)`, and `bal = SUM(s.balance) NON ADDITIVE BY
/// (<ts | tsk> <order>)`.
fn build_def(order: SortOrder, na: NaKey, unit: &str) -> SemanticViewDefinition {
    let tables = vec![TableRef {
        alias: "s".to_string(),
        table: "s".to_string(),
        pk_columns: vec![],
        unique_constraints: vec![],
        comment: None,
        synonyms: vec![],
    }];
    let dimensions = vec![
        Dimension {
            name: "ent".to_string(),
            expr: "s.entity".to_string(),
            source_table: Some("s".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
        },
        Dimension {
            name: "ts".to_string(),
            expr: "s.ts".to_string(),
            source_table: Some("s".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
        },
        Dimension {
            name: "tsk".to_string(),
            expr: tsk_expr(unit),
            source_table: Some("s".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
        },
        // PBT-6 filter members: never selected as output, named only by a
        // generated `where_clause`. Compound expressions, so a splice that
        // loses its parentheses changes which rows reach the snapshot.
        Dimension {
            name: "fent".to_string(),
            expr: "s.entity = 0 OR s.entity = 2".to_string(),
            source_table: Some("s".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: true,
        },
        Dimension {
            name: "fts".to_string(),
            expr: "s.ts = 0 OR s.ts = 2".to_string(),
            source_table: Some("s".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: true,
        },
    ];
    let metrics = vec![Metric {
        name: "bal".to_string(),
        expr: "sum(s.balance)".to_string(),
        source_table: Some("s".to_string()),
        output_type: None,
        using_relationships: vec![],
        comment: None,
        synonyms: vec![],
        access: AccessModifier::Public,
        // Default NULLS placement per direction (Last for ASC, First for DESC)
        // is what the parser assigns for a bare `NON ADDITIVE BY (ts [DESC])`;
        // the oracle below matches that default.
        non_additive_by: vec![NonAdditiveDim {
            dimension: DIMS[na.dim()].to_string(),
            order,
            nulls: match order {
                SortOrder::Asc => NullsOrder::Last,
                SortOrder::Desc => NullsOrder::First,
            },
        }],
        window_spec: None,
    }];
    SemanticViewDefinition {
        tables,
        dimensions,
        metrics,
        joins: vec![],
        // PIN: a snapshot is an aggregate shape; FACTS returns unaggregated rows and cannot express NON ADDITIVE BY. TECH-DEBT #66
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

/// Create the physical table and insert the generated rows.
fn make_db(inst: &Instance) -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch("CREATE TABLE s (entity INTEGER, ts INTEGER, balance INTEGER, d DATE);")
        .expect("create table");
    let cell = |c: &Option<i64>| c.map_or_else(|| "NULL".to_string(), |v| v.to_string());
    let day = |c: &Option<usize>| {
        c.map_or_else(
            || "NULL".to_string(),
            |i| format!("DATE '2024-01-01' + {}", DAY_OFFSETS[i]),
        )
    };
    if !inst.rows.is_empty() {
        let values: Vec<String> = inst
            .rows
            .iter()
            .map(|(e, t, b, d)| format!("({},{},{},{})", cell(e), cell(t), cell(b), day(d)))
            .collect();
        conn.execute_batch(&format!("INSERT INTO s VALUES {};", values.join(",")))
            .expect("insert rows");
    }
    conn
}

/// The raw SQL a queried dimension stands for.
fn dim_raw(i: usize, unit: &str) -> String {
    match DIMS[i] {
        "ent" => "s.entity".to_string(),
        "ts" => "s.ts".to_string(),
        "tsk" => tsk_expr(unit),
        other => unreachable!("unexpected dim {other}"),
    }
}

/// Physical `SELECT` expression + output alias for a queried dimension.
fn dim_item(i: usize, unit: &str) -> String {
    format!("{} AS {}", dim_raw(i, unit), DIMS[i])
}

/// Independent oracle SQL for a case. Structurally different from the
/// extension's RANK-CTE: the snapshot value is computed with `MAX`/`MIN` and
/// selected with `IS NOT DISTINCT FROM` (so NULL keys and a NULL snapshot match
/// by identity, not `=`).
fn oracle_sql(case: &Case) -> String {
    let dims: Vec<String> = case
        .sel_dims
        .iter()
        .map(|&i| dim_item(i, case.unit))
        .collect();
    let na_queried = case.sel_dims.contains(&case.na.dim());
    let has_metric = !case.sel_metrics.is_empty();
    // The snapshot key: the NA dimension's expression.
    let key = dim_raw(case.na.dim(), case.unit);

    // PBT-6: the predicate, rendered from raw columns. It is spliced into EVERY
    // reference to `s` below -- including the subquery that determines the
    // snapshot timestamp. Filtering before the snapshot is picked is the whole
    // semantic: dropping the MAX(ts) row promotes the next timestamp to be the
    // snapshot. An oracle that filtered only the outer aggregate would encode
    // post-snapshot filtering and would agree with an implementation that made
    // the same mistake.
    let pred = case.where_pred.as_ref().map(Pred::to_raw_sql);
    // ` WHERE <pred>` for a clause-less FROM, or the empty string.
    let where_sql = pred
        .as_ref()
        .map_or_else(String::new, |p| format!(" WHERE {p}"));
    // `<pred> AND ` to prepend to an existing WHERE, or the empty string.
    let and_pred = pred
        .as_ref()
        .map_or_else(String::new, |p| format!("{p} AND "));

    // Dims-only query -> SELECT DISTINCT (no aggregation, no snapshot).
    if !has_metric {
        return format!("SELECT DISTINCT {} FROM s{where_sql}", dims.join(", "));
    }

    // The NA dim is queried -> the metric is effectively regular: a plain
    // grouped SUM, no snapshot.
    if na_queried {
        let select = dims
            .iter()
            .cloned()
            .chain(std::iter::once("sum(s.balance) AS bal".to_string()))
            .collect::<Vec<_>>()
            .join(", ");
        if case.sel_dims.is_empty() {
            return format!("SELECT {select} FROM s{where_sql}");
        }
        let group_by = (1..=case.sel_dims.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return format!("SELECT {select} FROM s{where_sql} GROUP BY {group_by}");
    }

    // Active snapshot. Partition = queried dims; the NA dim is not queried
    // here. Snapshot key per partition (the key is `ts` or the `tsk` bucket):
    //   ASC  / NULLS LAST : MAX(key)                       (latest; NULL only if all NULL)
    //   DESC / NULLS FIRST: NULL if any NULL key, else MIN (earliest; NULL wins)
    let snap_expr = match case.order {
        SortOrder::Asc => format!("max({key})"),
        SortOrder::Desc => format!(
            "CASE WHEN count(*) FILTER (WHERE {key} IS NULL) > 0 THEN NULL ELSE min({key}) END"
        ),
    };

    // Partition dimensions among the queried dims (everything except the NA
    // dim, which is not queried in this branch, so this is exactly `sel_dims`),
    // as (name, raw SQL).
    let part_cols: Vec<(&str, String)> = case
        .sel_dims
        .iter()
        .map(|&i| (DIMS[i], dim_raw(i, case.unit)))
        .collect();

    let select = dims
        .iter()
        .cloned()
        .chain(std::iter::once("sum(s.balance) AS bal".to_string()))
        .collect::<Vec<_>>()
        .join(", ");

    if part_cols.is_empty() {
        // Global snapshot: single-row subquery, filter to the snapshot ts.
        // The predicate applies BOTH inside the snapshot subquery (so the
        // snapshot is chosen among surviving rows) and to the outer scan.
        format!(
            "SELECT {select} FROM s, (SELECT {snap_expr} AS snap FROM s{where_sql}) m \
             WHERE {and_pred}{key} IS NOT DISTINCT FROM m.snap"
        )
    } else {
        let sub_group = (1..=part_cols.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let sub_select = part_cols
            .iter()
            .map(|(name, raw)| format!("{raw} AS p_{name}"))
            .chain(std::iter::once(format!("{snap_expr} AS snap")))
            .collect::<Vec<_>>()
            .join(", ");
        let join_on = part_cols
            .iter()
            .map(|(name, raw)| format!("{raw} IS NOT DISTINCT FROM m.p_{name}"))
            .chain(std::iter::once(format!(
                "{key} IS NOT DISTINCT FROM m.snap"
            )))
            .collect::<Vec<_>>()
            .join(" AND ");
        let group_by = (1..=case.sel_dims.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        // Same on the partitioned path: the per-partition snapshot is computed
        // over filtered rows, and the outer scan is filtered too. A partition
        // emptied by the predicate drops out of both sides.
        format!(
            "SELECT {select} FROM s JOIN \
             (SELECT {sub_select} FROM s{where_sql} GROUP BY {sub_group}) m \
             ON {join_on}{} GROUP BY {group_by}",
            pred.as_ref()
                .map_or_else(String::new, |p| format!(" WHERE {p}"))
        )
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 192, ..ProptestConfig::default() })]

    #[test]
    fn semi_additive_snapshot_matches_independent_oracle(case in arb_case()) {
        let def = build_def(case.order, case.na, case.unit);
        let req = QueryRequest {
            where_clause: case.where_pred.as_ref().map(Pred::to_member_sql),
            dimensions: case.sel_dims.iter().map(|&i| DimensionName::new(DIMS[i])).collect(),
            metrics: case.sel_metrics.iter().map(|&i| MetricName::new(METS[i])).collect(),
            // PIN: a snapshot is an aggregate shape; FACTS returns unaggregated rows and cannot express NON ADDITIVE BY. TECH-DEBT #66
            facts: vec![],
        };

        // Single table, no joins -> no fan trap; every query is accepted.
        let expanded = match expand("semi", &def, &req) {
            Ok(sql) => sql,
            Err(e) => {
                prop_assert!(false, "single-table semi-additive query unexpectedly rejected: {e}\ncase: {case:?}");
                unreachable!()
            }
        };
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
            "semi-additive expansion disagrees with the independent snapshot oracle \
             (symmetric multiset diff = {}); order={:?} na={:?} unit={} dims={:?} metrics={:?}\n--- expanded:\n{}\n--- oracle:\n{}",
            diff, case.order, case.na, case.unit, case.sel_dims, case.sel_metrics, expanded, oracle
        );
    }
}

/// PBT-6 / TECH-DEBT #41 guard: prove the generator reaches the shapes this
/// harness exists to cover.
///
/// The important one is `touches_ts`: a predicate constraining the NA dimension
/// is what makes the snapshot *move* rather than merely shrink, and it is the
/// only shape that distinguishes "filtered before the RANK" from "filtered
/// after it". If the generator stopped producing those, the property would
/// still pass while testing nothing this file was extended for.
#[test]
fn generator_varies_the_predicate_and_moves_the_snapshot() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::HashSet;

    let mut runner = TestRunner::deterministic();
    let mut with_pred = 0usize;
    let mut without_pred = 0usize;
    let mut with_filter = 0usize;
    let mut touching_ts = 0usize;
    // The snapshot-moving combination: a predicate on `ts` while `ts` is NOT
    // queried, so the metric is an ACTIVE snapshot rather than a plain GROUP BY.
    let mut active_snapshot_ts_pred = 0usize;
    let mut distinct: HashSet<String> = HashSet::new();

    for _ in 0..400 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        let ts_queried = case.sel_dims.contains(&TS_DIM);
        let has_metric = !case.sel_metrics.is_empty();
        match &case.where_pred {
            None => without_pred += 1,
            Some(p) => {
                with_pred += 1;
                if p.references_filter() {
                    with_filter += 1;
                }
                if p.touches_ts() {
                    touching_ts += 1;
                    if case.na == NaKey::Ts && has_metric && !ts_queried {
                        active_snapshot_ts_pred += 1;
                    }
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
        touching_ts > 0,
        "generator never constrained the NA dimension -- nothing exercises a \
         predicate that changes which row wins the snapshot"
    );
    assert!(
        active_snapshot_ts_pred > 0,
        "no case combined an ACTIVE snapshot with a predicate on the NA \
         dimension; that is the only shape distinguishing filtering before the \
         RANK from filtering after it, so the extension to this harness would \
         be inert"
    );
    assert!(
        distinct.len() > 50,
        "generator produced only {} distinct predicates; search space collapsed",
        distinct.len()
    );
}

/// Anti-vacuity guard for the expression-key axis: every `date_trunc` unit is
/// the snapshot key of an ACTIVE snapshot (a metric queried without `tsk`) on
/// data where that unit puts different days in one bucket, so the snapshot sums
/// rows of several days. Without the merge the bucket would rank exactly as `d`
/// does and the axis would test nothing a raw column does not. The two keys
/// must also each be queried as a partition of the other's snapshot.
#[test]
fn generator_reaches_every_expression_key() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let mut merged_active = [0usize; UNITS.len()];
    let mut ts_partitions_tsk = 0usize;
    let mut tsk_partitions_ts = 0usize;

    for _ in 0..400 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        if case.sel_metrics.is_empty() {
            continue;
        }
        let u = UNITS
            .iter()
            .position(|x| *x == case.unit)
            .expect("unit is one of UNITS");
        let na_queried = case.sel_dims.contains(&case.na.dim());
        match case.na {
            NaKey::Trunc if !na_queried => {
                let days: std::collections::HashSet<usize> =
                    case.inst.rows.iter().filter_map(|r| r.3).collect();
                let merges = days.iter().any(|&a| {
                    days.iter()
                        .any(|&b| a != b && DAY_BUCKETS[u][a] == DAY_BUCKETS[u][b])
                });
                if merges {
                    merged_active[u] += 1;
                }
                if case.sel_dims.contains(&TS_DIM) {
                    tsk_partitions_ts += 1;
                }
            }
            NaKey::Ts if !na_queried && case.sel_dims.contains(&TSK_DIM) => {
                ts_partitions_tsk += 1;
            }
            _ => {}
        }
    }

    for (u, n) in merged_active.iter().enumerate() {
        assert!(
            *n > 0,
            "no active snapshot keyed by date_trunc('{}') saw two days merged into one \
             bucket",
            UNITS[u]
        );
    }
    assert!(
        tsk_partitions_ts > 0,
        "no snapshot on tsk was partitioned by ts"
    );
    assert!(
        ts_partitions_tsk > 0,
        "no snapshot on ts was partitioned by tsk"
    );
}

/// The oracle's defining property, pinned deterministically: filtering BEFORE
/// the snapshot is a different query from filtering AFTER it, and the extension
/// implements the former.
///
/// This is what makes the `where_clause` extension to this harness worth having.
/// A naive oracle that applied the predicate only to the outer aggregate would
/// still pass the randomized property against an implementation that made the
/// same mistake — the two would be wrong together. So the two formulations are
/// compared against each other here as well as against the extension: the test
/// fails if they ever stop disagreeing, which would mean the randomized
/// comparison had quietly become insensitive to the distinction.
///
/// Data: one entity with rows at `ts = 2` (balance 100) and `ts = 1`
/// (balance 10), `NON ADDITIVE BY (ts)` ASC, so the unfiltered snapshot is
/// `ts = 2` → 100. Under `where_clause := 'ts < 2'`:
///   * filtered BEFORE the snapshot → the surviving max is `ts = 1` → **10**
///   * filtered AFTER the snapshot  → the snapshot stays `ts = 2`, then the
///     filter removes it → **no rows**
#[test]
fn predicate_is_applied_before_the_snapshot_not_after() {
    let inst = Instance {
        rows: vec![
            (Some(1), Some(2), Some(100), None),
            (Some(1), Some(1), Some(10), None),
        ],
    };
    let def = build_def(SortOrder::Asc, NaKey::Ts, "month");
    let req = QueryRequest {
        where_clause: Some("ts < 2".to_string()),
        dimensions: vec![DimensionName::new("ent")],
        metrics: vec![MetricName::new("bal")],
        // PIN: a snapshot is an aggregate shape; FACTS returns unaggregated rows and cannot express NON ADDITIVE BY. TECH-DEBT #66
        facts: vec![],
    };
    let expanded = expand("semi", &def, &req).expect("single-table snapshot query must expand");

    // Filter applied inside the snapshot subquery AND the outer scan -- what
    // `oracle_sql` builds, and what the extension is documented to do.
    let before = "SELECT s.entity AS ent, sum(s.balance) AS bal FROM s \
                  JOIN (SELECT s.entity AS p_entity, max(s.ts) AS snap FROM s \
                        WHERE (s.ts < 2) GROUP BY entity) m \
                  ON s.entity IS NOT DISTINCT FROM m.p_entity \
                     AND s.ts IS NOT DISTINCT FROM m.snap \
                  WHERE (s.ts < 2) GROUP BY 1";
    // The naive alternative: snapshot chosen from unfiltered rows, predicate
    // applied afterwards.
    let after = "SELECT s.entity AS ent, sum(s.balance) AS bal FROM s \
                 JOIN (SELECT s.entity AS p_entity, max(s.ts) AS snap FROM s \
                       GROUP BY entity) m \
                 ON s.entity IS NOT DISTINCT FROM m.p_entity \
                    AND s.ts IS NOT DISTINCT FROM m.snap \
                 WHERE (s.ts < 2) GROUP BY 1";

    let conn = make_db(&inst);
    let fetch = |sql: &str| -> Vec<(i64, Option<i64>)> {
        let mut stmt = conn.prepare(sql).expect("prepare");
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        rows
    };

    let got = fetch(&expanded);
    let want_before = fetch(before);
    let want_after = fetch(after);

    // The two formulations must genuinely disagree, or this test proves nothing
    // and the randomized property is insensitive to the distinction.
    assert_ne!(
        want_before, want_after,
        "before/after-snapshot filtering must differ on this data, otherwise the \
         oracle's structure is not load-bearing"
    );
    assert_eq!(
        want_before,
        vec![(1i64, Some(10i64))],
        "sanity: filtering before the snapshot promotes ts=1, giving balance 10"
    );
    assert!(
        want_after.is_empty(),
        "sanity: filtering after the snapshot removes the ts=2 winner, leaving \
         no rows; got {want_after:?}"
    );
    assert_eq!(
        got, want_before,
        "the extension must filter BEFORE the snapshot is picked.\n--- emitted:\n{expanded}"
    );
}

// EXP-19 / EXP-20 (code-review 2026-08-06): a metric that DEPENDS on the
// semi-additive one.
//
// The routing predicate `is_active_semi_additive` asks only about a metric's
// OWN `non_additive_by`, so a derived metric referencing `bal`, or a window
// metric naming it as its inner aggregate, classified as regular: the raw
// `SUM(s.balance)` was inlined and evaluated over every row, silently
// discarding NON ADDITIVE BY. `dbal` did not even come out at twice `bal`.
//
// This harness generated no derived and no window metrics at all — the
// blind spot the coverage audit flagged and these two bugs then occupied. The
// property below is a dichotomy over the same generated data the numeric
// oracle uses:
//
//   * NA dimension NOT queried  => the snapshot is live and cannot be composed
//                                  through the dependency, so expansion MUST
//                                  error (and name the dependency).
//   * NA dimension queried      => the metric is "effectively regular"
//                                  (Snowflake semantics), so expansion MUST
//                                  succeed AND agree with an independent
//                                  oracle numerically.
//
// The second branch is what stops the first from being satisfiable by erroring
// on everything.
//
// PBT-14 (code-review 2026-08-08): the window half of that second branch used
// to `return Ok(())` before the numeric comparison, deferring to
// `window_metric_proptest` — whose inner metric is always the self-referential
// `w`. A window wrapping a DIFFERENT (here semi-additive-classified) inner
// metric therefore had numeric coverage nowhere: EXP-19's fence was randomized,
// EXP-20's arithmetic was not. `wbal` is now oracled with the same
// correlated-subquery formulation `window_metric_proptest` uses (the partition
// key `ent` is fixed here, so it is mechanical), and the early return is gone.
// The same change un-pins the harness's `where_clause: None`.

/// [`build_def`] plus the two dependent metrics: `dbal = bal * 2` (derived) and
/// `wbal = SUM(bal) OVER (PARTITION BY ent)` (window over the same inner).
fn build_def_with_dependents(order: SortOrder) -> SemanticViewDefinition {
    let mut def = build_def(order, NaKey::Ts, "month");
    let dependent = |name: &str, expr: &str, window: Option<WindowSpec>| Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: None,
        output_type: None,
        using_relationships: vec![],
        comment: None,
        synonyms: vec![],
        access: AccessModifier::Public,
        non_additive_by: vec![],
        window_spec: window,
    };
    def.metrics.push(dependent("dbal", "bal * 2", None));
    def.metrics.push(dependent(
        "wbal",
        "",
        Some(WindowSpec {
            window_function: "SUM".to_string(),
            inner_metric: "bal".to_string(),
            extra_args: vec![],
            excluding_dims: vec![],
            partition_dims: vec!["ent".to_string()],
            order_by: vec![],
            frame_clause: None,
        }),
    ));
    def
}

/// One generated dependent-metric case. A struct rather than four bare
/// `proptest!` arguments so [`dependent_generator_reaches_every_branch`] can
/// sample the same strategy and count what it actually produces (PBT-14).
#[derive(Debug, Clone)]
struct DependentCase {
    inst: Instance,
    order: SortOrder,
    /// Whether the NA dimension `ts` is queried — the dichotomy's switch.
    na_dim_queried: bool,
    /// `true` selects the WINDOW dependent `wbal`, `false` the DERIVED `dbal`.
    pick_window: bool,
    /// PBT-14: previously pinned at `None` here, the shape CLAUDE.md names as
    /// how EXP-9/EXP-10 reached `main`. A predicate is the most direct way to
    /// break a dependent metric's grain: it changes which rows reach the inner
    /// aggregate, and for `wbal` it can empty a group and so remove a row from
    /// the partition the window then sums over.
    where_pred: Option<Pred>,
}

fn arb_dependent_case() -> impl Strategy<Value = DependentCase> {
    (
        arb_instance(),
        prop_oneof![Just(SortOrder::Asc), Just(SortOrder::Desc)],
        any::<bool>(),
        any::<bool>(),
        prop_oneof![1 => Just(None), 3 => arb_pred().prop_map(Some)],
    )
        .prop_map(
            |(inst, order, na_dim_queried, pick_window, where_pred)| DependentCase {
                inst,
                order,
                na_dim_queried,
                pick_window,
                where_pred,
            },
        )
}

/// Independent oracle for the effectively-regular branch, per dependent metric.
///
/// The predicate belongs PRE-aggregation in both formulations: it decides which
/// rows reach `sum(s.balance)`, and — for the window metric — emptying a group
/// removes that row from the partition entirely, which is why the correlated
/// subquery reads from the already-filtered CTE rather than filtering after.
///
/// `dbal` is the direct arithmetic. `wbal` is deliberately NOT written as
/// `SUM(...) OVER (PARTITION BY ...)`: that would restate the extension's own
/// construct and agree with it by construction. It uses the correlated-subquery
/// formulation from `window_metric_proptest` — pre-aggregate by every queried
/// dim, then aggregate over the rows sharing the partition key, matched with
/// `IS NOT DISTINCT FROM` so NULL keys group together exactly as `PARTITION BY`
/// does.
fn dependent_oracle_sql(case: &DependentCase) -> String {
    let where_sql = case
        .where_pred
        .as_ref()
        .map_or_else(String::new, |p| format!(" WHERE {}", p.to_raw_sql()));
    if case.pick_window {
        format!(
            "WITH agg AS (SELECT s.entity AS ent, s.ts AS ts, sum(s.balance) AS bal \
             FROM s{where_sql} GROUP BY 1, 2) \
             SELECT a1.ent, a1.ts, \
             (SELECT SUM(a2.bal) FROM agg a2 \
              WHERE a2.ent IS NOT DISTINCT FROM a1.ent) AS wbal \
             FROM agg a1"
        )
    } else {
        format!(
            "SELECT s.entity AS ent, s.ts AS ts, sum(s.balance) * 2 AS dbal \
             FROM s{where_sql} GROUP BY 1, 2"
        )
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn a_dependent_metric_never_silently_drops_the_snapshot(case in arb_dependent_case()) {
        let def = build_def_with_dependents(case.order);
        let metric = if case.pick_window { "wbal" } else { "dbal" };
        let dims: Vec<&str> = if case.na_dim_queried { vec!["ent", "ts"] } else { vec!["ent"] };
        let req = QueryRequest {
            where_clause: case.where_pred.as_ref().map(Pred::to_member_sql),
            dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
            metrics: vec![MetricName::new(metric)],
            // PIN: a snapshot is an aggregate shape; FACTS returns unaggregated rows and cannot express NON ADDITIVE BY. TECH-DEBT #66
            facts: vec![],
        };

        match expand("semi", &def, &req) {
            Err(e) => {
                let msg = e.to_string();
                prop_assert!(
                    !case.na_dim_queried,
                    "with the NA dimension queried '{metric}' is effectively regular \
                     and must expand, got error: {msg}"
                );
                prop_assert!(
                    msg.contains("depends on semi-additive metric"),
                    "rejected, but not as a semi-additive dependency: {msg}"
                );
            }
            Ok(sql) => {
                prop_assert!(
                    case.na_dim_queried,
                    "without the NA dimension the snapshot cannot be composed through \
                     '{metric}' and must not be silently dropped, got SQL:\n{sql}"
                );
                // Effectively-regular branch: check the NUMBER, not just the
                // absence of an error. PBT-14: this now covers the window
                // dependent too — it used to return here, leaving a window over
                // a non-self-referential inner metric unoracled anywhere.
                let oracle = dependent_oracle_sql(&case);
                let cmp = format!(
                    "SELECT \
                       (SELECT count(*) FROM (SELECT {metric}, ent, ts FROM ({sql}) qa \
                                              EXCEPT ALL \
                                              SELECT {metric}, ent, ts FROM ({oracle}) qb) e1) \
                     + (SELECT count(*) FROM (SELECT {metric}, ent, ts FROM ({oracle}) qc \
                                              EXCEPT ALL \
                                              SELECT {metric}, ent, ts FROM ({sql}) qd) e2) AS diff"
                );
                let conn = make_db(&case.inst);
                let diff: i64 = conn.query_row(&cmp, [], |r| r.get(0)).unwrap_or_else(|e| {
                    panic!("comparison failed: {e}\n--- expanded:\n{sql}\n--- oracle:\n{oracle}")
                });
                prop_assert_eq!(
                    diff, 0,
                    "an effectively-regular dependent metric ('{}') disagrees with the \
                     independent oracle\n--- expanded:\n{}\n--- oracle:\n{}",
                    metric, sql, oracle
                );
            }
        }
    }
}

/// PBT-14 anti-vacuity guard for the dependent-metric property.
///
/// The property is a dichotomy over four independent switches, and every one of
/// its `prop_assert!`s sits inside a branch. A branch that is never generated is
/// exactly as green as a correct one — the same reasoning as
/// `generator_reaches_both_where_clause_branches` in the sibling harnesses — so
/// the branch counts are asserted directly here. In particular
/// `window_effectively_regular` is the cell PBT-14 opened: before the fix that
/// combination returned before comparing anything.
#[test]
fn dependent_generator_reaches_every_branch() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::HashSet;

    let mut runner = TestRunner::deterministic();
    let mut window = 0usize;
    let mut derived = 0usize;
    let mut window_effectively_regular = 0usize;
    let mut window_rejected = 0usize;
    let mut derived_effectively_regular = 0usize;
    let mut derived_rejected = 0usize;
    let mut with_pred = 0usize;
    let mut without_pred = 0usize;
    let mut window_regular_with_pred = 0usize;
    let mut pred_touches_ts = 0usize;
    let mut pred_uses_filter_member = 0usize;
    let mut distinct: HashSet<String> = HashSet::new();

    for _ in 0..400 {
        let case = arb_dependent_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        match (case.pick_window, case.na_dim_queried) {
            (true, true) => {
                window += 1;
                window_effectively_regular += 1;
            }
            (true, false) => {
                window += 1;
                window_rejected += 1;
            }
            (false, true) => {
                derived += 1;
                derived_effectively_regular += 1;
            }
            (false, false) => {
                derived += 1;
                derived_rejected += 1;
            }
        }
        match &case.where_pred {
            None => without_pred += 1,
            Some(p) => {
                with_pred += 1;
                if p.touches_ts() {
                    pred_touches_ts += 1;
                }
                if p.references_filter() {
                    pred_uses_filter_member += 1;
                }
                if case.pick_window && case.na_dim_queried {
                    window_regular_with_pred += 1;
                }
                distinct.insert(p.to_member_sql());
            }
        }
    }

    assert!(
        window > 0 && derived > 0,
        "both dependent metrics must be generated (window={window}, derived={derived})"
    );
    assert!(
        window_effectively_regular > 0,
        "no case reached the window + NA-dim-queried cell — the numeric \
         comparison PBT-14 added is unreachable and therefore vacuous"
    );
    assert!(
        window_rejected > 0,
        "no case reached the window + NA-dim-absent cell — the EXP-19 rejection \
         assertion is never exercised for the window dependent"
    );
    assert!(
        derived_effectively_regular > 0 && derived_rejected > 0,
        "both derived cells must be reached (regular={derived_effectively_regular}, \
         rejected={derived_rejected})"
    );
    assert!(
        with_pred > 0 && without_pred > 0,
        "where_clause must be both present and absent (present={with_pred}, \
         absent={without_pred}) — a field pinned at its inert default is not \
         coverage"
    );
    assert!(
        window_regular_with_pred > 0,
        "no case combined a predicate with the window metric on the oracled \
         branch, so the predicate never reaches the partition the window sums \
         over"
    );
    assert!(
        pred_touches_ts > 0,
        "no generated predicate constrained the NA dimension `ts`"
    );
    assert!(
        pred_uses_filter_member > 0,
        "no generated predicate named a FILTER member, so the member-substitution \
         surface is untested here"
    );
    assert!(
        distinct.len() > 50,
        "generator produced only {} distinct predicates; search space collapsed",
        distinct.len()
    );
}
