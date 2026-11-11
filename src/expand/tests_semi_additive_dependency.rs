//! A semi-additive metric reached THROUGH another metric (EXP-19 / EXP-20,
//! code-review 2026-08-06).
//!
//! `is_active_semi_additive` is the routing predicate that decides whether a
//! query takes the `RANK`-CTE snapshot path. It inspects only the metric's OWN
//! `non_additive_by`, so a metric that merely *depends* on a semi-additive
//! metric — a derived metric referencing it, or a window metric naming it as
//! its inner aggregate — classified as regular. The dependency's raw aggregate
//! was then inlined and evaluated over every row instead of the snapshot, with
//! `NON ADDITIVE BY` silently discarded.
//!
//! The wrongness is self-evident without reference to Snowflake: with
//! `balance` semi-additive and `double_balance AS balance * 2`, querying the
//! two returned numbers that were not in a 1:2 ratio (verified against DuckDB:
//! 150/70 vs 500/240 where 300/140 was required).
//!
//! A derived metric over one is rejected. Composing a snapshot with an outer
//! expression is a real feature, not a bug fix — see TECH-DEBT #55 for what
//! would finish it — so the fence errors rather than guessing, exactly as the
//! SG-5 co-query guard does for the shapes its CTE cannot decompose. A window
//! metric over one is answered since Phase 7 PR 5: the window runs over the
//! rows its inner metric gives as an ordinary query, the snapshot itself.

use super::*;
use crate::expand::test_helpers::{minimal_def, TestFixtureExt};
use crate::model::{NullsOrder, SortOrder, WindowSpec};

/// `accounts` with a semi-additive `balance` snapshotted at the latest
/// `report_date`, plus whatever metric the caller layers on top.
fn semi_additive_def() -> crate::model::SemanticViewDefinition {
    minimal_def(
        "accounts",
        "customer_id",
        "customer_id",
        "balance",
        "SUM(balance)",
    )
    .with_dimension("report_date", "report_date", None)
    .with_non_additive_by(
        "balance",
        &[("report_date", SortOrder::Desc, NullsOrder::First)],
    )
}

fn req(dims: &[&str], metrics: &[&str]) -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: metrics.iter().map(|m| MetricName::new(*m)).collect(),
    }
}

// EXP-19 — derived metric over a semi-additive base.

#[test]
fn a_derived_metric_over_a_semi_additive_base_errors() {
    let def = semi_additive_def().with_metric("double_balance", "balance * 2", None);

    let result = expand(
        "test_view",
        &def,
        &req(&["customer_id"], &["double_balance"]),
    );

    match result {
        Err(ExpandError::SemiAdditiveThroughDependency {
            ref metric_name,
            ref semi_metric_name,
            ..
        }) => {
            assert_eq!(metric_name, "double_balance");
            assert_eq!(semi_metric_name, "balance");
        }
        other => panic!("expected SemiAdditiveThroughDependency, got: {other:?}"),
    }
}

#[test]
fn the_dependency_error_names_both_metrics_and_the_way_out() {
    let def = semi_additive_def().with_metric("double_balance", "balance * 2", None);

    let msg = expand(
        "test_view",
        &def,
        &req(&["customer_id"], &["double_balance"]),
    )
    .unwrap_err()
    .to_string();

    assert!(
        msg.contains("double_balance") && msg.contains("balance"),
        "message should name both metrics: {msg}"
    );
    assert!(
        msg.contains("report_date"),
        "message should name the NON ADDITIVE BY dimension that resolves it: {msg}"
    );
}

/// A dependency reached through two derivation hops is still a dependency —
/// the walk must be transitive, not one level deep.
#[test]
fn a_transitively_derived_metric_over_a_semi_additive_base_errors() {
    let def = semi_additive_def()
        .with_metric("double_balance", "balance * 2", None)
        .with_metric("quadruple_balance", "double_balance * 2", None);

    let result = expand(
        "test_view",
        &def,
        &req(&["customer_id"], &["quadruple_balance"]),
    );

    assert!(
        matches!(
            result,
            Err(ExpandError::SemiAdditiveThroughDependency { .. })
        ),
        "a two-hop derivation must be caught too, got: {result:?}"
    );
}

// EXP-20 — window metric whose inner metric is semi-additive. Refused until
// Phase 7 PR 5; the window now runs over the rows the inner metric gives as an
// ordinary query, which is the snapshot.

#[test]
fn a_window_metric_over_a_semi_additive_inner_metric_reads_the_snapshot() {
    let def = semi_additive_def()
        .with_metric("rolling_balance", "", None)
        .with_window_spec(
            "rolling_balance",
            WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "balance".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["customer_id".to_string()],
                order_by: vec![],
                frame_clause: None,
            },
        );

    let sql = expand(
        "test_view",
        &def,
        &req(&["customer_id"], &["rolling_balance"]),
    )
    .expect("a window over a semi-additive metric runs over its snapshot");
    let snapshot = expand("test_view", &def, &req(&["customer_id"], &["balance"]))
        .expect("the snapshot alone");
    assert!(snapshot.contains("RANK()"), "{snapshot}");
    assert!(
        sql.contains(&format!("WITH __sv_agg AS (\n{snapshot}\n)")),
        "the window must run over the snapshot's rows: {sql}"
    );
}

// Controls — the guard must fire on the dependency, not on the shapes that
// were already correct. Without these, erroring unconditionally would pass.

/// The semi-additive metric queried directly still takes the snapshot path.
#[test]
fn control_the_semi_additive_metric_itself_still_snapshots() {
    let def = semi_additive_def().with_metric("double_balance", "balance * 2", None);

    let sql = expand("test_view", &def, &req(&["customer_id"], &["balance"]))
        .expect("querying the semi-additive metric directly must still work");

    assert!(
        sql.contains("RANK()"),
        "the snapshot path emits a RANK() CTE: {sql}"
    );
}

/// When every `NON ADDITIVE BY` dimension IS queried the base metric is
/// "effectively regular" (Snowflake semantics) — there is no snapshot to
/// discard, so the derived metric over it must still expand.
#[test]
fn control_a_derived_metric_expands_when_the_na_dimension_is_queried() {
    let def = semi_additive_def().with_metric("double_balance", "balance * 2", None);

    let sql = expand(
        "test_view",
        &def,
        &req(&["customer_id", "report_date"], &["double_balance"]),
    )
    .expect("with report_date queried the base metric is effectively regular");

    assert!(
        !sql.contains("RANK()"),
        "no snapshot is needed once the NA dimension is queried: {sql}"
    );
    assert!(
        sql.contains("SUM(balance)"),
        "the base aggregate is inlined as usual: {sql}"
    );
}

/// A derived metric over a NON-semi-additive base is untouched by the guard.
#[test]
fn control_a_derived_metric_over_a_regular_base_still_expands() {
    let def = minimal_def(
        "accounts",
        "customer_id",
        "customer_id",
        "balance",
        "SUM(balance)",
    )
    .with_metric("double_balance", "balance * 2", None);

    let sql = expand(
        "test_view",
        &def,
        &req(&["customer_id"], &["double_balance"]),
    )
    .expect("no semi-additive metric is involved at all");

    assert!(
        sql.contains("(SUM(balance)) * 2"),
        "the base aggregate is inlined and composed: {sql}"
    );
}
