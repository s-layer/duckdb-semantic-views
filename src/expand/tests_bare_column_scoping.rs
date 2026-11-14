//! Bare (unqualified) column references inside member expressions, on the
//! home-anchored path and the base-anchored path (TECH-DEBT #83).
//!
//! Snowflake scopes a member expression to its own logical table: a bare
//! column means "this table's column", and a column of another table is a
//! CREATE error. CREATE records each table's columns
//! (`SemanticViewDefinition::table_columns`), and the expansion qualifies a
//! bare column with the member's own table when that table has it
//! (`expand::bare_columns`). Two shapes follow:
//!
//! - **Same-name collision** -- `o_id` exists on both `o` and `li`. A member on
//!   either reads its own table's `o_id` whatever the query joins. Before #83
//!   a query joining both raised DuckDB's `Ambiguous reference to column name`.
//! - **Cross-table bare reference** -- `amount` exists only on `o`, used bare in
//!   a metric on `li`. CREATE refuses it, as Snowflake does
//!   (`bare_columns::check_bare_columns`); before #83 a definition that held it
//!   bound it to `o.amount` whenever `o` happened to be joined.
//!
//! A definition stored before 0.19.0 has no column lists, so its bare columns
//! are spliced as written, as then (`stored_without_column_lists_*`).

use super::*;
use crate::expand::test_helpers::TestFixtureExt;
use crate::model::{Cardinality, Join, SemanticViewDefinition, TableRef};

fn table(alias: &str, pk: &str) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: format!("bc_{alias}"),
        definition: None,
        pk_columns: vec![pk.to_string()],
        unique_constraints: vec![],
        comment: None,
        synonyms: vec![],
        tags: Vec::new(),
        distinct_ranges: vec![],
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
        asof: None,
        range_end: None,
        generated_name: false,
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
        table_columns: [
            ("c", &["c_id", "region"][..]),
            ("o", &["o_id", "c_id", "amount"][..]),
            ("li", &["li_id", "o_id", "qty"][..]),
        ]
        .into_iter()
        .map(|(t, cols)| {
            (
                t.to_string(),
                cols.iter().map(ToString::to_string).collect(),
            )
        })
        .collect(),
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
    outcome_of(&bare_def(), dims, mets, facts)
}

/// [`outcome`] over `def`.
fn outcome_of(
    def: &SemanticViewDefinition,
    dims: &[&str],
    mets: &[&str],
    facts: &[&str],
) -> Result<Vec<String>, String> {
    let request = QueryRequest {
        where_clause: None,
        facts: facts.iter().map(|f| FactName::new(*f)).collect(),
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    };
    let sql = expand("bc", def, &request).map_err(|e| e.to_string())?;
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

fn rows(rows: &[&str]) -> Result<Vec<String>, String> {
    Ok(rows.iter().map(ToString::to_string).collect())
}

// --- Same-name collision: each member reads its own table's column. --------

/// Metric, home-anchored (`FROM li LEFT JOIN o LEFT JOIN c`): `li.o_id`, so
/// EU 10 + 10 + 11 and the orphan item's NULL. Was an ambiguity error.
#[test]
fn metric_with_a_colliding_bare_column_reads_its_own_table_on_the_home_path() {
    assert_eq!(
        outcome(&["region"], &["sum_li_oid"], &[]),
        rows(&["EU|31", "NULL|NULL"])
    );
}

/// Metric, base-anchored (`FROM c LEFT JOIN o LEFT JOIN li`): US has no item.
#[test]
fn metric_with_a_colliding_bare_column_reads_its_own_table_on_the_base_path() {
    assert_eq!(
        outcome(&["region_u"], &["sum_li_oid"], &[]),
        rows(&["EU|31", "US|NULL"])
    );
}

/// Dimension, home-anchored: the dimension on `o` reads `o.o_id` inside `li`'s
/// query, the orphan item's order being NULL.
#[test]
fn dimension_with_a_colliding_bare_column_reads_its_own_table_on_the_home_path() {
    assert_eq!(
        outcome(&["order_id"], &["total_qty"], &[]),
        rows(&["10|3", "11|3", "NULL|40"])
    );
}

/// Dimension, base-anchored.
#[test]
fn dimension_with_a_colliding_bare_column_reads_its_own_table_on_the_base_path() {
    assert_eq!(
        outcome(&["order_id", "region_u"], &["total_qty"], &[]),
        rows(&["10|EU|3", "11|EU|3", "12|US|NULL"])
    );
}

/// Dimension, multi-grain plan: both grain CTEs join `o` and `li`'s CTE also
/// joins `li`.
#[test]
fn dimension_with_a_colliding_bare_column_reads_its_own_table_in_a_multi_grain_plan() {
    assert_eq!(
        outcome(&["order_id"], &["sum_o_oid", "total_qty"], &[]),
        rows(&[
            "10|10|3",
            "11|11|3",
            "12|12|NULL",
            "13|13|NULL",
            "NULL|NULL|40"
        ])
    );
}

/// Fact beside a dimension on `c`: the FACTS query reads `li` and joins `o`
/// and `c` up to the dimension; the fact reads `li.o_id`.
#[test]
fn fact_with_a_colliding_bare_column_reads_its_own_table() {
    assert_eq!(
        outcome(&[], &[], &["region", "li_order"]),
        rows(&["EU|10", "EU|10", "EU|11", "NULL|NULL"])
    );
}

/// `SUM(o_id)` on `li` with no dimension scans `li` alone: 10 + 10 + 11 = 31,
/// Snowflake's answer.
#[test]
fn metric_alone_binds_its_own_table() {
    assert_eq!(outcome(&[], &["sum_li_oid"], &[]), rows(&["31"]));
}

/// A parent-table metric whose joined tables do not share the column: binds to
/// its own `o.o_id` on both paths. Orphan order 13 is the NULL row on the home
/// path; the base-anchored path never sees it.
#[test]
fn metric_without_a_collision_binds_on_both_paths() {
    assert_eq!(
        outcome(&["region"], &["sum_o_oid"], &[]),
        rows(&["EU|21", "NULL|13", "US|12"])
    );
    assert_eq!(
        outcome(&["region_u"], &["sum_o_oid"], &[]),
        rows(&["EU|21", "US|12"])
    );
}

// --- Cross-table bare reference: refused at CREATE. -------------------------

/// `li.li_amount AS SUM(amount)` names a column of `o` alone, which Snowflake
/// rejects at CREATE. Before #83 it bound to `o.amount` whenever `o` was joined
/// and summed each order's amount once per line item (EU 250).
#[test]
fn cross_table_bare_column_is_refused() {
    let err = super::bare_columns::check_bare_columns("bc", &bare_def()).expect_err("refused");
    assert!(
        err.contains("metric 'li_amount'")
            && err.contains("'amount', a column of table 'o'")
            && err.contains("its own table ('li')"),
        "{err}"
    );
}

/// The view without that metric passes the check.
#[test]
fn own_table_bare_columns_pass_the_check() {
    let mut def = bare_def();
    def.metrics.retain(|m| m.name != "li_amount");
    assert_eq!(super::bare_columns::check_bare_columns("bc", &def), Ok(()));
}

// --- A definition stored before 0.19.0 has no column lists. -----------------

/// Its bare columns are spliced as written, as then: the collision is still
/// DuckDB's ambiguity error, loud rather than a wrong number. Re-creating the
/// view records the column lists.
#[test]
fn stored_without_column_lists_keeps_the_ambiguity() {
    let mut def = bare_def();
    def.table_columns.clear();
    assert_ambiguous(outcome_of(&def, &["region"], &["sum_li_oid"], &[]));
}
