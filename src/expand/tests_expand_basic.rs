//! Basic single/multi dimension + metric expansion.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase11_1_expand_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::model::TableRef;

fn def_with_join_columns() -> crate::model::SemanticViewDefinition {
    crate::model::SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                ..Default::default()
            },
            TableRef {
                alias: "c".to_string(),
                table: "customers".to_string(),
                ..Default::default()
            },
        ],
        dimensions: vec![
            crate::model::Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),

                ..Default::default()
            },
            crate::model::Dimension {
                name: "tier".to_string(),
                expr: "c.tier".to_string(),
                source_table: Some("c".to_string()),

                ..Default::default()
            },
        ],
        metrics: vec![crate::model::Metric {
            name: "revenue".to_string(),
            expr: "sum(o.amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],

        joins: vec![crate::model::Join {
            // Modern (Phase 24) FK encoding: source alias `o`, target
            // alias `c`, with fk/ref columns so the fan-trap safety
            // check can build the relationship graph (SG-7 / AR-4).
            table: "c".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            ref_columns: vec!["id".to_string()],
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
    }
}

#[test]
fn table_qualified_dimension_lookup_with_matching_source_table() {
    let def = def_with_join_columns();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("o.region")],
        metrics: vec![],
    };
    let sql = expand("sales_view", &def, &req).unwrap();
    assert!(
        sql.contains("o.region"),
        "Must include the dimension expr: {sql}"
    );
    assert!(
        sql.contains("AS \"region\""),
        "Must alias as bare name: {sql}"
    );
}

#[test]
fn bare_dimension_name_still_resolves() {
    let def = def_with_join_columns();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![],
    };
    let result = expand("sales_view", &def, &req);
    assert!(
        result.is_ok(),
        "Bare name lookup must succeed: {:?}",
        result.err()
    );
}

#[test]
fn table_qualified_unknown_dimension_returns_error() {
    let def = def_with_join_columns();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("o.nosuch")],
        metrics: vec![],
    };
    let result = expand("sales_view", &def, &req);
    match result {
        Err(ExpandError::UnknownDimension { name, .. }) => {
            let _ = name;
        }
        other => panic!("Expected UnknownDimension error, got: {:?}", other),
    }
}

#[test]
fn table_qualified_metric_lookup_with_matching_source_table() {
    let def = def_with_join_columns();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("o.revenue")],
    };
    let sql = expand("sales_view", &def, &req).unwrap();
    assert!(
        sql.contains("sum(o.amount)"),
        "Must include metric expr: {sql}"
    );
}

/// EXP-7 (code-review 2026-07-18): a dimension or metric declared with a
/// *quoted* name (`"order date"`) is stored WITH its quotes, so passing the
/// stored name straight through `quote_ident` at the emission sites re-quoted
/// it into a triple-quoted alias (`AS """order date"""`) — the output column
/// was then literally named with quote characters. The stored name must be
/// stripped to its logical value before quoting, yielding exactly one pair of
/// quotes (`AS "order date"`, output column `order date`).
#[test]
fn quoted_stored_names_emit_single_quoted_output_aliases() {
    use crate::model::{Dimension, Metric, SemanticViewDefinition, TableRef};
    let def = SemanticViewDefinition {
        tables: vec![TableRef {
            alias: "o".to_string(),
            table: "orders".to_string(),
            ..Default::default()
        }],
        dimensions: vec![Dimension {
            // Stored with its quotes, exactly as the body parser captures it.
            name: "\"order date\"".to_string(),
            expr: "o.order_date".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        metrics: vec![Metric {
            name: "\"total sales\"".to_string(),
            expr: "SUM(o.amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("order date")],
        metrics: vec![MetricName::new("total sales")],
    };
    let sql = expand("sales_view", &def, &req).expect("quoted-name query should expand");
    // One canonical pair of quotes per alias — the output columns are named
    // `order date` / `total sales`, not `"order date"` / `"total sales"`.
    assert!(
        sql.contains("AS \"order date\""),
        "dimension alias must be single-quoted: {sql}"
    );
    assert!(
        sql.contains("AS \"total sales\""),
        "metric alias must be single-quoted: {sql}"
    );
    // No triple-quote leak from re-quoting the already-quoted stored name.
    assert!(
        !sql.contains("\"\"\"order date\"\"\""),
        "dimension alias must not be triple-quoted: {sql}"
    );
    assert!(
        !sql.contains("\"\"\"total sales\"\"\""),
        "metric alias must not be triple-quoted: {sql}"
    );
}
