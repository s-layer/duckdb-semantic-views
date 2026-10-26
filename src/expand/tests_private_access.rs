//! PRIVATE access-modifier enforcement.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase43_private_access_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::model::{AccessModifier, Dimension, Metric, SemanticViewDefinition};

fn make_def_with_private_metric() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![],
        dimensions: vec![Dimension {
            name: "region".to_string(),
            expr: "region".to_string(),
            ..Default::default()
        }],
        metrics: vec![
            Metric {
                name: "total_revenue".to_string(),
                expr: "SUM(amount)".to_string(),
                ..Default::default()
            },
            Metric {
                name: "secret_cost".to_string(),
                expr: "SUM(cost)".to_string(),
                access: AccessModifier::Private,
                ..Default::default()
            },
        ],
        joins: vec![],
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

fn make_def_with_private_and_derived() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![],
        dimensions: vec![Dimension {
            name: "region".to_string(),
            expr: "region".to_string(),
            ..Default::default()
        }],
        metrics: vec![
            Metric {
                name: "total_revenue".to_string(),
                expr: "SUM(amount)".to_string(),
                ..Default::default()
            },
            Metric {
                name: "secret_cost".to_string(),
                expr: "SUM(cost)".to_string(),
                access: AccessModifier::Private,
                ..Default::default()
            },
            Metric {
                name: "profit".to_string(),
                expr: "total_revenue - secret_cost".to_string(),
                // no source_table: derived metric
                ..Default::default()
            },
        ],
        joins: vec![],
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

#[test]
fn private_metric_rejected() {
    let def = make_def_with_private_metric();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("secret_cost")],
    };
    match expand("test_view", &def, &req) {
        Err(ExpandError::PrivateMetric { name, .. }) => {
            assert_eq!(name, "secret_cost");
        }
        other => panic!("Expected PrivateMetric error, got: {:?}", other),
    }
}

#[test]
fn private_metric_error_message_contains_private() {
    let def = make_def_with_private_metric();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("secret_cost")],
    };
    let err = expand("test_view", &def, &req).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("private"),
        "Error message should contain 'private': {msg}"
    );
    assert!(
        msg.contains("secret_cost"),
        "Error message should contain metric name: {msg}"
    );
}

#[test]
fn public_metric_still_works() {
    let def = make_def_with_private_metric();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("test_view", &def, &req).unwrap();
    assert!(
        sql.contains("total_revenue"),
        "SQL should contain public metric"
    );
}

#[test]
fn derived_metric_referencing_private_base_works() {
    let def = make_def_with_private_and_derived();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("profit")],
    };
    let sql = expand("test_view", &def, &req).unwrap();
    assert!(sql.contains("profit"), "SQL should contain profit metric");
    // The derived metric expression should be inlined:
    // profit = total_revenue - secret_cost = SUM(amount) - SUM(cost)
    assert!(
        sql.contains("SUM(amount)"),
        "Derived metric should inline base expressions"
    );
    assert!(
        sql.contains("SUM(cost)"),
        "Derived metric should inline private base expression"
    );
}
