//! Which rows a query with dimensions and no metrics lists.
//!
//! Snowflake reads such a query from the one table that every queried
//! dimension's table (and every filtered table) can be reached from by
//! following foreign keys, and LEFT JOINs up from it. So the answer never
//! depends on the order of the TABLES clause (the 2026-11-09 dims-only
//! fixture, whose cases and answers these tests reuse):
//!
//! - one table lists its own distinct values;
//! - a parent and a child list the child's rows, a child row with no parent
//!   under a NULL parent dimension, and a parent with no child not at all;
//! - the two sides of a bridge list the bridge's rows.
//!
//! Each test asks its question of every table order and expects Snowflake's
//! answer from each. They pin ROWS: the generated SQL runs against an
//! in-memory DuckDB, one test per case so each reports on its own.

use super::*;
use crate::model::{Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef};

/// The fixture's data. Orphan has no author, Dan no book, JP no author, and
/// the bridge row (12, 99) names an author who does not exist. Cleo has no
/// orders; order 4 has no customer and order 5 one that does not exist.
fn db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE authors (author_id INT, name VARCHAR, country VARCHAR);
         CREATE TABLE books (book_id INT, title VARCHAR, pages INT);
         CREATE TABLE book_authors (book_id INT, author_id INT, role VARCHAR);
         CREATE TABLE countries (country VARCHAR, continent VARCHAR);
         INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'), (3, 'Cleo', 'UK'),
                                    (4, 'Dan', 'DE');
         INSERT INTO books VALUES (10, 'Duo', 100), (11, 'Mixed', 200), (12, 'Solo', 300),
                                  (13, 'Orphan', 50);
         INSERT INTO book_authors VALUES (10, 1, 'lead'), (10, 2, 'co'), (11, 1, 'lead'),
                                         (11, 3, 'co'), (12, 3, 'lead'), (12, 99, 'ghost');
         INSERT INTO countries VALUES ('FR', 'Europe'), ('UK', 'Europe'), ('DE', 'Europe'),
                                      ('JP', 'Asia');
         CREATE TABLE customers (cust_id INT, cust_name VARCHAR);
         CREATE TABLE orders (order_id INT, cust_id INT, status VARCHAR);
         INSERT INTO customers VALUES (1, 'Ana'), (2, 'Ben'), (3, 'Cleo');
         INSERT INTO orders VALUES (1, 1, 'open'), (2, 1, 'shipped'), (3, 2, 'open'),
                                   (4, NULL, 'lost'), (5, 9, 'stray');",
    )
    .expect("fixture data");
    con
}

fn table(alias: &str, name: &str, pk: &[&str]) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: name.to_string(),
        pk_columns: pk.iter().map(|c| (*c).to_string()).collect(),
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

/// `tables` reordered so `first` is declared first.
fn first(mut tables: Vec<TableRef>, alias: &str) -> Vec<TableRef> {
    let at = tables.iter().position(|t| t.alias == alias).expect("alias");
    let base = tables.remove(at);
    tables.insert(0, base);
    tables
}

/// The fixture's `dv_a` / `dv_c`, declared with `base` first.
fn bridge_def(base: &str) -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: first(
            vec![
                table("a", "authors", &["author_id"]),
                table("b", "books", &["book_id"]),
                table("ba", "book_authors", &["book_id", "author_id"]),
                table("c", "countries", &["country"]),
            ],
            base,
        ),
        dimensions: vec![
            dim("country", "a.country", "a"),
            dim("title", "b.title", "b"),
            dim("role", "ba.role", "ba"),
            dim("continent", "c.continent", "c"),
        ],
        metrics: vec![Metric {
            name: "total_pages".to_string(),
            expr: "SUM(b.pages)".to_string(),
            source_table: Some("b".to_string()),
            ..Default::default()
        }],
        joins: vec![
            rel("ba_author", "ba", "a", "author_id", "author_id"),
            rel("ba_book", "ba", "b", "book_id", "book_id"),
            rel("a_country", "a", "c", "country", "country"),
        ],
        ..Default::default()
    }
}

/// The fixture's `sv_cust` / `sv_ord`, declared with `base` first.
fn orders_def(base: &str) -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: first(
            vec![
                table("cu", "customers", &["cust_id"]),
                table("o", "orders", &["order_id"]),
            ],
            base,
        ),
        dimensions: vec![
            dim("cust_name", "cu.cust_name", "cu"),
            dim("status", "o.status", "o"),
        ],
        metrics: vec![Metric {
            name: "order_count".to_string(),
            expr: "COUNT(o.order_id)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        joins: vec![rel("o_cust", "o", "cu", "cust_id", "cust_id")],
        ..Default::default()
    }
}

/// Every row of the dims-only query, each column as text (`NULL` for a NULL),
/// sorted.
fn run(def: &SemanticViewDefinition, dims: &[&str], where_clause: Option<&str>) -> Vec<String> {
    let req = QueryRequest {
        where_clause: where_clause.map(str::to_string),
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: vec![],
    };
    let sql = expand("v", def, &req).unwrap_or_else(|e| panic!("{e}"));
    let con = db();
    let mut stmt = con
        .prepare(&sql)
        .unwrap_or_else(|e| panic!("emitted SQL must bind, got: {e}\nSQL:\n{sql}"));
    let mut out: Vec<String> = stmt
        .query_map([], |r| {
            let n = r.as_ref().column_count();
            (0..n)
                .map(|i| {
                    let v: Option<String> = r.get(i)?;
                    Ok(v.unwrap_or_else(|| "NULL".to_string()))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|cols| cols.join("/"))
        })
        .expect("query runs")
        .map(Result::unwrap)
        .collect();
    out.sort();
    out
}

fn sorted(rows: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = rows.iter().map(|r| (*r).to_string()).collect();
    out.sort();
    out
}

/// Asks the bridge view, declared in every order, and expects `want` each time.
fn bridge_case(dims: &[&str], where_clause: Option<&str>, want: &[&str]) {
    for base in ["a", "b", "ba", "c"] {
        assert_eq!(
            run(&bridge_def(base), dims, where_clause),
            sorted(want),
            "{dims:?} where {where_clause:?}, {base} declared first"
        );
    }
}

/// Asks the orders view, declared in both orders, and expects `want` each time.
fn orders_case(dims: &[&str], where_clause: Option<&str>, want: &[&str]) {
    for base in ["cu", "o"] {
        assert_eq!(
            run(&orders_def(base), dims, where_clause),
            sorted(want),
            "{dims:?} where {where_clause:?}, {base} declared first"
        );
    }
}

/// D1: a table reached through the bridge lists all its titles, Orphan too.
#[test]
fn d1_far_side_table_alone() {
    bridge_case(&["title"], None, &["Duo", "Mixed", "Orphan", "Solo"]);
}

/// D2: Dan's country is listed although he wrote nothing.
#[test]
fn d2_near_side_table_alone() {
    bridge_case(&["country"], None, &["DE", "FR", "UK"]);
}

/// D3: Asia is listed although no author lives there.
#[test]
fn d3_parent_table_alone() {
    bridge_case(&["continent"], None, &["Asia", "Europe"]);
}

/// D4: the bridge's own column, the dangling row's role included.
#[test]
fn d4_bridge_table_alone() {
    bridge_case(&["role"], None, &["co", "ghost", "lead"]);
}

/// D5 (7.0's B4): the pairs the bridge holds. The dangling row gives Solo with
/// no country; Dan and Orphan are not listed.
#[test]
fn d5_both_sides_of_the_bridge() {
    bridge_case(
        &["title", "country"],
        None,
        &["Duo/FR", "Mixed/FR", "Mixed/UK", "Solo/UK", "Solo/NULL"],
    );
}

/// D6: the bridge and the parent it references. No row for Dan.
#[test]
fn d6_bridge_and_near_side() {
    bridge_case(
        &["role", "country"],
        None,
        &["co/FR", "co/UK", "ghost/NULL", "lead/FR", "lead/UK"],
    );
}

/// D7: the bridge and the other parent. No row for Orphan.
#[test]
fn d7_bridge_and_far_side() {
    bridge_case(
        &["role", "title"],
        None,
        &[
            "co/Duo",
            "co/Mixed",
            "ghost/Solo",
            "lead/Duo",
            "lead/Mixed",
            "lead/Solo",
        ],
    );
}

/// D8: a hop past the bridge and the far side. No row for Asia or Orphan.
#[test]
fn d8_past_the_bridge_and_far_side() {
    bridge_case(
        &["continent", "title"],
        None,
        &["Europe/Duo", "Europe/Mixed", "Europe/Solo", "NULL/Solo"],
    );
}

/// D9: a plain many-to-one chain lists the child's rows. No row for JP's Asia.
#[test]
fn d9_child_and_its_parent() {
    bridge_case(
        &["continent", "country"],
        None,
        &["Europe/DE", "Europe/FR", "Europe/UK"],
    );
}

/// D10: a filter on the other side of the bridge.
#[test]
fn d10_filter_across_the_bridge() {
    bridge_case(&["title"], Some("a.country = 'FR'"), &["Duo", "Mixed"]);
}

/// D11: the same, the other way round.
#[test]
fn d11_filter_across_the_bridge_reversed() {
    bridge_case(&["country"], Some("b.title = 'Duo'"), &["FR"]);
}

/// D12: a filter on a parent of the dimension's table.
#[test]
fn d12_filter_on_a_parent() {
    bridge_case(&["country"], Some("c.continent = 'Asia'"), &[]);
}

/// S1: every customer, Cleo too.
#[test]
fn s1_parent_alone() {
    orders_case(&["cust_name"], None, &["Ana", "Ben", "Cleo"]);
}

/// S2: every order's status, the orders with no or a missing customer too.
#[test]
fn s2_child_alone() {
    orders_case(&["status"], None, &["lost", "open", "shipped", "stray"]);
}

/// S3: the orders' rows. Cleo has none, so she is not listed.
#[test]
fn s3_parent_and_child() {
    orders_case(
        &["cust_name", "status"],
        None,
        &[
            "Ana/open",
            "Ana/shipped",
            "Ben/open",
            "NULL/lost",
            "NULL/stray",
        ],
    );
}

/// S4: the parent filtered by the child.
#[test]
fn s4_parent_filtered_by_child() {
    orders_case(&["cust_name"], Some("o.status = 'open'"), &["Ana", "Ben"]);
}

/// S5: the child filtered by the parent.
#[test]
fn s5_child_filtered_by_parent() {
    orders_case(
        &["status"],
        Some("cu.cust_name = 'Ana'"),
        &["open", "shipped"],
    );
}
