//! General expansion behaviour: dimensions, metrics, DISTINCT, GROUP BY.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::expand_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::test_helpers::{minimal_def, orders_view, TestFixtureExt};
use crate::model::SemanticViewDefinition;

#[test]
fn test_basic_single_dimension_single_metric() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    let expected = "\
SELECT
    region AS \"region\",
    sum(amount) AS \"total_revenue\"
FROM \"orders\" AS \"orders\"
GROUP BY
    1";
    assert_eq!(sql, expected);
}

#[test]
fn test_multiple_dimensions_multiple_metrics() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region"), DimensionName::new("status")],
        metrics: vec![
            MetricName::new("total_revenue"),
            MetricName::new("order_count"),
        ],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(sql.starts_with("SELECT\n"), "Should start with SELECT");
    assert!(
        sql.contains("region AS \"region\""),
        "Should include region dim"
    );
    assert!(
        sql.contains("status AS \"status\""),
        "Should include status dim"
    );
    assert!(
        sql.contains("sum(amount) AS \"total_revenue\""),
        "Should include revenue metric"
    );
    assert!(
        sql.contains("count(*) AS \"order_count\""),
        "Should include count metric"
    );
    assert!(
        sql.contains("FROM \"orders\""),
        "Should reference orders table"
    );
    assert!(sql.contains("GROUP BY"), "Should have GROUP BY");
    assert!(sql.contains("1,"), "Should group by ordinal 1");
    assert!(sql.contains("2"), "Should group by ordinal 2");
}

#[test]
fn test_global_aggregate_no_dimensions() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(sql.starts_with("SELECT\n"), "Should start with SELECT");
    assert!(
        sql.contains("sum(amount) AS \"total_revenue\""),
        "Should include revenue metric"
    );
    assert!(
        sql.contains("FROM \"orders\""),
        "Should reference orders table"
    );
    assert!(!sql.contains("GROUP BY"), "No GROUP BY when no dimensions");
}

#[test]
fn test_identifier_quoting() {
    let def = minimal_def("select", "col", "col", "cnt", "count(*)");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("col")],
        metrics: vec![MetricName::new("cnt")],
    };
    let sql = expand("test", &def, &req).unwrap();
    // Base table "select" must be quoted
    assert!(sql.contains("FROM \"select\""));
}

#[test]
fn test_dimension_expression_not_quoted() {
    let def = minimal_def(
        "orders",
        "month",
        "date_trunc('month', created_at)",
        "total_revenue",
        "sum(amount)",
    );
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("month")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    // Expression appears verbatim in SELECT; GROUP BY uses ordinal position
    assert!(sql.contains("date_trunc('month', created_at) AS \"month\""));
    assert!(sql.contains("GROUP BY\n    1"));
}

#[test]
fn test_empty_request_error() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("orders", &def, &req);
    assert!(result.is_err());
    match result.unwrap_err() {
        ExpandError::EmptyRequest { view_name } => {
            assert_eq!(view_name, "orders");
        }
        other => panic!("Expected EmptyRequest, got: {other}"),
    }
}

#[test]
fn test_dimensions_only_generates_distinct() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region"), DimensionName::new("status")],
        metrics: vec![],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        sql.starts_with("SELECT DISTINCT\n"),
        "Should start with SELECT DISTINCT"
    );
    assert!(
        sql.contains("region AS \"region\""),
        "Should include region dim"
    );
    assert!(
        sql.contains("status AS \"status\""),
        "Should include status dim"
    );
    assert!(
        sql.contains("FROM \"orders\""),
        "Should reference orders table"
    );
    assert!(
        !sql.contains("GROUP BY"),
        "No GROUP BY for dims-only (DISTINCT instead)"
    );
}

#[test]
fn test_metrics_only_still_works() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![
            MetricName::new("total_revenue"),
            MetricName::new("order_count"),
        ],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(sql.starts_with("SELECT\n"), "Should start with SELECT");
    assert!(
        !sql.starts_with("SELECT DISTINCT"),
        "Should NOT be DISTINCT for metrics-only"
    );
    assert!(
        sql.contains("sum(amount) AS \"total_revenue\""),
        "Should include revenue metric"
    );
    assert!(
        sql.contains("count(*) AS \"order_count\""),
        "Should include count metric"
    );
    assert!(
        sql.contains("FROM \"orders\""),
        "Should reference orders table"
    );
    assert!(!sql.contains("GROUP BY"), "No GROUP BY when no dimensions");
}

#[test]
fn test_case_insensitive_dimension_lookup() {
    let def = minimal_def("orders", "Region", "region", "total_revenue", "sum(amount)");
    // Request uses lowercase "region" but definition has "Region"
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    // Should succeed and use the definition's expression
    assert!(sql.contains("region AS \"Region\""));
    assert!(sql.contains("GROUP BY\n    1"));
}

#[test]
fn test_unknown_dimension_error() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("reigon")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let result = expand("orders", &def, &req);
    assert!(result.is_err());
    match result.unwrap_err() {
        ExpandError::UnknownDimension {
            view_name,
            name,
            available,
            suggestion,
        } => {
            assert_eq!(view_name, "orders");
            assert_eq!(name, "reigon");
            assert!(available.contains(&"region".to_string()));
            assert_eq!(suggestion, Some("region".to_string()));
        }
        other => panic!("Expected UnknownDimension, got: {other}"),
    }
}

#[test]
fn test_unknown_metric_error() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("totl_revenue")],
    };
    let result = expand("orders", &def, &req);
    assert!(result.is_err());
    match result.unwrap_err() {
        ExpandError::UnknownMetric {
            view_name,
            name,
            available,
            suggestion,
        } => {
            assert_eq!(view_name, "orders");
            assert_eq!(name, "totl_revenue");
            assert!(available.contains(&"total_revenue".to_string()));
            assert_eq!(suggestion, Some("total_revenue".to_string()));
        }
        other => panic!("Expected UnknownMetric, got: {other}"),
    }
}

#[test]
fn test_unknown_dimension_no_suggestion() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("xyzzy")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let result = expand("orders", &def, &req);
    assert!(result.is_err());
    match result.unwrap_err() {
        ExpandError::UnknownDimension { suggestion, .. } => {
            assert_eq!(suggestion, None);
        }
        other => panic!("Expected UnknownDimension, got: {other}"),
    }
}

#[test]
fn test_duplicate_dimension_error() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region"), DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let result = expand("orders", &def, &req);
    assert!(result.is_err());
    match result.unwrap_err() {
        ExpandError::DuplicateDimension { view_name, name } => {
            assert_eq!(view_name, "orders");
            assert_eq!(name, "region");
        }
        other => panic!("Expected DuplicateDimension, got: {other}"),
    }
}

#[test]
fn test_duplicate_metric_error() {
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![
            MetricName::new("total_revenue"),
            MetricName::new("total_revenue"),
        ],
    };
    let result = expand("orders", &def, &req);
    assert!(result.is_err());
    match result.unwrap_err() {
        ExpandError::DuplicateMetric { view_name, name } => {
            assert_eq!(view_name, "orders");
            assert_eq!(name, "total_revenue");
        }
        other => panic!("Expected DuplicateMetric, got: {other}"),
    }
}

#[test]
fn test_case_insensitive_metric_lookup() {
    let def = SemanticViewDefinition::default()
        .with_table("orders", "orders", &[])
        .with_metric("Total_Revenue", "sum(amount)", None);
    // Request uses lowercase "total_revenue" but definition has "Total_Revenue"
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    // Should succeed and use the definition's name casing in the alias
    assert!(sql.contains("sum(amount) AS \"Total_Revenue\""));
}

#[test]
fn test_error_display_messages() {
    // EmptyRequest
    let err = ExpandError::EmptyRequest {
        view_name: "orders".to_string(),
    };
    let msg = format!("{err}");
    assert!(msg.contains("orders"));
    assert!(msg.contains("specify at least one of DIMENSIONS, METRICS or FACTS"));

    // UnknownDimension with suggestion
    let err = ExpandError::UnknownDimension {
        view_name: "orders".to_string(),
        name: "reigon".to_string(),
        available: vec!["region".to_string(), "status".to_string()],
        suggestion: Some("region".to_string()),
    };
    let msg = format!("{err}");
    assert!(msg.contains("orders"));
    assert!(msg.contains("reigon"));
    assert!(msg.contains("region, status"));
    assert!(msg.contains("Did you mean 'region'?"));

    // UnknownDimension without suggestion
    let err = ExpandError::UnknownDimension {
        view_name: "orders".to_string(),
        name: "xyzzy".to_string(),
        available: vec!["region".to_string()],
        suggestion: None,
    };
    let msg = format!("{err}");
    assert!(msg.contains("xyzzy"));
    assert!(!msg.contains("Did you mean"));

    // UnknownMetric with suggestion
    let err = ExpandError::UnknownMetric {
        view_name: "orders".to_string(),
        name: "totl_revenue".to_string(),
        available: vec!["total_revenue".to_string()],
        suggestion: Some("total_revenue".to_string()),
    };
    let msg = format!("{err}");
    assert!(msg.contains("orders"));
    assert!(msg.contains("totl_revenue"));
    assert!(msg.contains("Did you mean 'total_revenue'?"));

    // DuplicateDimension
    let err = ExpandError::DuplicateDimension {
        view_name: "orders".to_string(),
        name: "region".to_string(),
    };
    let msg = format!("{err}");
    assert!(msg.contains("orders"));
    assert!(msg.contains("duplicate dimension 'region'"));

    // DuplicateMetric
    let err = ExpandError::DuplicateMetric {
        view_name: "orders".to_string(),
        name: "total_revenue".to_string(),
    };
    let msg = format!("{err}");
    assert!(msg.contains("orders"));
    assert!(msg.contains("duplicate metric 'total_revenue'"));
}

#[test]
fn test_join_excluded_when_not_needed() {
    let def = orders_view()
        .with_table("customers", "customers", &["id"])
        .with_dimension("customer_name", "customers.name", Some("customers"))
        .with_pkfk_join("cust", "orders", "customers", &["customer_id"], &["id"]);
    // Request only "region" which comes from base table
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        !sql.contains("JOIN"),
        "JOIN should not appear when only base-table dims/metrics requested"
    );
}

#[test]
fn test_no_joins_declared_no_error() {
    let def = minimal_def("orders", "region", "region", "total_revenue", "sum(amount)");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        !sql.contains("JOIN"),
        "no JOIN clauses when no joins declared"
    );
}

#[test]
fn test_dot_qualified_base_table() {
    let def = minimal_def(
        "jaffle.raw_orders",
        "status",
        "status",
        "order_count",
        "count(*)",
    );
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("status")],
        metrics: vec![MetricName::new("order_count")],
    };
    let sql = expand("jaffle_orders", &def, &req).unwrap();
    // Must produce "jaffle"."raw_orders" not "jaffle.raw_orders"
    assert!(
        sql.contains("FROM \"jaffle\".\"raw_orders\""),
        "dot-qualified base_table must be split and quoted: {sql}"
    );
}

/// When database_name and schema_name are set on the definition,
/// the generated SQL must fully-qualify table references in FROM.
/// Without this, ADBC connections fail because they don't maintain
/// the same catalog/schema search path as normal connections.
#[test]
fn test_base_table_qualified_with_catalog_schema() {
    let mut def = minimal_def("orders", "region", "region", "total_revenue", "sum(amount)");
    def.database_name = Some("memory".to_string());
    def.schema_name = Some("main".to_string());
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders_view", &def, &req).unwrap();
    assert!(
        sql.contains("FROM \"memory\".\"main\".\"orders\""),
        "base table must be catalog.schema qualified when database_name/schema_name are set: {sql}"
    );
}

/// Same as above but for JOIN targets — joined tables must also be
/// fully qualified when database_name/schema_name are present.
#[test]
fn test_join_table_qualified_with_catalog_schema() {
    let mut def = orders_view()
        .with_table("c", "customers", &["id"])
        .with_pkfk_join("orders_customers", "orders", "c", &["customer_id"], &["id"])
        .with_dimension("customer_name", "c.name", Some("c"));
    def.database_name = Some("memory".to_string());
    def.schema_name = Some("main".to_string());
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![
            DimensionName::new("region"),
            DimensionName::new("customer_name"),
        ],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders_view", &def, &req).unwrap();
    assert!(
        sql.contains("FROM \"memory\".\"main\".\"orders\""),
        "base table in JOIN query must be qualified: {sql}"
    );
    assert!(
        sql.contains("LEFT JOIN \"memory\".\"main\".\"customers\""),
        "joined table must be catalog.schema qualified: {sql}"
    );
}

/// When only schema_name is set (no database_name), qualify with
/// just the schema prefix.
#[test]
fn test_base_table_qualified_schema_only() {
    let mut def = minimal_def("orders", "region", "region", "total_revenue", "sum(amount)");
    def.schema_name = Some("analytics".to_string());
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders_view", &def, &req).unwrap();
    assert!(
        sql.contains("FROM \"analytics\".\"orders\""),
        "base table must be schema-qualified when only schema_name is set: {sql}"
    );
}

/// When neither database_name nor schema_name are set, table refs
/// remain unqualified (backward compat).
#[test]
fn test_base_table_unqualified_when_no_catalog_schema() {
    let def = minimal_def("orders", "region", "region", "total_revenue", "sum(amount)");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders_view", &def, &req).unwrap();
    // Should NOT have any dot-qualification beyond what's in the table name itself
    assert!(
        sql.contains("FROM \"orders\""),
        "table must remain unqualified when no catalog/schema set: {sql}"
    );
    assert!(
        !sql.contains("FROM \"memory\""),
        "must not inject catalog when not set: {sql}"
    );
}

/// Tables that are already dot-qualified in the definition should
/// NOT get double-qualified.
#[test]
fn test_already_qualified_table_not_double_qualified() {
    let mut def = minimal_def(
        "mydb.myschema.orders",
        "region",
        "region",
        "total_revenue",
        "sum(amount)",
    );
    // Even with database_name/schema_name set, a table that's already
    // dot-qualified should be used as-is.
    def.database_name = Some("memory".to_string());
    def.schema_name = Some("main".to_string());
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_revenue")],
    };
    let sql = expand("orders_view", &def, &req).unwrap();
    assert!(
        sql.contains("FROM \"mydb\".\"myschema\".\"orders\""),
        "already-qualified table must not be re-qualified: {sql}"
    );
    assert!(
        !sql.contains("\"memory\".\"main\".\"mydb\""),
        "must not double-qualify: {sql}"
    );
}
