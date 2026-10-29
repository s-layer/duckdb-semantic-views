//! Phase 6.6: `CREATE OR ALTER` keeps a materialization only while its table
//! still answers the query that routes to it.
//!
//! Each case generates a table of random rows, a view over it with a
//! materialization covering a random choice of its dimensions and metrics, and
//! a random edit of the view: a member's expression changed, a member dropped,
//! renamed or added, the table alias changed, or nothing but the comment. The
//! materialization's table is filled with what the *old* view answers, so it is
//! correct before the edit. [`semantic_views::or_alter::merge`] then decides
//! whether the altered view keeps it.
//!
//! Two independent checks:
//!
//! * **Numbers.** When the materialization is kept, the routed query (which
//!   reads its table) returns the same rows, as a multiset compared inside
//!   `DuckDB`, as the altered view computes from the base table with no
//!   materialization. A kept table that the edit made stale shows up here as
//!   wrong numbers, whatever the reason the merge kept it.
//! * **The rule.** Each edit names the members it changes, written down by
//!   hand (a changed fact changes the metric reading it). The materialization
//!   is kept exactly when the edit changes none of the members it covers.

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, MetricName, QueryRequest};
use semantic_views::model::{Materialization, SemanticViewDefinition};
use semantic_views::or_alter::merge;
use semantic_views::parse::{plan_create, RewriteAction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Nothing,
    Comment,
    Fact,
    D0Expr,
    D1Expr,
    M0Agg,
    M1Agg,
    DropD1,
    DropM1,
    RenameD1,
    AddMetric,
    TableAlias,
}

impl Edit {
    const ALL: [Edit; 12] = [
        Edit::Nothing,
        Edit::Comment,
        Edit::Fact,
        Edit::D0Expr,
        Edit::D1Expr,
        Edit::M0Agg,
        Edit::M1Agg,
        Edit::DropD1,
        Edit::DropM1,
        Edit::RenameD1,
        Edit::AddMetric,
        Edit::TableAlias,
    ];

    /// The members whose answer the edit may change.
    fn changes(self) -> &'static [&'static str] {
        match self {
            Edit::Nothing | Edit::Comment | Edit::AddMetric => &[],
            Edit::Fact | Edit::M0Agg => &["m0"],
            Edit::D0Expr => &["d0"],
            Edit::D1Expr | Edit::DropD1 | Edit::RenameD1 => &["d1"],
            Edit::M1Agg | Edit::DropM1 => &["m1"],
            Edit::TableAlias => &["d0", "d1", "m0", "m1"],
        }
    }
}

/// The view's DDL body, as `edit` leaves it (`None`: the original).
fn body(edit: Option<Edit>) -> String {
    let is = |e: Edit| edit == Some(e);
    let a = if is(Edit::TableAlias) { "u" } else { "t" };
    let fact = if is(Edit::Fact) {
        format!("{a}.v0 + 1")
    } else {
        format!("{a}.v0")
    };
    let d0 = if is(Edit::D0Expr) {
        format!("{a}.d0 + 1")
    } else {
        format!("{a}.d0")
    };
    let d1 = if is(Edit::D1Expr) {
        format!("{a}.d1 * 2")
    } else {
        format!("{a}.d1")
    };
    let m0 = if is(Edit::M0Agg) {
        format!("MIN({a}.fv)")
    } else {
        format!("SUM({a}.fv)")
    };
    let m1 = if is(Edit::M1Agg) {
        format!("COUNT({a}.v1)")
    } else {
        format!("MAX({a}.v1)")
    };
    let mut dims = vec![format!("{a}.d0 AS {d0}")];
    if !is(Edit::DropD1) {
        let name = if is(Edit::RenameD1) { "e1" } else { "d1" };
        dims.push(format!("{a}.{name} AS {d1}"));
    }
    let mut mets = vec![format!("{a}.m0 AS {m0}")];
    if !is(Edit::DropM1) {
        mets.push(format!("{a}.m1 AS {m1}"));
    }
    if is(Edit::AddMetric) {
        mets.push(format!("{a}.m2 AS SUM({a}.v1)"));
    }
    let comment = if is(Edit::Comment) {
        " COMMENT = 'edited'"
    } else {
        ""
    };
    format!(
        "TABLES ({a} AS t PRIMARY KEY (id)) FACTS ({a}.fv AS {fact}) DIMENSIONS ({}) \
         METRICS ({}){comment}",
        dims.join(", "),
        mets.join(", ")
    )
}

fn def(body: &str) -> SemanticViewDefinition {
    match plan_create(&format!("CREATE SEMANTIC VIEW v {body}")).expect("the DDL plans") {
        Some(RewriteAction::Create { def, .. }) => *def,
        other => panic!("not a CREATE: {other:?}"),
    }
}

#[derive(Debug, Clone)]
struct Case {
    rows: Vec<(Option<i64>, Option<i64>, i64, Option<i64>)>,
    mat_dims: Vec<&'static str>,
    mat_metrics: Vec<&'static str>,
    edit: Edit,
}

fn arb_subset(names: &'static [&'static str]) -> impl Strategy<Value = Vec<&'static str>> {
    proptest::sample::subsequence(names, 1..=names.len())
}

fn arb_case() -> impl Strategy<Value = Case> {
    let cell = || prop_oneof![1 => Just(None), 4 => (0i64..3).prop_map(Some)];
    (
        proptest::collection::vec(
            (
                cell(),
                cell(),
                -5i64..10,
                prop_oneof![1 => Just(None), 4 => (-5i64..10).prop_map(Some)],
            ),
            0..16,
        ),
        arb_subset(&["d0", "d1"]),
        arb_subset(&["m0", "m1"]),
        proptest::sample::select(&Edit::ALL[..]),
    )
        .prop_map(|(rows, mat_dims, mat_metrics, edit)| Case {
            rows,
            mat_dims,
            mat_metrics,
            edit,
        })
}

fn request(case: &Case) -> QueryRequest {
    QueryRequest {
        dimensions: case
            .mat_dims
            .iter()
            .map(|d| DimensionName::new(*d))
            .collect(),
        metrics: case
            .mat_metrics
            .iter()
            .map(|m| MetricName::new(*m))
            .collect(),
        // Routing takes only a materialization's exact request, with no facts and no
        // where_clause (`expand::materialization`), so no other request reads its table.
        // PIN: a request with facts is never routed to a materialization.
        facts: vec![],
        // PIN: a request with a where_clause is never routed to a materialization.
        where_clause: None,
    }
}

/// The view before the edit, with the materialization, and the view after it,
/// merged.
fn old_and_merged(case: &Case) -> (SemanticViewDefinition, SemanticViewDefinition) {
    let mut old = def(&body(None));
    old.materializations = vec![Materialization {
        name: "mat".to_string(),
        table: "mat".to_string(),
        dimensions: case.mat_dims.iter().map(ToString::to_string).collect(),
        metrics: case.mat_metrics.iter().map(ToString::to_string).collect(),
    }];
    let merged = merge("v", def(&body(Some(case.edit))), Some(&old));
    (old, merged)
}

fn kept_by_the_rule(case: &Case) -> bool {
    !case
        .edit
        .changes()
        .iter()
        .any(|m| case.mat_dims.contains(m) || case.mat_metrics.contains(m))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn a_kept_materialization_answers_as_the_altered_view(case in arb_case()) {
        let (old, merged) = old_and_merged(&case);
        let kept = !merged.materializations.is_empty();
        prop_assert_eq!(kept, kept_by_the_rule(&case), "edit {:?}", case.edit);
        if !kept {
            return Ok(());
        }

        let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
        conn.execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, d0 INTEGER, d1 INTEGER, v0 INTEGER, v1 INTEGER)",
        )
        .expect("create t");
        for (i, (d0, d1, v0, v1)) in case.rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO t VALUES (?, ?, ?, ?, ?)",
                duckdb::params![i64::try_from(i).unwrap(), d0, d1, v0, v1],
            )
            .expect("insert a row");
        }
        let req = request(&case);
        let unrouted = |d: &SemanticViewDefinition| SemanticViewDefinition {
            materializations: Vec::new(),
            ..d.clone()
        };
        // The table holds what the view answered before the edit.
        let before = expand("v", &unrouted(&old), &req).expect("the old view expands");
        conn.execute_batch(&format!("CREATE TABLE mat AS {before}"))
            .expect("fill the materialization");

        let routed = expand("v", &merged, &req).expect("the merged view expands");
        prop_assert!(routed.contains("FROM \"mat\"") || routed.contains("FROM mat"), "not routed: {}", routed);
        let computed = expand("v", &unrouted(&merged), &req).expect("the altered view expands");
        let cmp = format!(
            "SELECT (SELECT count(*) FROM (({routed}) EXCEPT ALL ({computed}))) \
                  + (SELECT count(*) FROM (({computed}) EXCEPT ALL ({routed})))"
        );
        let diff: i64 = conn
            .query_row(&cmp, [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("comparison failed: {e}\n{cmp}"));
        prop_assert_eq!(diff, 0, "edit {:?}\n--- routed:\n{}\n--- computed:\n{}", case.edit, routed, computed);
    }
}

/// The generator reaches every edit, and both outcomes: kept materializations
/// (whose numbers the property compares) and dropped ones.
#[test]
fn the_generator_reaches_every_edit_and_both_outcomes() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::HashSet;

    let mut runner = TestRunner::deterministic();
    let mut edits = HashSet::new();
    let (mut kept, mut dropped, mut kept_after_change) = (0, 0, 0);
    for _ in 0..512 {
        let case = arb_case().new_tree(&mut runner).unwrap().current();
        edits.insert(format!("{:?}", case.edit));
        if kept_by_the_rule(&case) {
            kept += 1;
            if !case.edit.changes().is_empty() {
                kept_after_change += 1;
            }
        } else {
            dropped += 1;
        }
    }
    assert_eq!(edits.len(), Edit::ALL.len(), "{edits:?}");
    assert!(kept > 50 && dropped > 50, "kept {kept}, dropped {dropped}");
    // An edit to a member the materialization does not cover keeps it.
    assert!(kept_after_change > 20, "{kept_after_change}");
}
