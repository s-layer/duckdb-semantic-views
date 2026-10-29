//! Phantom-row (NULL-extended LEFT JOIN row) guard behaviour — EXP-25, EXP-26,
//! EXP-28, EXP-29 (code-review 2026-08-08).
//!
//! Every synthesized join is a LEFT JOIN anchored at the base table, so a base
//! row with no matching child row survives as one NULL-extended "phantom" row.
//! SG-8 rewrote `COUNT(*)` for it and EXP-21 added a constant-argument fence;
//! both were per-spelling whitelists that leaked. These tests pin the numbers,
//! not the SQL text: each runs the generated SQL against in-memory DuckDB over
//! the review's fixture and asserts the value a childless parent must produce.
//!
//! Fixture (shared with the review): base `o(id, region, rate)` = (1,'E',10),
//! (2,'N',5); child `li(id, order_id, qty)` = (1,1,2),(2,1,3). Order 2 is
//! childless, so `FROM "o" LEFT JOIN "li"` NULL-extends it.
//!
//! # Since TECH-DEBT #71
//!
//! A child metric is now aggregated from its OWN table (`FROM "li" LEFT JOIN
//! "o"`), where there is no phantom row at all: a childless parent has no row
//! for a child metric queried alone, and a NULL one when a metric of its own
//! grain brings the row in (Snowflake, D1). [`childless_metric_value`] asserts
//! both for every metric these tests declare.
//!
//! The guard is still load-bearing on the shapes that keep the base-anchored
//! path for a child metric (TECH-DEBT #82). The values below are measured on
//! one of them — a request grouped by `region_u`, the same `o.region` declared
//! WITHOUT a source table, which only YAML / legacy JSON can produce and which
//! is never re-anchored because its binding would move with the FROM. So the
//! EXP-25/26 numbers keep their meaning: what the guard makes the phantom row
//! contribute where a phantom row still exists.

use super::*;
use crate::expand::test_helpers::TestFixtureExt;
use crate::model::{Cardinality, Dimension, Join, SemanticViewDefinition, TableRef};

/// Base `o` (parent) + child `li`, both with declared PKs, `li.order_id`
/// referencing `o.id`.
fn child_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "o".to_string(),
                definition: None,
                pk_columns: vec!["id".to_string()],
                unique_constraints: vec![],
                comment: None,
                synonyms: vec![],
                tags: Vec::new(),
            },
            TableRef {
                alias: "li".to_string(),
                table: "li".to_string(),
                definition: None,
                pk_columns: vec!["id".to_string()],
                unique_constraints: vec![],
                comment: None,
                synonyms: vec![],
                tags: Vec::new(),
            },
        ],
        dimensions: vec![
            Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                cortex_search_service: None,
                is_enum: false,
                sample_values: Vec::new(),
            },
            // The base-anchored fallback's dimension: see the module docs.
            Dimension {
                name: "region_u".to_string(),
                expr: "o.region".to_string(),
                source_table: None,
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                cortex_search_service: None,
                is_enum: false,
                sample_values: Vec::new(),
            },
        ],
        // An order-grain metric, co-queried to bring the childless parent's row in.
        metrics: vec![crate::model::Metric {
            name: "n_orders".to_string(),
            expr: "COUNT(o.id)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        joins: vec![Join {
            from_alias: "li".to_string(),
            table: "o".to_string(),
            fk_columns: vec!["order_id".to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some("li_o".to_string()),
            cardinality: Cardinality::ManyToOne,
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

/// The review's fixture, loaded into an in-memory DuckDB.
fn fixture_db() -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE o (id INTEGER, region VARCHAR, rate INTEGER); \
         CREATE TABLE li (id INTEGER, order_id INTEGER, qty INTEGER); \
         INSERT INTO o VALUES (1,'E',10),(2,'N',5); \
         INSERT INTO li VALUES (1,1,2),(2,1,3);",
    )
    .expect("load fixture");
    conn
}

/// TECH-DEBT #71: on the home-anchored path a childless parent has NO row for a
/// child metric queried alone, and a NULL value (never 0, never the phantom's
/// constant) when the order-grain `n_orders` brings its row in.
fn assert_home_anchored_childless(def: &SemanticViewDefinition, metric: &str) {
    let conn = fixture_db();
    let by_region = |metrics: Vec<MetricName>| QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics,
    };
    let alone = expand("v", def, &by_region(vec![MetricName::new(metric)]))
        .unwrap_or_else(|e| panic!("expand failed for {metric}: {e}"));
    let n_rows: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM ({alone}) q WHERE region = 'N'"),
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{alone}"));
    assert_eq!(
        n_rows, 0,
        "a childless parent has no row for {metric} alone:\n{alone}"
    );
    let co = expand(
        "v",
        def,
        &by_region(vec![MetricName::new(metric), MetricName::new("n_orders")]),
    )
    .unwrap_or_else(|e| panic!("expand failed for {metric} + n_orders: {e}"));
    let value: Option<String> = conn
        .query_row(
            &format!(
                "SELECT CAST(q.{} AS VARCHAR) FROM ({co}) q WHERE region = 'N'",
                quote_ident(metric)
            ),
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{co}"));
    assert_eq!(
        value, None,
        "a childless parent's {metric} is NULL when co-queried:\n{co}"
    );
}

/// Expand a single-metric request grouped by the base-anchored fallback's
/// `region_u` and return the value of the metric for the CHILDLESS parent
/// (`region_u = 'N'`), as an `Option<i64>` so a legitimate NULL is
/// distinguishable from 0 — after asserting the home-anchored behaviour of the
/// same metric ([`assert_home_anchored_childless`]).
fn childless_metric_value(def: &SemanticViewDefinition, metric: &str) -> Option<i64> {
    assert_home_anchored_childless(def, metric);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region_u")],
        metrics: vec![MetricName::new(metric)],
    };
    let sql = expand("v", def, &req).unwrap_or_else(|e| panic!("expand failed for {metric}: {e}"));
    let conn = fixture_db();
    conn.query_row(
        &format!("SELECT m FROM ({sql}) q WHERE region_u = 'N'"),
        [],
        |r| r.get::<_, Option<i64>>(0),
    )
    .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"))
}

// ---------------------------------------------------------------------------
// EXP-25 — escapes of the EXP-21 constant-argument whitelist.
// ---------------------------------------------------------------------------

/// `COUNT(DISTINCT 1)` is multiplicity-invariant but EXISTENCE-sensitive: the
/// phantom row is not a duplicate, it is a row that should not be there. A
/// childless parent has no line items, so the count is 0.
#[test]
fn exp25_count_distinct_constant_is_zero_for_a_childless_parent() {
    let def = child_def().with_metric("m", "COUNT(DISTINCT 1)", Some("li"));
    assert_eq!(childless_metric_value(&def, "m"), Some(0));
}

/// A constant *expression* is not a constant *literal*, so `COUNT(1+0)` slipped
/// past `is_constant_literal` exactly as `COUNT(1)` did before EXP-21.
#[test]
fn exp25_count_constant_expression_is_zero_for_a_childless_parent() {
    let def = child_def().with_metric("m", "COUNT(1+0)", Some("li"));
    assert_eq!(childless_metric_value(&def, "m"), Some(0));
}

/// MIN/MAX/AVG are multiplicity-invariant over a constant but not
/// empty-group-invariant: with no rows at all they must be NULL, not the
/// constant the phantom row supplies.
#[test]
fn exp25_min_constant_is_null_for_a_childless_parent() {
    let def = child_def().with_metric("m", "MIN(1)", Some("li"));
    assert_eq!(childless_metric_value(&def, "m"), None);
}

#[test]
fn exp25_max_constant_is_null_for_a_childless_parent() {
    let def = child_def().with_metric("m", "MAX(1)", Some("li"));
    assert_eq!(childless_metric_value(&def, "m"), None);
}

#[test]
fn exp25_avg_constant_is_null_for_a_childless_parent() {
    let def = child_def().with_metric("m", "AVG(1)", Some("li"));
    assert_eq!(childless_metric_value(&def, "m"), None);
}

// ---------------------------------------------------------------------------
// EXP-26 — NULL-insensitive arguments that are not constants at all.
// ---------------------------------------------------------------------------

/// `COALESCE` (like `CASE`, like `x IS NULL`) resurrects the phantom row: the
/// child column is NULL there, so the fallback fires and the row contributes.
#[test]
fn exp26_sum_coalesce_over_child_column_is_null_for_a_childless_parent() {
    let def = child_def().with_metric("m", "SUM(COALESCE(li.qty, 99))", Some("li"));
    assert_eq!(childless_metric_value(&def, "m"), None);
}

/// A child-grain metric whose expression reaches a fact on the PARENT table:
/// `li -> o` does not fan, so no fan-trap check fires, and the parent-side
/// value of the childless order is counted at line-item grain.
#[test]
fn exp26_cross_table_fact_reference_excludes_the_childless_parent() {
    let def =
        child_def()
            .with_fact("orate", "o.rate", "o")
            .with_metric("m", "SUM(orate)", Some("li"));
    // Only the two line items of order 1 exist, each carrying rate 10 -> 20.
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("m")],
    };
    let sql = expand("v", &def, &req).unwrap_or_else(|e| panic!("expand failed: {e}"));
    let conn = fixture_db();
    let total: Option<i64> = conn
        .query_row(&format!("SELECT m FROM ({sql}) q"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(total, Some(20), "SQL:\n{sql}");
}

/// The grouped half of the same shape: the childless order's own group must be
/// NULL (it has no line items), not its parent-side rate.
#[test]
fn exp26_cross_table_fact_reference_is_null_for_a_childless_parent() {
    let def =
        child_def()
            .with_fact("orate", "o.rate", "o")
            .with_metric("m", "SUM(orate)", Some("li"));
    assert_eq!(childless_metric_value(&def, "m"), None);
}

/// The control: an ordinary column aggregate was already correct and must stay
/// correct (the guard is required to be neutral on real rows).
#[test]
fn guard_is_neutral_for_a_plain_column_aggregate() {
    let def = child_def().with_metric("m", "SUM(li.qty)", Some("li"));
    assert_eq!(childless_metric_value(&def, "m"), None);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("m")],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = fixture_db();
    let total: Option<i64> = conn
        .query_row(&format!("SELECT m FROM ({sql}) q"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(total, Some(5), "SQL:\n{sql}");
}

// ---------------------------------------------------------------------------
// EXP-28 — FACTS query on a child-table fact.
// ---------------------------------------------------------------------------

/// `SELECT li.qty FROM o LEFT JOIN li` returns a spurious all-NULL row for the
/// childless order. A fact query is at the grain of its facts; the childless
/// order has no row at line-item grain.
#[test]
fn exp28_facts_query_on_a_child_fact_has_no_phantom_row() {
    let def = child_def().with_fact("liqty", "li.qty", "li");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("liqty")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap_or_else(|e| panic!("expand failed: {e}"));
    let conn = fixture_db();
    let mut stmt = conn
        .prepare(&format!("SELECT liqty FROM ({sql}) q ORDER BY 1"))
        .unwrap_or_else(|e| panic!("prepare failed: {e}\nSQL:\n{sql}"));
    let rows: Vec<Option<i64>> = stmt
        .query_map([], |r| r.get::<_, Option<i64>>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, vec![Some(2), Some(3)], "SQL:\n{sql}");
}

/// The same, with a base-table dimension alongside: joining UP from the child
/// grain is fan-free, so the dimension rides along on the two real rows.
#[test]
fn exp28_facts_query_with_a_base_dimension_has_no_phantom_row() {
    let def = child_def().with_fact("liqty", "li.qty", "li");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("region"), FactName::new("liqty")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap_or_else(|e| panic!("expand failed: {e}"));
    let conn = fixture_db();
    let mut stmt = conn
        .prepare(&format!("SELECT region, liqty FROM ({sql}) q ORDER BY 2"))
        .unwrap_or_else(|e| panic!("prepare failed: {e}\nSQL:\n{sql}"));
    let rows: Vec<(Option<String>, Option<i64>)> = stmt
        .query_map([], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![
            (Some("E".to_string()), Some(2)),
            (Some("E".to_string()), Some(3))
        ],
        "SQL:\n{sql}"
    );
}

/// A fact on the BASE table keeps every base row — the base table is never
/// NULL-extended, so no filter may be added.
#[test]
fn base_table_facts_query_keeps_every_base_row() {
    let def = child_def().with_fact("orate", "o.rate", "o");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("orate")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = fixture_db();
    let n: i64 = conn
        .query_row(&format!("SELECT count(*) FROM ({sql}) q"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(n, 2, "SQL:\n{sql}");
}

/// A FACTS list may hold dimensions only. One on the child table is at the
/// child's grain like a child fact, so the childless order has no row.
#[test]
fn facts_list_of_a_child_dimension_has_no_phantom_row() {
    let def = child_def().with_dimension("qty", "li.qty", Some("li"));
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("qty")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap_or_else(|e| panic!("expand failed: {e}"));
    let conn = fixture_db();
    let mut stmt = conn
        .prepare(&format!("SELECT qty FROM ({sql}) q ORDER BY 1"))
        .unwrap_or_else(|e| panic!("prepare failed: {e}\nSQL:\n{sql}"));
    let rows: Vec<Option<i64>> = stmt
        .query_map([], |r| r.get::<_, Option<i64>>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, vec![Some(2), Some(3)], "SQL:\n{sql}");
}

/// A FACTS list of a base and a child dimension keeps the childless base row,
/// as the same dimensions do in DIMENSIONS: the members span two tables.
#[test]
fn facts_list_mixing_base_and_child_dimensions_keeps_the_base_row() {
    let def = child_def().with_dimension("qty", "li.qty", Some("li"));
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("region"), FactName::new("qty")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = fixture_db();
    let n: i64 = conn
        .query_row(&format!("SELECT count(*) FROM ({sql}) q"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(n, 3, "SQL:\n{sql}");
}

// ---------------------------------------------------------------------------
// EXP-29 — dimensions-only DISTINCT on a child dimension.
// ---------------------------------------------------------------------------

/// `SELECT DISTINCT li.qty FROM o LEFT JOIN li` manufactures a NULL that is
/// indistinguishable from a genuine data NULL.
#[test]
fn exp29_dims_only_distinct_on_a_child_dimension_has_no_phantom_null() {
    let def = child_def().with_dimension("qty", "li.qty", Some("li"));
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("qty")],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap_or_else(|e| panic!("expand failed: {e}"));
    let conn = fixture_db();
    let mut stmt = conn
        .prepare(&format!("SELECT qty FROM ({sql}) q ORDER BY 1"))
        .unwrap_or_else(|e| panic!("prepare failed: {e}\nSQL:\n{sql}"));
    let rows: Vec<Option<i64>> = stmt
        .query_map([], |r| r.get::<_, Option<i64>>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, vec![Some(2), Some(3)], "SQL:\n{sql}");
}

// ---------------------------------------------------------------------------
// Direction control: a member ABOVE the base is an attribute, not a grain.
//
// The same LEFT JOIN NULL-extends a base row whose foreign key is NULL or
// dangling — but that row is a genuine row of the view whose parent attribute is
// simply unknown, so filtering it would DELETE data.
// `multi_hop_join_proptest` catches an over-eager filter here immediately; these
// pin it at the unit level too.
// ---------------------------------------------------------------------------

/// Base `li` (the child) with parent `o` — the direction reversed.
fn parent_def() -> SemanticViewDefinition {
    let mut def = child_def();
    def.tables.reverse(); // `li` becomes the base table
    def.dimensions.clear();
    def.with_dimension("region", "o.region", Some("o"))
        .with_fact("orate", "o.rate", "o")
}

/// `li` row 2 is an ORPHAN: its `order_id` is NULL, so the LEFT JOIN up to `o`
/// NULL-extends it.
fn orphan_db() -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE o (id INTEGER, region VARCHAR, rate INTEGER); \
         CREATE TABLE li (id INTEGER, order_id INTEGER, qty INTEGER); \
         INSERT INTO o VALUES (1,'E',10); \
         INSERT INTO li VALUES (1,1,2),(2,NULL,7);",
    )
    .expect("load fixture");
    conn
}

#[test]
fn dims_only_distinct_on_a_parent_dimension_keeps_the_unmatched_base_row() {
    let def = parent_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = orphan_db();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT region FROM ({sql}) q ORDER BY 1 NULLS LAST"
        ))
        .unwrap_or_else(|e| panic!("prepare failed: {e}\nSQL:\n{sql}"));
    let rows: Vec<Option<String>> = stmt
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, vec![Some("E".to_string()), None], "SQL:\n{sql}");
}

#[test]
fn facts_query_on_a_parent_fact_keeps_the_unmatched_base_row() {
    let def = parent_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("orate")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = orphan_db();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT orate FROM ({sql}) q ORDER BY 1 NULLS LAST"
        ))
        .unwrap_or_else(|e| panic!("prepare failed: {e}\nSQL:\n{sql}"));
    let rows: Vec<Option<i64>> = stmt
        .query_map([], |r| r.get::<_, Option<i64>>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, vec![Some(10), None], "SQL:\n{sql}");
}

/// A dimensions-only DISTINCT that mixes a base dimension with a child one is
/// NOT filtered: the base row is a legitimate member of the result even without
/// a child, and the queried members no longer live on one non-base table.
#[test]
fn dims_only_distinct_mixing_base_and_child_keeps_the_base_row() {
    let def = child_def().with_dimension("qty", "li.qty", Some("li"));
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region"), DimensionName::new("qty")],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = fixture_db();
    let n: i64 = conn
        .query_row(&format!("SELECT count(*) FROM ({sql}) q"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(n, 3, "SQL:\n{sql}");
}

/// Dimensions-only DISTINCT on a BASE dimension keeps every base row.
#[test]
fn dims_only_distinct_on_a_base_dimension_keeps_every_base_row() {
    let def = child_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = fixture_db();
    let n: i64 = conn
        .query_row(&format!("SELECT count(*) FROM ({sql}) q"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(n, 2, "SQL:\n{sql}");
}

// ---------------------------------------------------------------------------
// Guard shape: the rewrite must stay valid SQL for the awkward argument forms.
// ---------------------------------------------------------------------------

/// `COUNT(*)` is SG-8's job; the argument guard must not wrap it a second time.
#[test]
fn count_star_is_not_double_guarded() {
    let def = child_def().with_metric("m", "COUNT(*)", Some("li"));
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("m")],
    };
    let sql = expand("v", &def, &req).unwrap();
    assert!(
        sql.contains("COUNT(\"li\".\"id\") AS \"m\""),
        "COUNT(*) must stay the plain SG-8 rewrite: {sql}"
    );
}

/// A second argument is not part of the guarded value: `STRING_AGG`'s separator
/// must survive verbatim, and the result must still be executable SQL. Run on
/// the base-anchored fallback (`region_u`), where the guard has a phantom row
/// to exclude.
#[test]
fn string_agg_separator_is_left_alone() {
    let def = child_def().with_metric("m", "STRING_AGG(li.qty, ',')", Some("li"));
    assert_home_anchored_childless(&def, "m");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region_u")],
        metrics: vec![MetricName::new("m")],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = fixture_db();
    let got: Option<String> = conn
        .query_row(
            &format!("SELECT m FROM ({sql}) q WHERE region_u = 'E'"),
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(got.as_deref(), Some("2,3"), "SQL:\n{sql}");
    let childless: Option<String> = conn
        .query_row(
            &format!("SELECT m FROM ({sql}) q WHERE region_u = 'N'"),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(childless, None, "SQL:\n{sql}");
}

/// An `ORDER BY` inside the aggregate belongs to the call, not to the argument:
/// wrapping it inside the `CASE` would be a syntax error.
#[test]
fn ordered_aggregate_stays_valid_sql() {
    let def = child_def().with_metric(
        "m",
        "STRING_AGG(li.qty, ',' ORDER BY li.qty DESC)",
        Some("li"),
    );
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("m")],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = fixture_db();
    let got: Option<String> = conn
        .query_row(
            &format!("SELECT m FROM ({sql}) q WHERE region = 'E'"),
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(got.as_deref(), Some("3,2"), "SQL:\n{sql}");
}

/// A `FILTER` clause sits outside the parentheses; the guard rewrites only the
/// argument, and the two compose (the phantom row is excluded either way). Run
/// on the base-anchored fallback (`region_u`), where the phantom exists.
#[test]
fn filtered_aggregate_stays_valid_sql() {
    let def = child_def().with_metric("m", "COUNT(1) FILTER (WHERE li.qty > 2)", Some("li"));
    assert_home_anchored_childless(&def, "m");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region_u")],
        metrics: vec![MetricName::new("m")],
    };
    let sql = expand("v", &def, &req).unwrap();
    let conn = fixture_db();
    let e_val: Option<i64> = conn
        .query_row(
            &format!("SELECT m FROM ({sql}) q WHERE region_u = 'E'"),
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    assert_eq!(e_val, Some(1), "SQL:\n{sql}");
    let n_val: Option<i64> = conn
        .query_row(
            &format!("SELECT m FROM ({sql}) q WHERE region_u = 'N'"),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n_val, Some(0), "SQL:\n{sql}");
}

// ---------------------------------------------------------------------------
// Guard text: the scanner's edges, asserted directly on the rewrite.
// ---------------------------------------------------------------------------

/// The whole-word match is what makes a long aggregate name reachable: the
/// prefix scan this replaces matched `sum`, found `_` instead of `(`, and gave
/// up — leaving `sum_no_overflow(1)` unguarded with no diagnostic.
#[test]
fn guard_matches_a_whole_aggregate_word_not_a_prefix() {
    assert_eq!(
        super::facts::guard_aggregate_args("sum_no_overflow(1)", "pk").as_deref(),
        Some("sum_no_overflow(CASE WHEN pk IS NOT NULL THEN 1 END)")
    );
}

/// …and it must not fire on a word that merely ENDS with an aggregate name.
#[test]
fn guard_ignores_a_word_ending_in_an_aggregate_name() {
    assert!(super::facts::guard_aggregate_args("miscount(1)", "pk").is_none());
    assert!(super::facts::guard_aggregate_args("li.summary", "pk").is_none());
}

/// Quoted regions are inert: a string literal or a double-quoted identifier
/// that happens to spell an aggregate call is data, not code.
#[test]
fn guard_leaves_quoted_regions_alone() {
    assert!(super::facts::guard_aggregate_args("'count(1)'", "pk").is_none());
    assert!(super::facts::guard_aggregate_args("\"sum(1) col\"", "pk").is_none());
    assert_eq!(
        super::facts::guard_aggregate_args("COUNT(1) + \"count(1)\"", "pk").as_deref(),
        Some("COUNT(CASE WHEN pk IS NOT NULL THEN 1 END) + \"count(1)\"")
    );
}

/// The `DISTINCT` quantifier stays outside the `CASE` — `COUNT(CASE … DISTINCT
/// …)` would not parse.
#[test]
fn guard_places_the_case_after_a_distinct_quantifier() {
    assert_eq!(
        super::facts::guard_aggregate_args("COUNT(DISTINCT li.qty)", "pk").as_deref(),
        Some("COUNT(DISTINCT CASE WHEN pk IS NOT NULL THEN li.qty END)")
    );
}

/// A trailing in-call `ORDER BY` and any argument after the first are part of
/// the CALL, and are copied through verbatim.
#[test]
fn guard_rewrites_only_the_first_argument() {
    assert_eq!(
        super::facts::guard_aggregate_args("STRING_AGG(li.qty, ',' ORDER BY li.id)", "pk")
            .as_deref(),
        Some("STRING_AGG(CASE WHEN pk IS NOT NULL THEN li.qty END, ',' ORDER BY li.id)")
    );
    // A column whose name merely starts with `order` is not the keyword.
    assert_eq!(
        super::facts::guard_aggregate_args("SUM(li.order_id)", "pk").as_deref(),
        Some("SUM(CASE WHEN pk IS NOT NULL THEN li.order_id END)")
    );
}

/// `COUNT(*)` is skipped so SG-8's star rewrite owns it alone; a nested
/// aggregate-looking name inside the argument is not rescanned.
#[test]
fn guard_skips_the_star_argument() {
    assert!(super::facts::guard_aggregate_args("COUNT(*)", "pk").is_none());
    assert!(super::facts::guard_aggregate_args("COUNT( * )", "pk").is_none());
}

/// A metric on a non-base table with no declared PRIMARY KEY cannot be guarded
/// at all. On the base-anchored fallback that is still a loud error for the
/// shapes that are definitely wrong (`COUNT(*)`, constant arguments) —
/// including the MIN/MAX/AVG spellings EXP-25 added to the family. Anchored at
/// its own table (TECH-DEBT #71) the metric needs no key: there is no phantom.
#[test]
fn no_pk_child_min_constant_errors_loudly() {
    let mut def = child_def().with_metric("m", "MIN(1)", Some("li"));
    def.tables[1].pk_columns.clear();
    let home = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("m")],
    };
    expand("v", &def, &home).expect("home-anchored, MIN(1) needs no PRIMARY KEY");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region_u")],
        metrics: vec![MetricName::new("m")],
    };
    let err = expand("v", &def, &req).expect_err("no-PK child constant aggregate must error");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("primary key"),
        "expected a PRIMARY KEY diagnostic, got: {msg}"
    );
}
