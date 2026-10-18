//! T-9 (code-review 2026-07-11): Rust-side randomized-schema differential
//! proptest for the core base-table aggregation path.
//!
//! For each case we generate a random single-table star-schema *shape* — a
//! random number of group-by dimension columns and a random set of aggregate
//! metrics over random value columns — fill it with random integer rows, then
//! for a random non-empty subset of dims + metrics compare the semantic-view
//! expansion (`expand`) against an independently hand-written `GROUP BY` query
//! over the same physical table.
//!
//! The two result sets are multiset-compared *inside `DuckDB`* via a symmetric
//! `EXCEPT ALL` difference, which is type-agnostic (no Rust-side row decoding)
//! and order-independent (both sides are projected in a canonical column order
//! keyed by output alias, so a column-ordering difference between the two
//! formulations is not a false diff).
//!
//! Scope: base-table metrics, single grain, integer data + exact aggregates
//! (`SUM`/`COUNT`/`MIN`/`MAX` — no floating point, so equality is exact).
//! Joins, semi-additive, window, wildcard, and USING paths are exercised by
//! the fixed-schema Python differential harness (`test/integration/
//! test_differential.py`, extended per T-1); this test adds schema + data +
//! query randomization for the core path and runs under plain `cargo test`.
//!
//! PBT-6 (code-review 2026-08-03): the generated query now also carries a
//! randomized pre-aggregation `where_clause`. Until then every harness in
//! `tests/` pinned `where_clause: None`, so the newest number-changing feature
//! had no randomized coverage at all — the blind spot EXP-9/EXP-10 reached
//! `main` through. The predicate is generated as an AST and rendered **twice**:
//! once in member names for the request, once as raw columns for the oracle,
//! which is what keeps the two formulations independent.
//!
//! Two properties of the feature make the generator worth more than its size:
//!
//! * **Substitution is not identity.** Alongside the plain dimensions (whose
//!   expression *is* their column, so a bug there would be invisible) the
//!   definition declares one **filter member** per dimension — `LABELS =
//!   (FILTER)`, expression `d{i} = 0 OR d{i} = 2`. Nothing in the physical
//!   table is called `f{i}`, so a predicate naming one only binds if the
//!   member's expression was actually spliced in.
//! * **Precedence is load-bearing.** A filter member is emitted BARE into a
//!   surrounding operator expression, so `f0 AND d1 = 0` is correct only if the
//!   splice parenthesizes: `(d0 = 0 OR d0 = 2) AND d1 = 0`, not the
//!   `d0 = 0 OR (d0 = 2 AND d1 = 0)` a plain textual replacement would give.
//!   The oracle renders every operand explicitly parenthesized, so the two
//!   agree only if the splice got this right.
//!
//! NULL handling needs no special care in the oracle: both sides are evaluated
//! by the same engine under the same three-valued logic, and `WHERE` keeps only
//! TRUE on both. The comparison literals straddle the dimension domain (`0..3`
//! plus NULL), so cases range from always-true through selective to always-false
//! — including predicates that filter every row away.

use proptest::prelude::*;
use semantic_views::expand::{
    expand, output_column_names, DimensionName, MetricName, QueryRequest,
};
use semantic_views::model::{
    AccessModifier, Dimension, Fact, Metric, SemanticViewDefinition, TableRef,
};
use semantic_views::query::items::{Group, OutputColumns};
use semantic_views::query::wire::build_projected_sql;

/// Comparison operators used by generated predicates.
#[derive(Debug, Clone, Copy)]
enum CmpOp {
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
}

impl CmpOp {
    fn to_sql(self) -> &'static str {
        match self {
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Eq => "=",
            CmpOp::Ne => "<>",
            CmpOp::Ge => ">=",
            CmpOp::Gt => ">",
        }
    }
}

/// A generated pre-aggregation predicate over the schema's members.
///
/// Rendered two ways — [`Pred::to_member_sql`] for the `where_clause`
/// parameter, [`Pred::to_raw_sql`] for the hand-written oracle — so the
/// comparison stays a differential one rather than the same string twice.
#[derive(Debug, Clone)]
enum Pred {
    /// `d{i} <op> <literal>`
    Cmp(usize, CmpOp, i64),
    /// `d{i} IS NULL`
    IsNull(usize),
    /// `d{i} IS NOT NULL`
    IsNotNull(usize),
    /// A reference to filter member `f{i}` — the substitution under test.
    Filter(usize),
    /// A reference to CHAINED fact `fb{i}`, whose expression names another fact
    /// (`fa{i}`). EXP-23: the `where_clause` fact branch spliced a fact's
    /// expression in VERBATIM, with no inlining pass, so the inner fact's name
    /// survived into the emitted SQL as a bare column.
    ChainedFact(usize),
    /// `d{i} IN (<uncorrelated subquery>)`, whose body names leaf fact `fa{i}`
    /// as its own column. A subquery is its own scope: the name inside it is
    /// the subquery's, and substituting the member there turned it into a
    /// correlated subquery (or a syntax error, in its alias position).
    InSubquery(usize, i64),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
}

/// The expression declared for filter member `f{i}`. Deliberately a compound
/// `OR` at the top level: spliced into a surrounding `AND` it changes meaning
/// unless the splice parenthesizes it.
fn filter_expr(i: usize) -> String {
    format!("d{i} = 0 OR d{i} = 2")
}

/// Leaf fact `fa{i}` — an ordinary column expression.
fn leaf_fact_expr(i: usize) -> String {
    format!("d{i} + 1")
}

/// Chained fact `fb{i}` — references the leaf fact BY NAME, so resolving it
/// needs a topological inlining pass rather than one substitution round
/// (EXP-23).
fn chained_fact_expr(i: usize) -> String {
    format!("fa{i} * 2")
}

impl Pred {
    /// The `where_clause` text: member **names**, with composites parenthesized
    /// so the SQL parse matches this AST. A filter member is emitted BARE —
    /// that is precisely the surface under test.
    fn to_member_sql(&self) -> String {
        match self {
            Pred::Cmp(i, op, k) => format!("d{i} {} {k}", op.to_sql()),
            Pred::IsNull(i) => format!("d{i} IS NULL"),
            Pred::IsNotNull(i) => format!("d{i} IS NOT NULL"),
            Pred::Filter(i) => format!("f{i}"),
            Pred::ChainedFact(i) => format!("fb{i} > 0"),
            Pred::InSubquery(i, k) => format!(
                "d{i} IN (SELECT x FROM (SELECT d{i} AS x, d{i} + 1 AS fa{i} FROM t) AS s \
                 WHERE fa{i} > {k})"
            ),
            Pred::And(a, b) => format!("({} AND {})", a.to_member_sql(), b.to_member_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_member_sql(), b.to_member_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_member_sql()),
        }
    }

    /// The oracle's independent formulation: raw physical columns, filter
    /// members expanded inline, every operand explicitly parenthesized so the
    /// intended grouping is unambiguous without relying on operator precedence.
    fn to_raw_sql(&self) -> String {
        match self {
            Pred::Cmp(i, op, k) => format!("(d{i} {} {k})", op.to_sql()),
            Pred::IsNull(i) => format!("(d{i} IS NULL)"),
            Pred::IsNotNull(i) => format!("(d{i} IS NOT NULL)"),
            Pred::Filter(i) => format!("({})", filter_expr(*i)),
            // The oracle expands the chain by hand — leaf inlined into
            // chained — so the comparison stays differential rather than
            // re-using the expander's own inlining.
            Pred::ChainedFact(i) => format!("((({}) * 2) > 0)", leaf_fact_expr(*i)),
            Pred::InSubquery(i, k) => {
                format!("(d{i} IN (SELECT d{i} FROM t WHERE (d{i} + 1) > {k}))")
            }
            Pred::And(a, b) => format!("({} AND {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Or(a, b) => format!("({} OR {})", a.to_raw_sql(), b.to_raw_sql()),
            Pred::Not(a) => format!("(NOT {})", a.to_raw_sql()),
        }
    }

    /// Whether this predicate names at least one filter member — used only to
    /// assert the generator is not producing filter-free predicates every time
    /// (see `generator_varies_the_predicate_and_exercises_filter_members`).
    fn references_filter(&self) -> bool {
        match self {
            Pred::Filter(_) => true,
            Pred::Cmp(..)
            | Pred::IsNull(_)
            | Pred::IsNotNull(_)
            | Pred::ChainedFact(_)
            | Pred::InSubquery(..) => false,
            Pred::And(a, b) | Pred::Or(a, b) => a.references_filter() || b.references_filter(),
            Pred::Not(a) => a.references_filter(),
        }
    }

    /// Whether this predicate names a chained fact — the EXP-23 surface. Same
    /// anti-vacuity role as [`Pred::references_filter`]: a `ChainedFact` arm
    /// that the generator never emits is not coverage.
    fn references_chained_fact(&self) -> bool {
        match self {
            Pred::ChainedFact(_) => true,
            Pred::Cmp(..)
            | Pred::IsNull(_)
            | Pred::IsNotNull(_)
            | Pred::Filter(_)
            | Pred::InSubquery(..) => false,
            Pred::And(a, b) | Pred::Or(a, b) => {
                a.references_chained_fact() || b.references_chained_fact()
            }
            Pred::Not(a) => a.references_chained_fact(),
        }
    }

    /// Whether this predicate holds a subquery. Same anti-vacuity role.
    fn has_subquery(&self) -> bool {
        match self {
            Pred::InSubquery(..) => true,
            Pred::Cmp(..)
            | Pred::IsNull(_)
            | Pred::IsNotNull(_)
            | Pred::Filter(_)
            | Pred::ChainedFact(_) => false,
            Pred::And(a, b) | Pred::Or(a, b) => a.has_subquery() || b.has_subquery(),
            Pred::Not(a) => a.has_subquery(),
        }
    }
}

fn arb_cmp_op() -> impl Strategy<Value = CmpOp> {
    prop_oneof![
        Just(CmpOp::Lt),
        Just(CmpOp::Le),
        Just(CmpOp::Eq),
        Just(CmpOp::Ne),
        Just(CmpOp::Ge),
        Just(CmpOp::Gt),
    ]
}

/// Predicates over `n_dims` dimensions. Literals span `-1..=3` against a
/// dimension domain of `0..3` + NULL, so the generated set includes
/// always-true, selective, and everything-filtered-away cases.
fn arb_pred(n_dims: usize) -> impl Strategy<Value = Pred> {
    let leaf = prop_oneof![
        3 => (0..n_dims, arb_cmp_op(), -1i64..=3)
            .prop_map(|(i, op, k)| Pred::Cmp(i, op, k)),
        1 => (0..n_dims).prop_map(Pred::IsNull),
        1 => (0..n_dims).prop_map(Pred::IsNotNull),
        3 => (0..n_dims).prop_map(Pred::Filter),
        3 => (0..n_dims).prop_map(Pred::ChainedFact),
        2 => (0..n_dims, -1i64..=3).prop_map(|(i, k)| Pred::InSubquery(i, k)),
    ];
    // depth 3, up to 8 nodes, branching 2 — deep enough for a filter member to
    // land inside a surrounding AND/OR/NOT, which is where precedence bites.
    leaf.prop_recursive(3, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            inner.prop_map(|a| Pred::Not(Box::new(a))),
        ]
    })
}

/// An aggregate over a generated column (or `COUNT(*)`), rendered to SQL.
///
/// `CountCol` (`count(v{j})`) counts only non-NULL rows, so with NULL-bearing
/// value columns it diverges from `Count` (`count(*)`) — the two are now
/// differentially distinguished (previously only `count(*)` existed).
#[derive(Debug, Clone)]
enum Agg {
    Sum(usize),
    Count,
    CountCol(usize),
    Min(usize),
    Max(usize),
}

impl Agg {
    fn to_sql(&self) -> String {
        match self {
            Agg::Sum(j) => format!("sum(v{j})"),
            Agg::Count => "count(*)".to_string(),
            Agg::CountCol(j) => format!("count(v{j})"),
            Agg::Min(j) => format!("min(v{j})"),
            Agg::Max(j) => format!("max(v{j})"),
        }
    }
}

/// A generated schema shape plus its data. Columns are `d0..d{n_dims-1}`
/// (group-by dimensions) followed by `v0..v{n_vals-1}` (metric inputs), all
/// `INTEGER`. `rows` holds `n_dims + n_vals` cells per row; `None` is a SQL
/// `NULL` (exercises NULL group keys and NULL aggregate inputs).
#[derive(Debug, Clone)]
struct Schema {
    n_dims: usize,
    n_vals: usize,
    metric_aggs: Vec<Agg>,
    rows: Vec<Vec<Option<i64>>>,
}

/// A full test case: a schema plus the non-empty subset of dims and metrics to
/// query (indices into the schema's dimension list / `metric_aggs`), and the
/// optional pre-aggregation predicate (PBT-6). `None` keeps the original
/// no-`where_clause` coverage rather than replacing it.
#[derive(Debug, Clone)]
struct Case {
    schema: Schema,
    sel_dims: Vec<usize>,
    sel_metrics: Vec<usize>,
    where_pred: Option<Pred>,
    /// The column layout `SEMANTIC_VIEW(...)` asks for: the clause order, and
    /// which selected members (dimensions, then metrics, in request order)
    /// are renamed with an alias.
    order: [Group; 3],
    aliased: Vec<bool>,
}

impl Case {
    /// Each selected member's output name: `x_<member>` when aliased.
    fn output_names(&self) -> Vec<String> {
        self.sel_dims
            .iter()
            .map(|i| format!("d{i}"))
            .chain(self.sel_metrics.iter().map(|i| format!("m{i}")))
            .zip(&self.aliased)
            .map(|(name, aliased)| if *aliased { format!("x_{name}") } else { name })
            .collect()
    }

    fn is_default_layout(&self) -> bool {
        self.order == Group::DEFAULT_ORDER && !self.aliased.contains(&true)
    }
}

/// Every order of the three clauses.
fn arb_order() -> impl Strategy<Value = [Group; 3]> {
    use Group::{Dimensions as D, Facts as F, Metrics as M};
    prop::sample::select(vec![
        [D, M, F],
        [D, F, M],
        [M, D, F],
        [M, F, D],
        [F, D, M],
        [F, M, D],
    ])
}

fn arb_schema() -> impl Strategy<Value = Schema> {
    (1usize..=3, 1usize..=3).prop_flat_map(|(n_dims, n_vals)| {
        let agg = prop_oneof![
            (0..n_vals).prop_map(Agg::Sum),
            Just(Agg::Count),
            (0..n_vals).prop_map(Agg::CountCol),
            (0..n_vals).prop_map(Agg::Min),
            (0..n_vals).prop_map(Agg::Max),
        ];
        let metrics = prop::collection::vec(agg, 1..=3);
        // Dimension cells: a small domain (0..3) so rows collide into real
        // groups, plus `None` (NULL) so NULL group keys are exercised.
        let dim_cell = prop_oneof![
            1 => Just(None),
            5 => (0i64..3).prop_map(Some),
        ];
        // Value cells: `None` (NULL, so SUM-over-all-NULL and COUNT(col) vs
        // COUNT(*) diverge), a small signed domain (exact sums / collisions),
        // and a large signed magnitude — both spanning negatives. The domain
        // stays within INT32 so it fits the `INTEGER` columns; SUM widens to
        // HUGEINT in DuckDB so no aggregate overflow arises.
        let val_cell = prop_oneof![
            1 => Just(None),
            2 => (-5i64..=5).prop_map(Some),
            2 => (-1_000_000_000i64..=1_000_000_000).prop_map(Some),
        ];
        let row = (
            prop::collection::vec(dim_cell, n_dims),
            prop::collection::vec(val_cell, n_vals),
        )
            .prop_map(|(mut cells, vals)| {
                cells.extend(vals);
                cells
            });
        // 0 rows is allowed — the empty-table path (global aggregate over no
        // rows, empty DISTINCT) was never differentially checked before.
        let rows = prop::collection::vec(row, 0..=25);
        (Just(n_dims), Just(n_vals), metrics, rows).prop_map(
            |(n_dims, n_vals, metric_aggs, rows)| Schema {
                n_dims,
                n_vals,
                metric_aggs,
                rows,
            },
        )
    })
}

fn arb_case() -> impl Strategy<Value = Case> {
    arb_schema().prop_flat_map(|schema| {
        let nd = schema.n_dims;
        let nm = schema.metric_aggs.len();
        // Either selection may be empty (dims-only → SELECT DISTINCT,
        // metrics-only → global aggregate), but not both — a fully-empty
        // request is invalid, so the at-least-one invariant is preserved by
        // the filter below.
        let dim_sel = prop::sample::subsequence((0..nd).collect::<Vec<_>>(), 0..=nd);
        let met_sel = prop::sample::subsequence((0..nm).collect::<Vec<_>>(), 0..=nm);
        // PBT-6: `None` one time in four keeps the original no-predicate path
        // covered; the rest carry a generated predicate. Note the predicate may
        // reference dimensions that are NOT selected — the case an outer SQL
        // `WHERE` cannot express, and the whole reason the parameter exists.
        let where_pred = prop_oneof![
            1 => Just(None),
            3 => arb_pred(nd).prop_map(Some),
        ];
        // The layout `SEMANTIC_VIEW(...)` lowers to: half the cases keep the
        // default, so the plain execution SQL stays covered.
        let layout = prop_oneof![
            1 => Just(([Group::Dimensions, Group::Metrics, Group::Facts], 0u8)),
            1 => (arb_order(), any::<u8>()),
        ];
        (Just(schema), dim_sel, met_sel, where_pred, layout)
            .prop_filter(
                "at least one of dimensions/metrics must be selected",
                |(_, sel_dims, sel_metrics, _, _)| !sel_dims.is_empty() || !sel_metrics.is_empty(),
            )
            .prop_map(
                |(schema, sel_dims, sel_metrics, where_pred, (order, mask))| {
                    let aliased = (0..sel_dims.len() + sel_metrics.len())
                        .map(|k| mask & (1 << k) != 0)
                        .collect();
                    Case {
                        schema,
                        sel_dims,
                        sel_metrics,
                        where_pred,
                        order,
                        aliased,
                    }
                },
            )
    })
}

/// Build the semantic-view definition for a generated schema: base table `t`,
/// one dimension per `d{i}` (expr == column), one metric per generated agg.
fn build_def(s: &Schema) -> SemanticViewDefinition {
    // Selectable dimensions `d{i}` (expression == column), followed by one
    // filter member `f{i}` per dimension (PBT-6). The filter members are never
    // selected as output — they exist to be named by a `where_clause`, which is
    // what `LABELS = (FILTER)` declares — and their expressions are compound,
    // so a splice that loses the parentheses changes the answer.
    let dimensions = (0..s.n_dims)
        .map(|i| Dimension {
            name: format!("d{i}"),
            expr: format!("d{i}"),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
        })
        .chain((0..s.n_dims).map(|i| Dimension {
            name: format!("f{i}"),
            expr: filter_expr(i),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: true,
        }))
        .collect();
    let metrics = s
        .metric_aggs
        .iter()
        .enumerate()
        .map(|(i, agg)| Metric {
            name: format!("m{i}"),
            expr: agg.to_sql(),
            source_table: None,
            output_type: None,
            using_relationships: vec![],
            comment: None,
            synonyms: vec![],
            access: AccessModifier::Public,
            non_additive_by: vec![],
            window_spec: None,
        })
        .collect();
    SemanticViewDefinition {
        tables: vec![TableRef {
            alias: "t".to_string(),
            table: "t".to_string(),
            pk_columns: vec![],
            unique_constraints: vec![],
            comment: None,
            synonyms: vec![],
        }],
        dimensions,
        metrics,
        joins: vec![],
        facts: (0..s.n_dims)
            .flat_map(|i| {
                [
                    (format!("fa{i}"), leaf_fact_expr(i)),
                    (format!("fb{i}"), chained_fact_expr(i)),
                ]
            })
            .map(|(name, expr)| Fact {
                name,
                expr,
                source_table: Some("t".to_string()),
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                access: AccessModifier::Public,
            })
            .collect(),
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        comment: None,
    }
}

/// Create the physical base table `t` and insert the generated rows.
fn make_db(s: &Schema) -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    let mut cols: Vec<String> = (0..s.n_dims).map(|i| format!("d{i} INTEGER")).collect();
    cols.extend((0..s.n_vals).map(|j| format!("v{j} INTEGER")));
    conn.execute_batch(&format!("CREATE TABLE t ({});", cols.join(", ")))
        .expect("create table t");
    let values: Vec<String> = s
        .rows
        .iter()
        .map(|r| {
            format!(
                "({})",
                r.iter()
                    .map(|c| c.map_or_else(|| "NULL".to_string(), |v| v.to_string()))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect();
    if !values.is_empty() {
        conn.execute_batch(&format!("INSERT INTO t VALUES {};", values.join(",")))
            .expect("insert rows");
    }
    conn
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// The semantic-view expansion of a random core query returns exactly the
    /// same rows (as a multiset) as an independently hand-written GROUP BY over
    /// the same physical table.
    #[test]
    fn expansion_matches_handwritten_group_by(case in arb_case()) {
        let def = build_def(&case.schema);

        let req = QueryRequest {
            where_clause: case.where_pred.as_ref().map(Pred::to_member_sql),
            dimensions: case
                .sel_dims
                .iter()
                .map(|i| DimensionName::new(format!("d{i}")))
                .collect(),
            metrics: case
                .sel_metrics
                .iter()
                .map(|i| MetricName::new(format!("m{i}")))
                .collect(),
            // PIN: the row-level FACTS path is oracled by child_grain_proptest (below-base grain) and metamorphic_proptest; this harness oracles single-table aggregation. TECH-DEBT #66
            facts: vec![],
        };

        let expanded = expand("t_diff", &def, &req)
            .expect("expand must succeed for a core base-table definition");

        let conn = make_db(&case.schema);

        // The column layout `SEMANTIC_VIEW(...)` asks for (clause order and
        // aliases), applied as `semantic_view()` applies it: by name over the
        // expansion's columns. The names `semantic_view()` works them out from
        // must be the ones the expansion's columns really have, which a LIMIT 0
        // probe reads.
        let names = output_column_names("t_diff", &def, &req).expect("request resolves");
        let mut probe = conn
            .prepare(&format!("SELECT * FROM ({expanded}) q LIMIT 0"))
            .expect("probe must prepare");
        probe.execute([]).expect("probe must run");
        prop_assert_eq!(&names, &probe.column_names());
        let outputs = case.output_names();
        let n_dims = case.sel_dims.len();
        let alias_of = |k: usize| case.aliased[k].then(|| outputs[k].clone());
        let dim_aliases: Vec<Option<String>> = (0..n_dims).map(alias_of).collect();
        let met_aliases: Vec<Option<String>> = (n_dims..outputs.len()).map(alias_of).collect();
        let layout = OutputColumns::new(&names, [&dim_aliases, &met_aliases, &[]], case.order)
            .expect("generated output names are distinct");
        let expanded = build_projected_sql(&expanded, &names, &layout);

        // The columns come out in the clause order the request gives, each
        // under its output name. The value comparison below matches columns by
        // name, so it checks which values each name carries, and this checks
        // where each name is.
        let expected_order: Vec<String> = case
            .order
            .iter()
            .flat_map(|group| match group {
                Group::Dimensions => outputs[..n_dims].to_vec(),
                Group::Metrics => outputs[n_dims..].to_vec(),
                Group::Facts => vec![],
            })
            .collect();
        let mut laid_out = conn
            .prepare(&format!("SELECT * FROM ({expanded}) q LIMIT 0"))
            .expect("the laid-out expansion must prepare");
        laid_out.execute([]).expect("the laid-out expansion must run");
        prop_assert_eq!(laid_out.column_names(), expected_order);

        // Independent oracle: a plain GROUP BY over the same table, naming
        // each output column by the name the request gives it.
        let dim_items: Vec<String> = case
            .sel_dims
            .iter()
            .zip(&outputs)
            .map(|(i, out)| format!("d{i} AS {out}"))
            .collect();
        let met_items: Vec<String> = case
            .sel_metrics
            .iter()
            .zip(&outputs[n_dims..])
            .map(|(i, out)| format!("{} AS {out}", case.schema.metric_aggs[*i].to_sql()))
            .collect();
        let select_items = dim_items
            .iter()
            .chain(met_items.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        // Metrics-only requests are a single global-aggregate row (no GROUP
        // BY). Otherwise GROUP BY the selected dimensions by ordinal — they are
        // projected first, so positions 1..=sel_dims.len(). Dims-only is a
        // GROUP BY over all selected dims, multiset-equal to the expansion's
        // SELECT DISTINCT.
        // PBT-6: the predicate is applied BEFORE the grouping in the oracle
        // too -- that is what `where_clause` means, and an outer WHERE over the
        // result would be a different (and, for an unselected member, an
        // inexpressible) query.
        let where_sql = case
            .where_pred
            .as_ref()
            .map_or_else(String::new, |p| format!(" WHERE {}", p.to_raw_sql()));
        let oracle = if case.sel_dims.is_empty() {
            format!("SELECT {select_items} FROM t{where_sql}")
        } else {
            let group_by = (1..=case.sel_dims.len())
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!("SELECT {select_items} FROM t{where_sql} GROUP BY {group_by}")
        };

        // Canonical projection (columns sorted by output name) so a column
        // ORDER difference between the two formulations is not a false diff.
        let mut proj_cols = outputs.clone();
        proj_cols.sort();
        let proj = proj_cols.join(", ");

        // Symmetric multiset difference inside DuckDB: 0 iff the two result
        // sets are equal as multisets.
        let cmp = format!(
            "SELECT \
               (SELECT count(*) FROM (SELECT {proj} FROM ({expanded}) qa \
                                      EXCEPT ALL \
                                      SELECT {proj} FROM ({oracle}) qb) e1) \
             + (SELECT count(*) FROM (SELECT {proj} FROM ({oracle}) qc \
                                      EXCEPT ALL \
                                      SELECT {proj} FROM ({expanded}) qd) e2) AS diff"
        );

        let diff: i64 = conn.query_row(&cmp, [], |r| r.get(0)).unwrap_or_else(|e| {
            panic!("differential comparison query failed: {e}\n--- expanded:\n{expanded}\n--- oracle:\n{oracle}")
        });

        prop_assert_eq!(
            diff, 0,
            "semantic-view expansion disagrees with hand-written GROUP BY \
             (symmetric multiset diff = {})\n--- expanded:\n{}\n--- oracle:\n{}",
            diff, expanded, oracle
        );
    }
}

/// PBT-6 guard: prove the generator actually *varies* the new parameter.
///
/// CLAUDE.md's rule is that a field being present in a struct literal is not
/// coverage — the generator has to vary it. `where_clause: None` pinned in
/// every harness is exactly what that warns about, and it looked identical to
/// real coverage from the diff. This test samples the strategy and fails if the
/// predicate ever collapses back to a constant: no predicates, no *absent*
/// predicates (the original path), no filter-member references (the
/// substitution surface), or too few distinct shapes.
///
/// It asserts on the generator, not on query results, so it stays meaningful
/// even if the differential property above were somehow satisfied vacuously.
#[test]
fn generator_varies_the_predicate_and_exercises_filter_members() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::HashSet;

    let mut runner = TestRunner::deterministic();
    let mut with_pred = 0usize;
    let mut without_pred = 0usize;
    let mut with_filter = 0usize;
    let mut with_chained_fact = 0usize;
    let mut with_subquery = 0usize;
    let mut compound = 0usize;
    let mut distinct: HashSet<String> = HashSet::new();
    let mut default_layout = 0usize;
    let mut reordered = 0usize;
    let mut aliased = 0usize;

    for _ in 0..300 {
        let case = arb_case()
            .new_tree(&mut runner)
            .expect("strategy must produce a value")
            .current();
        if case.is_default_layout() {
            default_layout += 1;
        }
        // A reorder moves columns only when both dimensions and metrics are
        // selected and metrics come before dimensions.
        let pos = |g| case.order.iter().position(|o| *o == g);
        if !case.sel_dims.is_empty()
            && !case.sel_metrics.is_empty()
            && pos(Group::Metrics) < pos(Group::Dimensions)
        {
            reordered += 1;
        }
        if case.aliased.contains(&true) {
            aliased += 1;
        }
        match &case.where_pred {
            None => without_pred += 1,
            Some(p) => {
                with_pred += 1;
                if p.references_filter() {
                    with_filter += 1;
                }
                if p.references_chained_fact() {
                    with_chained_fact += 1;
                }
                if p.has_subquery() {
                    with_subquery += 1;
                }
                let sql = p.to_member_sql();
                if sql.contains(" AND ") || sql.contains(" OR ") || sql.contains("NOT ") {
                    compound += 1;
                }
                distinct.insert(sql);
            }
        }
    }

    assert!(
        with_pred > 0,
        "generator never produced a where_clause -- the parameter is inert again"
    );
    assert!(
        without_pred > 0,
        "generator never produced the None case -- the original no-predicate \
         path lost its coverage"
    );
    assert!(
        with_filter > 0,
        "generator never referenced a filter member -- the member-substitution \
         and parenthesization surface is untested"
    );
    assert!(
        with_chained_fact > 0,
        "generator never referenced a chained fact -- EXP-23's surface (a fact \
         whose expression names another fact) is untested"
    );
    assert!(
        with_subquery > 0,
        "generator never produced a subquery -- a subquery's own scope is untested"
    );
    assert!(
        compound > 0,
        "generator never produced a compound predicate -- precedence around a \
         spliced member expression is untested"
    );
    assert!(
        default_layout > 0 && reordered > 0 && aliased > 0,
        "generator never varied the column layout (default {default_layout}, \
         reordered {reordered}, aliased {aliased}) -- SEMANTIC_VIEW(...)'s clause \
         order and aliases are untested"
    );
    assert!(
        distinct.len() > 50,
        "generator produced only {} distinct predicates out of {with_pred}; \
         the search space has collapsed",
        distinct.len()
    );
}
