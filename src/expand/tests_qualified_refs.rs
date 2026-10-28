//! Qualified (table.col) reference handling in expressions.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase27_qualified_refs_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::model::{Dimension, Join, Metric, SemanticViewDefinition, TableRef};

fn qualified_ref_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "p27_orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "c".to_string(),
                table: "p27_customers".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![Dimension {
            name: "customer_name".to_string(),
            expr: "c.name".to_string(),
            source_table: Some("c".to_string()),
            ..Default::default()
        }],
        metrics: vec![Metric {
            name: "total_amount".to_string(),
            expr: "sum(o.amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],

        joins: vec![Join {
            table: "c".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            ..Default::default()
        }],
        facts: vec![],
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

#[test]
fn test_expand_qualified_column_refs_verbatim() {
    let def = qualified_ref_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("customer_name")],
        metrics: vec![MetricName::new("total_amount")],
    };
    let sql = expand("p27_test", &def, &req).unwrap();

    assert!(
        sql.contains("c.name AS"),
        "Qualified dim expr 'c.name' must appear verbatim in SQL: {sql}"
    );

    assert!(
        sql.contains("sum(o.amount) AS"),
        "Qualified metric expr 'sum(o.amount)' must appear verbatim in SQL: {sql}"
    );
}

#[test]
fn test_expand_multiple_qualified_refs_different_tables() {
    let def = SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "p27_orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "c".to_string(),
                table: "p27_customers".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![
            Dimension {
                name: "customer_name".to_string(),
                expr: "c.name".to_string(),
                source_table: Some("c".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "order_region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            },
        ],
        metrics: vec![Metric {
            name: "total_amount".to_string(),
            expr: "sum(o.amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],

        joins: vec![Join {
            table: "c".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            ..Default::default()
        }],
        facts: vec![],
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
    };
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![
            DimensionName::new("customer_name"),
            DimensionName::new("order_region"),
        ],
        metrics: vec![MetricName::new("total_amount")],
    };
    let sql = expand("p27_test", &def, &req).unwrap();

    assert!(
        sql.contains("c.name AS"),
        "Qualified dim expr 'c.name' must appear verbatim: {sql}"
    );
    assert!(
        sql.contains("o.region AS"),
        "Qualified dim expr 'o.region' must appear verbatim: {sql}"
    );
    assert!(
        sql.contains("sum(o.amount) AS"),
        "Qualified metric expr 'sum(o.amount)' must appear verbatim: {sql}"
    );
}
