//! Bare (unqualified) column references inside member expressions, on the
//! home-anchored path and the base-anchored path (TECH-DEBT #83).
//!
//! Snowflake scopes a member expression to its own logical table: a bare
//! column means "this table's column", and a column of another table is a
//! CREATE error. This extension splices the expression verbatim into whatever
//! FROM the query needs, so a bare column binds against every table joined
//! there. Two shapes follow, both pre-existing on `main`:
//!
//! - **Same-name collision** — `o_id` exists on both `o` and `li`. Whenever a
//!   query joins both, DuckDB raises `Ambiguous reference to column name`. Loud,
//!   never a wrong number. Home anchoring (#71) never joins a table the
//!   base-anchored path would not have — in a tree of many-to-one edges the
//!   path from a metric's table to the dimensions is a subset of the paths from
//!   the base table to both — so it adds no collision; it removes one where the
//!   home table alone suffices (`metric_alone_binds_its_own_table`).
//! - **Cross-table bare reference** — `amount` exists only on `o`, used bare in
//!   a metric on `li`. Snowflake rejects it at CREATE; `graph::validate_member_references`
//!   (PAR-3) checks only QUALIFIED references, so it is accepted here, and binds
//!   to `o.amount` whenever `o` happens to be joined. That is a silent cross-grain
//!   number (`known_gap_*` below), and whether `o` is joined depends on the
//!   query shape.
//!
//! These tests pin today's behaviour so a change to either shows up red; the
//! `known_gap_*` ones pin a WRONG answer on purpose and flip when #83 is fixed.
//! The documented workaround is to qualify every column
//! (`docs/reference/create-semantic-view.rst`, "Column qualification in
//! expressions").

use super::*;
use crate::expand::test_helpers::TestFixtureExt;
use crate::model::{Cardinality, Join, SemanticViewDefinition, TableRef};

fn table(alias: &str, pk: &str) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: format!("bc_{alias}"),
        pk_columns: vec![pk.to_string()],
        unique_constraints: vec![],
        comment: None,
        synonyms: vec![],
        tags: Vec::new(),
    }
}

fn many_to_one(name: &str, from: &str, to: &str, key: &str) -> Join {
    Join {
        table: to.to_string(),
        from_alias: from.to_string(),
        fk_columns: vec![key.to_string()],
        ref_columns: vec![key.to_string()],
        name: Some(name.to_string()),
        cardinality: Cardinality::ManyToOne,
    }
}

/// customers `c` <- orders `o` <- line items `li`, base `c`. `o_id` is a column
/// of both `o` and `li`; `amount` only of `o`. Every member expression is BARE.
/// `region_u` has no source table, which keeps a query on the base-anchored
/// path (TECH-DEBT #82), so each shape is exercised on both paths.
fn bare_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![table("c", "c_id"), table("o", "o_id"), table("li", "li_id")],
        joins: vec![
            many_to_one("o_to_c", "o", "c", "c_id"),
            many_to_one("li_to_o", "li", "o", "o_id"),
        ],
        ..Default::default()
    }
    .with_dimension("region", "region", Some("c"))
    .with_dimension("region_u", "region", None)
    .with_dimension("order_id", "o_id", Some("o"))
    .with_fact("li_order", "o_id", "li")
    .with_metric("sum_o_oid", "SUM(o_id)", Some("o"))
    .with_metric("sum_li_oid", "SUM(o_id)", Some("li"))
    .with_metric("total_qty", "SUM(qty)", Some("li"))
    .with_metric("li_amount", "SUM(amount)", Some("li"))
}

/// The D1 data shape: an orphan order (13) and an orphan item (103).
fn bare_db() -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE bc_c (c_id INT, region VARCHAR); \
         CREATE TABLE bc_o (o_id INT, c_id INT, amount INT); \
         CREATE TABLE bc_li (li_id INT, o_id INT, qty INT); \
         INSERT INTO bc_c VALUES (1, 'EU'), (2, 'US'); \
         INSERT INTO bc_o VALUES (10, 1, 100), (11, 1, 50), (12, 2, 70), (13, NULL, 5); \
         INSERT INTO bc_li VALUES (100, 10, 1), (101, 10, 2), (102, 11, 3), (103, NULL, 40);",
    )
    .expect("load the fixture");
    conn
}

/// Expand and run; `Ok(rows)` as sorted `a|b|…` strings, or `Err(message)`
/// from expansion or from DuckDB.
fn outcome(dims: &[&str], mets: &[&str], facts: &[&str]) -> Result<Vec<String>, String> {
    let request = QueryRequest {
        where_clause: None,
        facts: facts.iter().map(|f| FactName::new(*f)).collect(),
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    };
    let sql = expand("bc", &bare_def(), &request).map_err(|e| e.to_string())?;
    let cols: Vec<String> = dims
        .iter()
        .chain(facts)
        .chain(mets)
        .map(|c| format!("COALESCE(CAST(q.{} AS VARCHAR), 'NULL')", quote_ident(c)))
        .collect();
    let probe = format!(
        "SELECT concat_ws('|', {}) AS r FROM ({sql}) q ORDER BY r",
        cols.join(", ")
    );
    let conn = bare_db();
    let mut stmt = conn.prepare(&probe).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .map(Result::unwrap)
        .collect();
    Ok(rows)
}

fn assert_ambiguous(result: Result<Vec<String>, String>) {
    match result {
        Err(e) => assert!(
            e.contains("Ambiguous reference to column name \"o_id\""),
            "expected DuckDB's ambiguity error, got: {e}"
        ),
        Ok(rows) => panic!("expected an ambiguity error, got rows {rows:?}"),
    }
}

// --- Same-name collision: loud on every path that joins both tables. --------

/// Metric, home-anchored (`FROM li LEFT JOIN o LEFT JOIN c`).
#[test]
fn metric_with_a_colliding_bare_column_is_ambiguous_on_the_home_path() {
    assert_ambiguous(outcome(&["region"], &["sum_li_oid"], &[]));
}

/// Metric, base-anchored (`FROM c LEFT JOIN o LEFT JOIN li`).
#[test]
fn metric_with_a_colliding_bare_column_is_ambiguous_on_the_base_path() {
    assert_ambiguous(outcome(&["region_u"], &["sum_li_oid"], &[]));
}

/// Dimension, home-anchored: the dimension on `o` is spliced into `li`'s CTE.
#[test]
fn dimension_with_a_colliding_bare_column_is_ambiguous_on_the_home_path() {
    assert_ambiguous(outcome(&["order_id"], &["total_qty"], &[]));
}

/// Dimension, base-anchored.
#[test]
fn dimension_with_a_colliding_bare_column_is_ambiguous_on_the_base_path() {
    assert_ambiguous(outcome(&["order_id", "region_u"], &["total_qty"], &[]));
}

/// Dimension, multi-grain plan: both grain CTEs join `o` and `li`'s CTE also
/// joins `li`.
#[test]
fn dimension_with_a_colliding_bare_column_is_ambiguous_in_a_multi_grain_plan() {
    assert_ambiguous(outcome(&["order_id"], &["sum_o_oid", "total_qty"], &[]));
}

/// Fact (row-level path, base-anchored by design: TECH-DEBT #82).
#[test]
fn fact_with_a_colliding_bare_column_is_ambiguous() {
    assert_ambiguous(outcome(&[], &[], &["region", "li_order"]));
}

// --- Where the home table alone suffices, the bare column binds correctly. --

/// `SUM(o_id)` on `li` with no dimension scans `li` alone: 10 + 10 + 11 = 31,
/// Snowflake's answer. Before #71 the base-anchored `FROM c LEFT JOIN o LEFT
/// JOIN li` made this ambiguous.
#[test]
fn metric_alone_binds_its_own_table() {
    assert_eq!(
        outcome(&[], &["sum_li_oid"], &[]),
        Ok(vec!["31".to_string()])
    );
}

/// A parent-table metric whose joined tables do not share the column: binds to
/// its own `o.o_id` on both paths. Orphan order 13 is the NULL row on the home
/// path; the base-anchored path never sees it.
#[test]
fn metric_without_a_collision_binds_on_both_paths() {
    assert_eq!(
        outcome(&["region"], &["sum_o_oid"], &[]),
        Ok(vec!["EU|21".into(), "NULL|13".into(), "US|12".into()])
    );
    assert_eq!(
        outcome(&["region_u"], &["sum_o_oid"], &[]),
        Ok(vec!["EU|21".into(), "US|12".into()])
    );
}

// --- Cross-table bare reference: accepted at CREATE, bound by accident. -----

/// KNOWN GAP (TECH-DEBT #83): `li.li_amount AS SUM(amount)` names a column of
/// `o`, which Snowflake rejects at CREATE. Grouped by region, `o` is joined, so
/// it binds to `o.amount` and sums each order's amount once PER LINE ITEM
/// (order 10 twice): EU 250 where no line-item column was ever read. Flips when
/// #83 rejects (or qualifies) the reference.
#[test]
fn known_gap_cross_table_bare_column_binds_silently_when_its_table_is_joined() {
    assert_eq!(
        outcome(&["region"], &["li_amount"], &[]),
        Ok(vec!["EU|250".into(), "NULL|NULL".into()])
    );
}

/// KNOWN GAP (TECH-DEBT #83): the same metric with no dimension scans `li`
/// alone, so `amount` does not bind at all. Before #71 this returned the same
/// silent 250 through the base-anchored join; it is now a loud error, which is
/// the side of the gap Snowflake is on (it rejects the definition).
#[test]
fn known_gap_cross_table_bare_column_is_unbound_on_its_home_table() {
    let err = outcome(&[], &["li_amount"], &[]).expect_err("must not bind");
    assert!(err.contains("\"amount\" not found"), "{err}");
}
