//! Fact-awareness in metric/dimension resolution.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase46_facts_awareness_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::test_helpers::{orders_view, TestFixtureExt};

#[test]
fn test_facts_metrics_mutual_exclusion() {
    let def = orders_view().with_fact("line_total", "quantity * price", "orders");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("line_total")],
        dimensions: vec![],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let result = expand("test_view", &def, &req);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExpandError::FactsMetricsMutualExclusion { .. }),
        "Expected FactsMetricsMutualExclusion, got: {err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("cannot combine facts and metrics"),
        "Error message should contain 'cannot combine facts and metrics', got: {msg}"
    );
    assert!(
        msg.contains("Use FACTS or METRICS (facts := [...] or metrics := [...]), not both."),
        "Error message should name both query forms, got: {msg}"
    );
}

/// Snowflake's 2026_07 behaviour change: FACTS and DIMENSIONS are not queried
/// together. A dimension goes in FACTS for one row per source row, a fact in
/// DIMENSIONS for one row per distinct value.
#[test]
fn facts_with_dimensions_is_refused() {
    let def = orders_view().with_fact("line_total", "quantity * price", "orders");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("line_total")],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![],
    };
    let err = expand("test_view", &def, &req).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("querying FACTS with DIMENSIONS is not supported"),
        "got: {msg}"
    );
}

/// A dimension listed in FACTS is read row by row, in the list's order.
#[test]
fn a_dimension_in_facts_is_read_row_by_row() {
    let def = orders_view().with_fact("line_total", "quantity * price", "orders");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("line_total"), FactName::new("region")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test_view", &def, &req).unwrap();
    assert!(!sql.contains("GROUP BY"), "{sql}");
    let fact_at = sql.find("AS \"line_total\"").unwrap();
    let dim_at = sql.find("AS \"region\"").unwrap();
    assert!(fact_at < dim_at, "columns follow the FACTS list: {sql}");
    assert_eq!(
        output_column_names("test_view", &def, &req).unwrap(),
        ["line_total", "region"]
    );
}

/// An unknown name in FACTS is suggested among the dimensions too, since
/// FACTS takes them.
#[test]
fn an_unknown_name_in_facts_suggests_a_dimension() {
    let def = orders_view().with_fact("line_total", "quantity * price", "orders");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("regoin"), FactName::new("line_total")],
        dimensions: vec![],
        metrics: vec![],
    };
    let err = expand("test_view", &def, &req).unwrap_err();
    let ExpandError::UnknownFact {
        available,
        suggestion,
        ..
    } = &err
    else {
        panic!("expected UnknownFact, got: {err}");
    };
    assert_eq!(suggestion.as_deref(), Some("region"), "{err}");
    assert!(available.iter().any(|n| n == "region"), "{err}");
    assert!(available.iter().any(|n| n == "line_total"), "{err}");
}

/// A fact is named by its table too, as a dimension is (`orders.line_total`),
/// and only by its own table.
#[test]
fn a_fact_resolves_by_its_qualified_name() {
    let def = orders_view().with_fact("line_total", "quantity * price", "orders");
    let req = |name: &str| QueryRequest {
        where_clause: None,
        facts: vec![FactName::new(name)],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test_view", &def, &req("orders.line_total")).unwrap();
    assert!(sql.contains("AS \"line_total\""), "{sql}");
    let err = expand("test_view", &def, &req("other.line_total")).unwrap_err();
    assert!(err.to_string().contains("unknown fact"), "got: {err}");
}

#[test]
fn test_empty_request_with_facts_is_not_empty() {
    let def = orders_view().with_fact("line_total", "quantity * price", "orders");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("line_total")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    // The expand should NOT return EmptyRequest. It may return another error
    // since fact expansion is not yet implemented — the test verifies the
    // guard condition only.
    assert!(
        !matches!(result, Err(ExpandError::EmptyRequest { .. })),
        "facts-only request should not be treated as empty"
    );
}

#[test]
fn test_unknown_fact_display() {
    let err = ExpandError::UnknownFact {
        view_name: "v".to_string(),
        name: "bad_fact".to_string(),
        available: vec!["f1".to_string(), "f2".to_string()],
        suggestion: Some("f1".to_string()),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("unknown fact"),
        "Should contain 'unknown fact': {msg}"
    );
    assert!(msg.contains("f1, f2"), "Should list available facts: {msg}");
    assert!(msg.contains("Did you mean"), "Should suggest: {msg}");
}

#[test]
fn test_duplicate_fact_display() {
    let err = ExpandError::DuplicateFact {
        view_name: "v".to_string(),
        name: "f1".to_string(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate fact"),
        "Should contain 'duplicate fact': {msg}"
    );
}

#[test]
fn test_fact_path_violation_display() {
    let err = ExpandError::FactPathViolation {
        view_name: "v".to_string(),
        table_a: "orders".to_string(),
        table_b: "products".to_string(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("fact query references"),
        "Should contain 'fact query references': {msg}"
    );
    assert!(msg.contains("orders"), "Should contain table_a: {msg}");
    assert!(msg.contains("products"), "Should contain table_b: {msg}");
}
