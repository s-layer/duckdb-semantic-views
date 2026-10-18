//! PK/FK join resolution and emission during expansion.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase26_pkfk_expand_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::model::{Dimension, Join, Metric, SemanticViewDefinition, TableRef};

/// Helper: build a 2-table PK/FK definition (orders -> customers).
fn pkfk_two_table_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "c".to_string(),
                table: "customers".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![
            Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "customer_name".to_string(),
                expr: "c.name".to_string(),
                source_table: Some("c".to_string()),
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
    }
}

/// Helper: build a 3-table PK/FK definition (li -> o -> c).
fn pkfk_three_table_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "li".to_string(),
                table: "line_items".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "c".to_string(),
                table: "customers".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![
            Dimension {
                name: "product".to_string(),
                expr: "li.product".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "customer_name".to_string(),
                expr: "c.name".to_string(),
                source_table: Some("c".to_string()),
                ..Default::default()
            },
        ],
        metrics: vec![Metric {
            name: "total_qty".to_string(),
            expr: "sum(li.qty)".to_string(),
            source_table: Some("li".to_string()),
            ..Default::default()
        }],

        joins: vec![
            Join {
                table: "o".to_string(),
                from_alias: "li".to_string(),
                fk_columns: vec!["order_id".to_string()],
                ..Default::default()
            },
            Join {
                table: "c".to_string(),
                from_alias: "o".to_string(),
                fk_columns: vec!["customer_id".to_string()],
                ..Default::default()
            },
        ],
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

#[test]
fn test_pkfk_on_clause_simple() {
    let def = pkfk_two_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("customer_name")],
        metrics: vec![MetricName::new("total_amount")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("\"o\".\"customer_id\" = \"c\".\"id\""),
        "PK/FK ON clause must use from_alias.fk = to_alias.pk: {sql}"
    );
}

#[test]
fn test_pkfk_on_clause_composite() {
    let def = SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "a".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "b".to_string(),
                table: "details".to_string(),
                pk_columns: vec!["pk1".to_string(), "pk2".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![Dimension {
            name: "detail".to_string(),
            expr: "b.detail".to_string(),
            source_table: Some("b".to_string()),
            ..Default::default()
        }],
        metrics: vec![Metric {
            name: "cnt".to_string(),
            expr: "count(*)".to_string(),
            source_table: Some("a".to_string()),
            ..Default::default()
        }],

        joins: vec![Join {
            table: "b".to_string(),
            from_alias: "a".to_string(),
            fk_columns: vec!["fk1".to_string(), "fk2".to_string()],
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
    };
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("detail")],
        metrics: vec![MetricName::new("cnt")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("\"a\".\"fk1\" = \"b\".\"pk1\""),
        "First FK/PK pair must appear: {sql}"
    );
    assert!(sql.contains("AND"), "Composite ON must use AND: {sql}");
    assert!(
        sql.contains("\"a\".\"fk2\" = \"b\".\"pk2\""),
        "Second FK/PK pair must appear: {sql}"
    );
}

#[test]
fn test_pkfk_left_join_emitted() {
    let def = pkfk_two_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("customer_name")],
        metrics: vec![MetricName::new("total_amount")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("LEFT JOIN"),
        "PK/FK path must emit LEFT JOIN: {sql}"
    );
    let join_lines: Vec<&str> = sql
        .lines()
        .filter(|l| l.trim().starts_with("LEFT JOIN") || l.trim().starts_with("JOIN"))
        .collect();
    for line in &join_lines {
        assert!(
            line.trim().starts_with("LEFT JOIN"),
            "All joins must be LEFT JOIN, got: {line}"
        );
    }
}

#[test]
fn test_pkfk_transitive_join_inclusion() {
    let def = pkfk_three_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("customer_name")],
        metrics: vec![MetricName::new("total_qty")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("LEFT JOIN \"orders\" AS \"o\""),
        "Transitive intermediate join (o) must be included: {sql}"
    );
    assert!(
        sql.contains("LEFT JOIN \"customers\" AS \"c\""),
        "Target join (c) must be included: {sql}"
    );
}

#[test]
fn test_pkfk_pruning() {
    let def = pkfk_three_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("product")],
        metrics: vec![MetricName::new("total_qty")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        !sql.contains("JOIN"),
        "No joins needed when only base-table dims requested: {sql}"
    );
}

#[test]
fn test_pkfk_topological_order() {
    let mut def = pkfk_three_table_def();
    def.joins.reverse();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("customer_name")],
        metrics: vec![MetricName::new("total_qty")],
    };
    let sql = expand("test", &def, &req).unwrap();
    let o_pos = sql
        .find("LEFT JOIN \"orders\"")
        .expect("orders join missing");
    let c_pos = sql
        .find("LEFT JOIN \"customers\"")
        .expect("customers join missing");
    assert!(
        o_pos < c_pos,
        "orders (closer to root) must appear before customers (further from root) in topo order: {sql}"
    );
}
