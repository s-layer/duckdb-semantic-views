//! Inline-query logical tables (Phase 6.4): `TABLES (alias AS (<query>) ...)`.
//!
//! A table defined by a query is read wherever a physical table would be: as
//! the view's root, as a joined table, and as the anchor of a metric's own
//! grain. These tests pin NUMBERS: each executes the generated SQL against an
//! in-memory DuckDB.

use super::*;
use crate::model::{Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef};

/// Four customers in four cities, and their orders. Di's only order is a
/// refund (a negative amount).
///
/// | customer | region | city | orders  |
/// |----------|--------|------|---------|
/// | 1 Ann    | N      | Oslo | 5, 7    |
/// | 2 Bob    | S      | Rome | 11      |
/// | 3 Cy     | N      | Oslo | 1       |
/// | 4 Di     | S      | Pisa | -3      |
fn shop_db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE customers (id INTEGER, name VARCHAR, region VARCHAR);
         CREATE TABLE addresses (cust_id INTEGER, city VARCHAR);
         CREATE TABLE orders (id INTEGER, cust_id INTEGER, amount INTEGER);
         INSERT INTO customers VALUES (1, 'Ann', 'N'), (2, 'Bob', 'S'), (3, 'Cy', 'N'),
                                      (4, 'Di', 'S');
         INSERT INTO addresses VALUES (1, 'Oslo'), (2, 'Rome'), (3, 'Oslo'), (4, 'Pisa');
         INSERT INTO orders VALUES (10, 1, 5), (11, 1, 7), (12, 2, 11), (13, 3, 1),
                                   (14, 4, -3);",
    )
    .expect("fixture data");
    con
}

fn physical(alias: &str, name: &str, pk: &str) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: name.to_string(),
        pk_columns: vec![pk.to_string()],
        ..Default::default()
    }
}

fn inline(alias: &str, query: &str, pk: &str) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        definition: Some(query.to_string()),
        pk_columns: vec![pk.to_string()],
        ..Default::default()
    }
}

fn dim(name: &str, expr: &str, table: &str) -> Dimension {
    Dimension {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(table.to_string()),
        ..Default::default()
    }
}

fn metric(name: &str, expr: &str, table: &str) -> Metric {
    Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(table.to_string()),
        ..Default::default()
    }
}

fn rel(name: &str, from: &str, to: &str, fk: &str, pk: &str) -> Join {
    Join {
        table: to.to_string(),
        from_alias: from.to_string(),
        fk_columns: vec![fk.to_string()],
        ref_columns: vec![pk.to_string()],
        name: Some(name.to_string()),
        cardinality: Cardinality::ManyToOne,
        ..Default::default()
    }
}

/// Snowflake's documented example: a customer table defined as a join of two
/// physical tables, referenced by the orders.
fn customer_info_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            physical("o", "orders", "id"),
            inline(
                "ci",
                "SELECT * FROM customers JOIN addresses ON customers.id = addresses.cust_id",
                "id",
            ),
        ],
        dimensions: vec![dim("city", "ci.city", "ci")],
        metrics: vec![
            metric("revenue", "SUM(o.amount)", "o"),
            metric("customer_count", "COUNT(*)", "ci"),
        ],
        joins: vec![rel("o_ci", "o", "ci", "cust_id", "id")],
        ..Default::default()
    }
}

/// Paid orders only, as the view's root table.
fn paid_orders_def(query: &str) -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![inline("o", query, "id"), physical("c", "customers", "id")],
        dimensions: vec![dim("region", "c.region", "c")],
        metrics: vec![metric("revenue", "SUM(o.amount)", "o")],
        joins: vec![rel("o_c", "o", "c", "cust_id", "id")],
        ..Default::default()
    }
}

fn req(dims: &[&str], mets: &[&str]) -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    }
}

/// Every row of `sql`, each column rendered as text, sorted.
fn rows(sql: &str) -> Vec<Vec<String>> {
    let con = shop_db();
    let mut stmt = con
        .prepare(sql)
        .unwrap_or_else(|e| panic!("emitted SQL must bind, got: {e}\nSQL:\n{sql}"));
    let mut out: Vec<Vec<String>> = stmt
        .query_map([], |r| {
            let n = r.as_ref().column_count();
            (0..n)
                .map(|i| {
                    let v: duckdb::types::Value = r.get(i)?;
                    Ok(match v {
                        duckdb::types::Value::Null => "NULL".to_string(),
                        duckdb::types::Value::Text(s) => s,
                        other => format!("{other:?}")
                            .trim_start_matches(|c: char| c.is_alphabetic())
                            .trim_matches(|c| c == '(' || c == ')')
                            .to_string(),
                    })
                })
                .collect()
        })
        .expect("query runs")
        .map(Result::unwrap)
        .collect();
    out.sort();
    out
}

fn table_of(text: &[&[&str]]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = text
        .iter()
        .map(|row| row.iter().map(|c| (*c).to_string()).collect())
        .collect();
    out.sort();
    out
}

fn run(def: &SemanticViewDefinition, dims: &[&str], mets: &[&str]) -> Vec<Vec<String>> {
    let sql = expand("v", def, &req(dims, mets)).unwrap_or_else(|e| panic!("{e}"));
    rows(&sql)
}

/// A joined table defined by a query: revenue by the city its join supplies.
#[test]
fn a_joined_inline_query_table_supplies_its_columns() {
    assert_eq!(
        run(&customer_info_def(), &["city"], &["revenue"]),
        table_of(&[&["Oslo", "13"], &["Pisa", "-3"], &["Rome", "11"]])
    );
}

/// The view's root defined by a query: the refund it filters out is not
/// counted, so region S has Bob's order alone.
#[test]
fn a_root_inline_query_table_is_read_through_its_query() {
    assert_eq!(
        run(
            &paid_orders_def("SELECT * FROM orders WHERE amount > 0"),
            &["region"],
            &["revenue"]
        ),
        table_of(&[&["N", "13"], &["S", "11"]])
    );
}

/// A metric on the query table and one on the orders, by a dimension on the
/// query table: each metric at its own grain, so the query table anchors one
/// of them.
#[test]
fn an_inline_query_table_anchors_its_own_metrics_grain() {
    assert_eq!(
        run(
            &customer_info_def(),
            &["city"],
            &["revenue", "customer_count"]
        ),
        table_of(&[
            &["Oslo", "13", "2"],
            &["Pisa", "-3", "1"],
            &["Rome", "11", "1"],
        ])
    );
    assert_eq!(
        run(&customer_info_def(), &[], &["customer_count"]),
        table_of(&[&["4"]])
    );
}

/// A query table may aggregate: a lifetime value per customer, which a
/// dimension then buckets.
#[test]
fn an_aggregating_inline_query_table_is_a_dimension_source() {
    let def = SemanticViewDefinition {
        tables: vec![
            physical("c", "customers", "id"),
            inline(
                "ltv",
                "SELECT cust_id, SUM(amount) AS lifetime FROM orders GROUP BY cust_id",
                "cust_id",
            ),
        ],
        dimensions: vec![dim(
            "tier",
            "CASE WHEN ltv.lifetime >= 12 THEN 'high' ELSE 'low' END",
            "ltv",
        )],
        metrics: vec![metric("customer_count", "COUNT(*)", "c")],
        joins: vec![rel("c_ltv", "c", "ltv", "id", "cust_id")],
        ..Default::default()
    };
    assert_eq!(
        run(&def, &["tier"], &["customer_count"]),
        table_of(&[&["high", "1"], &["low", "3"]])
    );
}

/// The query goes on lines of its own, so a trailing line comment in it (the
/// YAML form keeps comments) cannot swallow the closing parenthesis.
#[test]
fn a_trailing_line_comment_in_the_query_stays_inside_it() {
    let def = paid_orders_def("SELECT * FROM orders WHERE amount > 0 -- refunds are left out");
    let sql = expand("v", &def, &req(&["region"], &["revenue"])).unwrap();
    assert!(
        sql.contains(
            "FROM (\nSELECT * FROM orders WHERE amount > 0 -- refunds are left out\n) AS \"o\""
        ),
        "{sql}"
    );
    assert_eq!(rows(&sql), table_of(&[&["N", "13"], &["S", "11"]]));
}
