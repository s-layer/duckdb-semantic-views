//! Queries on views where one table is referenced by two different tables,
//! which CREATE used to refuse unless the shared table was declared first
//! ("diamond: ... reachable from multiple tables"). Snowflake creates them in
//! any TABLES order (the 2026-11-10 fixture 3) and decides per query:
//!
//! - `qv`, two bridges between books and authors (`book_authors`,
//!   `book_editors`): a query that crosses from one side to the other has two
//!   bridges to cross by, and Snowflake refuses it with 000002 "Unsupported
//!   feature 'Multi-path in many-to-many relationship'" (Q2, Q3, Q7, Q8). A
//!   query that stays on one bridge is answered (Q4-Q6, Q9).
//! - `hv`, reviews of books bridged to authors: a review metric by a book
//!   dimension (H5) and with none (H2) are answered.
//! - `bv`, balances of accounts held by people through `holders`: a
//!   semi-additive balance by account (S8) and with no dimension (S9) are
//!   answered; by person it crosses the bridge and is refused (S7).
//!
//! - `pv`, books with a lead author (`b_lead`) beside the `book_authors`
//!   bridge, its dimensions written unqualified as in the fixture: a metric
//!   whose `USING (b_lead)` names the one relationship from books to authors
//!   is answered by an author dimension (fixture 5, P2).
//!
//! - `xv`, fixture 5's diamond (orders reference customers and stores, both
//!   reference regions): a customer metric by a store member, and the other
//!   way round, has no chain of relationships to follow and two paths (through
//!   orders, through regions), and Snowflake refuses it (000002, X1, X2, X4),
//!   as it does a region metric by a customer or an order (X5, X6). Customers
//!   and stores with no metric list the pairs orders hold (X3).
//! - `pv` again: books reference authors directly, so pages by country follow
//!   the lead author (P1, P7); a member reached by no chain is refused when it
//!   is also reached a second way (P3, P4), as in Snowflake.
//!
//! Each test asks its question of every TABLES order and pins ROWS: the
//! generated SQL runs against an in-memory DuckDB, one test per case so each
//! reports on its own.

use super::*;
use crate::model::{
    Cardinality, Dimension, Join, Metric, NonAdditiveDim, SemanticViewDefinition, TableRef,
};

fn db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE authors (author_id INT, name VARCHAR, country VARCHAR);
         CREATE TABLE books (book_id INT, title VARCHAR, pages INT);
         CREATE TABLE book_authors (book_id INT, author_id INT, role VARCHAR);
         CREATE TABLE book_editors (book_id INT, author_id INT);
         INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'), (3, 'Cleo', 'UK'), (4, 'Dan', 'DE');
         INSERT INTO books VALUES (10, 'Duo', 100), (11, 'Mixed', 200), (12, 'Solo', 300), (13, 'Orphan', 50);
         INSERT INTO book_authors VALUES (10, 1, 'lead'), (10, 2, 'co'), (11, 1, 'lead'), (11, 3, 'co'), (12, 3, 'lead');
         INSERT INTO book_editors VALUES (12, 2);
         CREATE TABLE lead_books (book_id INT, title VARCHAR, pages INT, lead_id INT);
         INSERT INTO lead_books VALUES (10, 'Duo', 100, 1), (11, 'Mixed', 200, 3), (12, 'Solo', 300, 3), (13, 'Orphan', 50, NULL);
         CREATE TABLE reviews (review_id INT, book_id INT, stars INT);
         INSERT INTO reviews VALUES (1, 10, 5), (2, 10, 3), (3, 12, 4), (4, 13, 2), (5, NULL, 1);
         CREATE TABLE people (person_id INT, person_name VARCHAR);
         CREATE TABLE accounts (acct_id VARCHAR, acct_name VARCHAR);
         CREATE TABLE holders (acct_id VARCHAR, person_id INT);
         CREATE TABLE balances (bal_id INT, acct_id VARCHAR, snap_date DATE, balance INT);
         INSERT INTO people VALUES (1, 'Ana'), (2, 'Ben'), (3, 'Cleo');
         INSERT INTO accounts VALUES ('A', 'Joint'), ('B', 'Solo');
         INSERT INTO holders VALUES ('A', 1), ('A', 2), ('B', 2);
         INSERT INTO balances VALUES (1, 'A', '2026-01-01', 100), (2, 'A', '2026-02-01', 150),
                                     (3, 'B', '2026-01-01', 30), (4, 'B', '2026-02-01', 40);
         CREATE TABLE x_regions (region_id INT, region_name VARCHAR);
         CREATE TABLE x_customers (cust_id INT, cust_name VARCHAR, region_id INT);
         CREATE TABLE x_stores (store_id INT, store_name VARCHAR, region_id INT);
         CREATE TABLE x_orders (order_id INT, cust_id INT, store_id INT, amount INT);
         INSERT INTO x_regions VALUES (1, 'EU'), (2, 'US');
         INSERT INTO x_customers VALUES (1, 'Ana', 1), (2, 'Bo', 2);
         INSERT INTO x_stores VALUES (1, 'S-US', 2), (2, 'S-EU', 1);
         INSERT INTO x_orders VALUES (1, 1, 1, 10), (2, 2, 1, 20), (3, 1, 2, 40);
         CREATE TABLE s_cust (id INT, name VARCHAR);
         CREATE TABLE s_orders (id INT, c_id INT, amount INT);
         CREATE TABLE s_tickets (id INT, c_id INT);
         CREATE TABLE s_visits (id INT, c_id INT);
         CREATE TABLE s_pairs (id INT, x1 INT, x2 INT);
         INSERT INTO s_cust VALUES (1, 'A'), (2, 'B');
         INSERT INTO s_orders VALUES (1, 1, 10), (2, 1, 20), (3, 2, 5), (4, NULL, 7);
         INSERT INTO s_tickets VALUES (1, 1);
         INSERT INTO s_visits VALUES (1, 2);
         INSERT INTO s_pairs VALUES (1, 1, 1);
         CREATE TABLE b_orders (id INT, c_id INT, amount INT);
         CREATE TABLE b_cust (id INT, r_id INT);
         CREATE TABLE b_regions (id INT, name VARCHAR);
         CREATE TABLE b_shipments (id INT, o_id INT, r_id INT);
         INSERT INTO b_orders VALUES (1, 1, 10);
         INSERT INTO b_cust VALUES (1, 1);
         INSERT INTO b_regions VALUES (1, 'R1'), (2, 'R2');
         INSERT INTO b_shipments VALUES (1, 1, 2);
         CREATE TABLE f_regions (id INT, name VARCHAR);
         CREATE TABLE f_carriers (id INT, r_id INT, fee INT);
         CREATE TABLE f_airports (id INT, r_id INT);
         CREATE TABLE f_flights (id INT, c_id INT, dep INT, arr INT);
         INSERT INTO f_regions VALUES (1, 'R1'), (2, 'R2');
         INSERT INTO f_carriers VALUES (1, 1, 10), (2, 2, 20);
         INSERT INTO f_airports VALUES (1, 2);
         INSERT INTO f_flights VALUES (1, 1, 1, 1);
         CREATE TABLE n_cust (id INT, signup DATE);
         CREATE TABLE n_orders (id INT, c_id INT, amount INT);
         CREATE TABLE n_stores (id INT, c_id INT);
         INSERT INTO n_cust VALUES (1, '2026-01-01'), (2, '2026-02-01');
         INSERT INTO n_orders VALUES (1, 1, 10), (2, 2, 20), (3, 2, 5);
         INSERT INTO n_stores VALUES (1, 1);
         CREATE TABLE g_regions (id INT, name VARCHAR);
         CREATE TABLE g_cust (id INT, r_id INT, v INT);
         CREATE TABLE g_stores (id INT, r_id INT);
         CREATE TABLE g_orders (id INT, c_id INT, s_id INT);
         INSERT INTO g_regions VALUES (1, 'R1'), (2, 'R2');
         INSERT INTO g_cust VALUES (1, 1, 10);
         INSERT INTO g_stores VALUES (1, 2);
         INSERT INTO g_orders VALUES (1, 1, 1);
         CREATE TABLE k_country (id INT, name VARCHAR);
         CREATE TABLE k_airports (id INT, k_id INT);
         CREATE TABLE k_flights (id INT, dep INT, arr INT);
         INSERT INTO k_country VALUES (1, 'FR'), (2, 'UK');
         INSERT INTO k_airports VALUES (1, 1), (2, 2);
         INSERT INTO k_flights VALUES (1, 1, 2), (2, 2, 2);
         CREATE TABLE m_root (id INT, name VARCHAR);
         CREATE TABLE m_x (id INT, r_id INT);
         CREATE TABLE m_m (id INT, x_id INT);
         CREATE TABLE m_a (id INT, r_id INT, m_id INT);
         INSERT INTO m_root VALUES (1, 'R1'), (2, 'R2');
         INSERT INTO m_x VALUES (1, 1);
         INSERT INTO m_m VALUES (1, 1), (2, 1);
         INSERT INTO m_a VALUES (1, 2, 1), (2, 2, 1);",
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

/// Every order of `tables`, by index.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![vec![]];
    }
    let mut out = Vec::new();
    for rest in permutations(n - 1) {
        for at in 0..=rest.len() {
            let mut p = rest.clone();
            p.insert(at, n - 1);
            out.push(p);
        }
    }
    out
}

/// `def` once per TABLES order, each with the order's aliases for messages.
fn every_order(def: &SemanticViewDefinition) -> Vec<(String, SemanticViewDefinition)> {
    permutations(def.tables.len())
        .into_iter()
        .map(|p| {
            let mut d = def.clone();
            d.tables = p.iter().map(|&i| def.tables[i].clone()).collect();
            let names: Vec<&str> = d.tables.iter().map(|t| t.alias.as_str()).collect();
            (names.join(","), d)
        })
        .collect()
}

/// `qv`: two bridges between books and authors.
fn qv() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("b", "books", &["book_id"]),
            table("a", "authors", &["author_id"]),
            table("ba", "book_authors", &["book_id", "author_id"]),
            table("be", "book_editors", &["book_id", "author_id"]),
        ],
        joins: vec![
            rel("ba_author", "ba", "a", "author_id", "author_id"),
            rel("ba_book", "ba", "b", "book_id", "book_id"),
            rel("be_author", "be", "a", "author_id", "author_id"),
            rel("be_book", "be", "b", "book_id", "book_id"),
        ],
        dimensions: vec![
            dim("country", "a.country", "a"),
            dim("title", "b.title", "b"),
            dim("role", "ba.role", "ba"),
        ],
        metrics: vec![
            metric("total_pages", "SUM(b.pages)", "b"),
            metric("author_count", "COUNT(a.author_id)", "a"),
            metric("credits", "COUNT(*)", "ba"),
            metric("edits", "COUNT(*)", "be"),
        ],
        ..Default::default()
    }
}

/// `hv`: reviews -> books <- book_authors -> authors.
fn hv() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("a", "authors", &["author_id"]),
            table("b", "books", &["book_id"]),
            table("ba", "book_authors", &["book_id", "author_id"]),
            table("rv", "reviews", &["review_id"]),
        ],
        joins: vec![
            rel("ba_author", "ba", "a", "author_id", "author_id"),
            rel("ba_book", "ba", "b", "book_id", "book_id"),
            rel("rv_book", "rv", "b", "book_id", "book_id"),
        ],
        dimensions: vec![
            dim("country", "a.country", "a"),
            dim("title", "b.title", "b"),
            dim("stars", "rv.stars", "rv"),
        ],
        metrics: vec![
            metric("review_count", "COUNT(rv.review_id)", "rv"),
            metric("total_stars", "SUM(rv.stars)", "rv"),
            metric("total_pages", "SUM(b.pages)", "b"),
        ],
        ..Default::default()
    }
}

/// `bv`: balances -> accounts <- holders -> people (fixture 3's `sv_bal`).
fn bv() -> SemanticViewDefinition {
    let mut last = metric("last_balance", "SUM(bl.balance)", "bl");
    last.non_additive_by = vec![NonAdditiveDim {
        dimension: "snap_date".to_string(),
        ..Default::default()
    }];
    SemanticViewDefinition {
        tables: vec![
            table("pe", "people", &["person_id"]),
            table("ac", "accounts", &["acct_id"]),
            table("h", "holders", &["acct_id", "person_id"]),
            table("bl", "balances", &["bal_id"]),
        ],
        joins: vec![
            rel("h_person", "h", "pe", "person_id", "person_id"),
            rel("h_acct", "h", "ac", "acct_id", "acct_id"),
            rel("bl_acct", "bl", "ac", "acct_id", "acct_id"),
        ],
        dimensions: vec![
            dim("person_name", "pe.person_name", "pe"),
            dim("acct_name", "ac.acct_name", "ac"),
            dim("snap_date", "bl.snap_date", "bl"),
        ],
        metrics: vec![last, metric("total_balance", "SUM(bl.balance)", "bl")],
        ..Default::default()
    }
}

/// `pv`: books reference their lead author directly and all their authors
/// through `book_authors`. Dimension and metric expressions are unqualified,
/// as fixture 5 writes them (`a.country AS country`).
fn pv() -> SemanticViewDefinition {
    let mut by_lead = metric("pages_by_lead", "SUM(pages)", "b");
    by_lead.using_relationships = vec!["b_lead".to_string()];
    SemanticViewDefinition {
        tables: vec![
            table("b", "lead_books", &["book_id"]),
            table("a", "authors", &["author_id"]),
            table("ba", "book_authors", &["book_id", "author_id"]),
        ],
        joins: vec![
            rel("b_lead", "b", "a", "lead_id", "author_id"),
            rel("ba_author", "ba", "a", "author_id", "author_id"),
            rel("ba_book", "ba", "b", "book_id", "book_id"),
        ],
        dimensions: vec![
            dim("country", "country", "a"),
            dim("title", "title", "b"),
            dim("role", "role", "ba"),
        ],
        metrics: vec![
            metric("total_pages", "SUM(pages)", "b"),
            by_lead,
            metric("author_count", "COUNT(author_id)", "a"),
            metric("credits", "COUNT(*)", "ba"),
        ],
        ..Default::default()
    }
}

/// `xv`: fixture 5's diamond. Orders reference customers and stores, and both
/// reference regions, so customers and stores are linked two ways: through the
/// orders between them (many-to-many) and through their regions.
fn xv() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("o", "x_orders", &["order_id"]),
            table("c", "x_customers", &["cust_id"]),
            table("s", "x_stores", &["store_id"]),
            table("r", "x_regions", &["region_id"]),
        ],
        joins: vec![
            rel("o_cust", "o", "c", "cust_id", "cust_id"),
            rel("o_store", "o", "s", "store_id", "store_id"),
            rel("c_region", "c", "r", "region_id", "region_id"),
            rel("s_region", "s", "r", "region_id", "region_id"),
        ],
        dimensions: vec![
            dim("order_id", "order_id", "o"),
            dim("cust_name", "cust_name", "c"),
            dim("store_name", "store_name", "s"),
            dim("region_name", "region_name", "r"),
        ],
        metrics: vec![
            metric("revenue", "SUM(amount)", "o"),
            metric("customer_count", "COUNT(cust_id)", "c"),
            metric("store_count", "COUNT(store_id)", "s"),
            metric("region_count", "COUNT(region_id)", "r"),
        ],
        ..Default::default()
    }
}

fn req(dims: &[&str], mets: &[&str], where_clause: Option<&str>) -> QueryRequest {
    QueryRequest {
        where_clause: where_clause.map(str::to_string),
        // PIN: no case here queries facts; the views declare none.
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    }
}

/// Every row of `sql`, each column rendered as text (`NULL` for a NULL), sorted.
fn rows(sql: &str) -> Vec<Vec<String>> {
    let con = db();
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

/// CREATE accepts the view, and the query gives `expected`, in every TABLES order.
fn assert_answer(view: &SemanticViewDefinition, request: &QueryRequest, expected: &[&[&str]]) {
    for (order, def) in every_order(view) {
        if let Err(e) = crate::graph::validate_graph(&def) {
            panic!(
                "TABLES ({order}): CREATE must accept the view, got: {}",
                e.message
            );
        }
        let sql = expand("v", &def, request).unwrap_or_else(|e| panic!("TABLES ({order}): {e}"));
        assert_eq!(rows(&sql), table_of(expected), "TABLES ({order})");
    }
}

/// The query is refused in every TABLES order as one that reaches a member by
/// no chain of relationships and by more than one path across a many-to-many
/// relationship (Snowflake's 000002), naming each path.
fn assert_many_to_many(view: &SemanticViewDefinition, request: &QueryRequest, paths: &[&str]) {
    for (order, def) in every_order(view) {
        match expand("v", &def, request) {
            Ok(sql) => panic!(
                "TABLES ({order}): expected a many-to-many refusal, got rows {:?}\nSQL:\n{sql}",
                rows(&sql)
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("many-to-many") && paths.iter().all(|p| msg.contains(p)),
                    "TABLES ({order}): expected a many-to-many refusal naming {paths:?}, \
                     got: {msg}"
                );
            }
        }
    }
}

/// CREATE accepts each view in every TABLES order.
#[test]
fn every_view_is_created_in_every_tables_order() {
    for view in [qv(), hv(), bv(), pv(), xv()] {
        for (order, def) in every_order(&view) {
            assert!(
                crate::graph::validate_graph(&def).is_ok(),
                "TABLES ({order}): {:?}",
                crate::graph::validate_graph(&def).err().map(|e| e.message)
            );
        }
    }
}

/// Q2 / Q10: pages by author country reach authors through either bridge.
#[test]
fn q2_book_metric_by_author_dimension_is_refused() {
    assert_many_to_many(
        &qv(),
        &req(&["country"], &["total_pages"], None),
        &["b <- ba -> a", "b <- be -> a"],
    );
}

/// The same refusal when the author side is only filtered on.
#[test]
fn q2_book_metric_filtered_on_authors_is_refused() {
    assert_many_to_many(
        &qv(),
        &req(&[], &["total_pages"], Some("a.country = 'FR'")),
        &["b <- ba -> a", "b <- be -> a"],
    );
}

/// Q3: author count by title, the other direction.
#[test]
fn q3_author_metric_by_book_dimension_is_refused() {
    assert_many_to_many(
        &qv(),
        &req(&["title"], &["author_count"], None),
        &["a <- ba -> b", "a <- be -> b"],
    );
}

/// Q7: pages by a dimension on one bridge. Books reach `ba` directly, and
/// through the other bridge and authors.
#[test]
fn q7_book_metric_by_bridge_dimension_is_refused() {
    assert_many_to_many(
        &qv(),
        &req(&["role"], &["total_pages"], None),
        &["b <- ba", "b <- be -> a <- ba"],
    );
}

/// Q8: titles and countries with no metric: both bridges pair them.
#[test]
fn q8_dims_only_across_two_bridges_is_refused() {
    assert_many_to_many(
        &qv(),
        &req(&["title", "country"], &[], None),
        &["a <- ba -> b", "a <- be -> b"],
    );
}

/// Q8 with a materialization of exactly those dimensions (review of #342): it
/// was routed to the materialization before the refusal was asked.
#[test]
fn q8_is_refused_beside_a_materialization_of_its_dimensions() {
    let mut view = qv();
    view.materializations = vec![crate::model::Materialization {
        name: "titles_by_country".to_string(),
        table: "titles_by_country".to_string(),
        dimensions: vec!["title".to_string(), "country".to_string()],
        metrics: vec![],
    }];
    assert_many_to_many(
        &view,
        &req(&["title", "country"], &[], None),
        &["a <- ba -> b", "a <- be -> b"],
    );
}

/// Q4: pages with no dimension: 650.
#[test]
fn q4_book_metric_alone_is_answered() {
    assert_answer(&qv(), &req(&[], &["total_pages"], None), &[&["650"]]);
}

/// Q5 / Q11: credits by country, one many-to-one step from `ba`.
#[test]
fn q5_bridge_metric_by_author_dimension_is_answered() {
    assert_answer(
        &qv(),
        &req(&["country"], &["credits"], None),
        &[&["FR", "3"], &["UK", "2"]],
    );
}

/// Q6: edits by country, from the other bridge.
#[test]
fn q6_other_bridge_metric_by_author_dimension_is_answered() {
    assert_answer(&qv(), &req(&["country"], &["edits"], None), &[&["FR", "1"]]);
}

/// Q9: countries alone list the authors' own values.
#[test]
fn q9_dims_only_on_one_side_is_answered() {
    assert_answer(
        &qv(),
        &req(&["country"], &[], None),
        &[&["DE"], &["FR"], &["UK"]],
    );
}

/// Credits by role and country stay on one bridge.
#[test]
fn bridge_dimensions_with_its_own_metric_are_answered() {
    assert_answer(
        &qv(),
        &req(&["role", "country"], &["credits"], None),
        &[
            &["co", "FR", "1"],
            &["co", "UK", "1"],
            &["lead", "FR", "2"],
            &["lead", "UK", "1"],
        ],
    );
}

/// H5: review count by title (one many-to-one step).
#[test]
fn h5_review_metric_by_book_dimension_is_answered() {
    assert_answer(
        &hv(),
        &req(&["title"], &["review_count"], None),
        &[
            &["Duo", "2"],
            &["Orphan", "1"],
            &["Solo", "1"],
            &["NULL", "1"],
        ],
    );
}

/// H2: review metrics with no dimension.
/// H1, H3, H6: a fan-out after a hop up. Snowflake counts each review once
/// per author country it reaches across the bridge; we still refuse, in every
/// order, as a fan trap (TECH-DEBT #111, Phase 7 PR 4).
#[test]
fn h1_h3_h6_a_fan_out_after_a_hop_up_is_still_refused() {
    let requests = [
        req(&["country"], &["review_count", "total_stars"], None),
        req(&[], &["review_count"], Some("a.country = 'FR'")),
        req(&["stars", "country"], &[], None),
    ];
    for (order, def) in every_order(&hv()) {
        for request in &requests {
            assert!(
                expand("v", &def, request).is_err(),
                "TABLES ({order}): TECH-DEBT #111 until PR 4"
            );
        }
    }
}

#[test]
fn h2_review_metrics_alone_are_answered() {
    assert_answer(
        &hv(),
        &req(&[], &["review_count", "total_stars"], None),
        &[&["5", "15"]],
    );
}

/// S8: the last balance by account: Joint 150, Solo 40.
#[test]
fn s8_semi_additive_by_account_is_answered() {
    assert_answer(
        &bv(),
        &req(&["acct_name"], &["last_balance"], None),
        &[&["Joint", "150"], &["Solo", "40"]],
    );
}

/// S9: the last balance with no dimension: 190.
#[test]
fn s9_semi_additive_alone_is_answered() {
    assert_answer(&bv(), &req(&[], &["last_balance"], None), &[&["190"]]);
}

/// S7: by person crosses the holders bridge, which a semi-additive metric
/// cannot (Snowflake refuses it too, 000002).
#[test]
fn s7_semi_additive_by_person_is_refused() {
    for (order, def) in every_order(&bv()) {
        assert!(
            expand("v", &def, &req(&["person_name"], &["last_balance"], None)).is_err(),
            "TABLES ({order})"
        );
    }
}

/// P2: `USING (b_lead)` names the one relationship from books to authors, so
/// authors were joined bare and again as `a__b_lead`, and the unqualified
/// `country` bound to neither ("Ambiguous reference to column name").
#[test]
fn p2_using_the_only_relationship_by_an_unqualified_dimension_is_answered() {
    assert_answer(
        &pv(),
        &req(&["country"], &["pages_by_lead"], None),
        &[&["FR", "100"], &["UK", "500"], &["NULL", "50"]],
    );
}

/// The same metric by a dimension on its own table, beside one on authors.
#[test]
fn p2_using_metric_by_own_and_author_dimensions_is_answered() {
    assert_answer(
        &pv(),
        &req(&["title", "country"], &["pages_by_lead"], None),
        &[
            &["Duo", "FR", "100"],
            &["Mixed", "UK", "200"],
            &["Solo", "UK", "300"],
            &["Orphan", "NULL", "50"],
        ],
    );
}

/// P1: pages by country with no `USING` follow the one chain, the lead author.
#[test]
fn p1_book_metric_by_author_dimension_follows_the_lead_author() {
    assert_answer(
        &pv(),
        &req(&["country"], &["total_pages"], None),
        &[&["FR", "100"], &["UK", "500"], &["NULL", "50"]],
    );
}

/// P3: pages by a dimension on the bridge. Books reach `ba` by no chain, and
/// two ways: directly, and through their lead author.
#[test]
fn p3_book_metric_by_bridge_dimension_is_refused() {
    assert_many_to_many(
        &pv(),
        &req(&["role"], &["total_pages"], None),
        &["b <- ba", "b -> a <- ba"],
    );
}

/// P4: author count by title, back along the lead FK or across the bridge.
#[test]
fn p4_author_metric_by_book_dimension_is_refused() {
    assert_many_to_many(
        &pv(),
        &req(&["title"], &["author_count"], None),
        &["a <- b", "a <- ba -> b"],
    );
}

/// P5: titles beside countries with no metric follow the lead author.
#[test]
fn p5_dims_only_follow_the_lead_author() {
    assert_answer(
        &pv(),
        &req(&["title", "country"], &[], None),
        &[
            &["Duo", "FR"],
            &["Mixed", "UK"],
            &["Orphan", "NULL"],
            &["Solo", "UK"],
        ],
    );
}

/// P6: credits by country reach authors by two chains, `ba -> a` and
/// `ba -> b -> a` (Snowflake's 010246).
#[test]
fn p6_bridge_metric_by_author_dimension_is_refused_as_multi_path() {
    for (order, def) in every_order(&pv()) {
        let e = expand("v", &def, &req(&["country"], &["credits"], None))
            .expect_err("two chains to authors");
        let msg = e.to_string();
        assert!(
            msg.contains("by more than one path: ba -> a, ba -> b -> a"),
            "TABLES ({order}): {msg}"
        );
    }
}

/// P7: pages filtered on country follow the lead author: 100.
#[test]
fn p7_book_metric_filtered_on_authors_follows_the_lead_author() {
    assert_answer(
        &pv(),
        &req(&[], &["total_pages"], Some("country = 'FR'")),
        &[&["100"]],
    );
}

/// X1: customer count by store, through the orders between them or the
/// regions they share.
#[test]
fn x1_customer_metric_by_store_dimension_is_refused() {
    assert_many_to_many(
        &xv(),
        &req(&["store_name"], &["customer_count"], None),
        &["c <- o -> s", "c -> r <- s"],
    );
}

/// X2: store count by customer, the other direction.
#[test]
fn x2_store_metric_by_customer_dimension_is_refused() {
    assert_many_to_many(
        &xv(),
        &req(&["cust_name"], &["store_count"], None),
        &["s <- o -> c", "s -> r <- c"],
    );
}

/// X4: customer count filtered on a store.
#[test]
fn x4_customer_metric_filtered_on_stores_is_refused() {
    assert_many_to_many(
        &xv(),
        &req(&[], &["customer_count"], Some("store_name = 'S-US'")),
        &["c <- o -> s", "c -> r <- s"],
    );
}

/// X5: region count by customer: back along `c_region`, or round through
/// stores and orders.
#[test]
fn x5_region_metric_by_customer_dimension_is_refused() {
    assert_many_to_many(
        &xv(),
        &req(&["cust_name"], &["region_count"], None),
        &["r <- c", "r <- s <- o -> c"],
    );
}

/// X6: region count by order, back through customers or through stores.
#[test]
fn x6_region_metric_by_order_dimension_is_refused() {
    assert_many_to_many(
        &xv(),
        &req(&["order_id"], &["region_count"], None),
        &["r <- c <- o", "r <- s <- o"],
    );
}

/// X3: customers and stores with no metric list the pairs orders hold.
#[test]
fn x3_dims_only_list_the_pairs_orders_hold() {
    assert_answer(
        &xv(),
        &req(&["cust_name", "store_name"], &[], None),
        &[&["Ana", "S-EU"], &["Ana", "S-US"], &["Bo", "S-US"]],
    );
}

/// Revenue by store and by customer: one chain to each.
#[test]
fn order_metric_by_customer_and_store_is_answered() {
    assert_answer(
        &xv(),
        &req(&["cust_name", "store_name"], &["revenue"], None),
        &[
            &["Ana", "S-EU", "40"],
            &["Ana", "S-US", "10"],
            &["Bo", "S-US", "20"],
        ],
    );
}

/// `cv`: customers referenced by orders and by tickets, two different tables.
/// With `visits`, also referenced twice by `pairs` (so `x` is role-played),
/// `x` references customers too: a role-played table beside the orders'
/// path, not on it.
fn cv(with_role_played_sibling: bool) -> SemanticViewDefinition {
    let mut by_cust = metric("rev_by_cust", "SUM(o.amount)", "o");
    by_cust.using_relationships = vec!["o_c".to_string()];
    let mut def = SemanticViewDefinition {
        tables: vec![
            table("o", "s_orders", &["id"]),
            table("t", "s_tickets", &["id"]),
            table("c", "s_cust", &["id"]),
        ],
        joins: vec![
            rel("o_c", "o", "c", "c_id", "id"),
            rel("t_c", "t", "c", "c_id", "id"),
        ],
        dimensions: vec![dim("name", "c.name", "c")],
        metrics: vec![metric("rev", "SUM(o.amount)", "o"), by_cust],
        ..Default::default()
    };
    if with_role_played_sibling {
        def.tables.push(table("x", "s_visits", &["id"]));
        def.tables.push(table("p", "s_pairs", &["id"]));
        def.joins.push(rel("x_c", "x", "c", "c_id", "id"));
        def.joins.push(rel("p_x1", "p", "x", "x1", "id"));
        def.joins.push(rel("p_x2", "p", "x", "x2", "id"));
    }
    def
}

/// A `USING` naming the one relationship from the metric's table to a table
/// another table references too, by a qualified dimension on that table
/// (review of #342): the dimension was rewritten to the scoped alias
/// `c__o_c`, which was not joined ("Referenced table ... not found").
#[test]
fn using_into_a_shared_table_by_a_qualified_dimension_is_answered() {
    assert_answer(
        &cv(false),
        &req(&["name"], &["rev_by_cust"], None),
        &[&["A", "30"], &["B", "5"], &["NULL", "7"]],
    );
}

/// The same with the lead-author view's dimension written qualified.
#[test]
fn p2_using_the_only_relationship_by_a_qualified_dimension_is_answered() {
    let mut view = pv();
    view.dimensions[0].expr = "a.country".to_string();
    assert_answer(
        &view,
        &req(&["country"], &["pages_by_lead"], None),
        &[&["FR", "100"], &["UK", "500"], &["NULL", "50"]],
    );
}

/// A role-played table beside the metric's path, not on it (review of #342):
/// orders reach customers directly, and `x`, which `pairs` references twice,
/// also references customers. Refused as an ambiguous role when orders were
/// declared first, answered when customers were.
#[test]
fn a_role_played_table_off_the_metrics_path_does_not_refuse_it() {
    assert_answer(
        &cv(true),
        &req(&["name"], &["rev"], None),
        &[&["A", "30"], &["B", "5"], &["NULL", "7"]],
    );
}

/// The role-played table is on the path from its referencing table: a metric
/// on `pairs` by a customer dimension still needs a role, in every order.
#[test]
fn a_role_played_table_on_the_metrics_path_still_needs_a_role() {
    let mut view = cv(true);
    view.metrics.push(metric("pair_count", "COUNT(p.id)", "p"));
    for (order, def) in every_order(&view) {
        let e = expand("v", &def, &req(&["name"], &["pair_count"], None))
            .expect_err("x is role-played on the path from p to c");
        assert!(
            matches!(e, ExpandError::AmbiguousDescendantPath { .. }),
            "TABLES ({order}): {e}"
        );
    }
}

/// The same off-path role-played table, with customers only filtered on.
#[test]
fn a_role_played_table_off_the_metrics_path_does_not_refuse_its_filter() {
    assert_answer(
        &cv(true),
        &req(&[], &["rev"], Some("c.name = 'A'")),
        &[&["30"]],
    );
}

/// Orders reach regions by one chain, through their customer. Each order also
/// has one shipment (`s_o`, one-to-one, declared first), which references a
/// region of its own (review of #342). The join walk stepped back along the
/// one-to-one relationship before following the chain, and grouped revenue by
/// the shipment's region: R2 where the chain gives R1.
#[test]
fn a_chain_is_followed_before_a_step_back_along_a_one_to_one_relationship() {
    let mut s_o = rel("s_o", "s", "o", "o_id", "id");
    s_o.cardinality = Cardinality::OneToOne;
    let view = SemanticViewDefinition {
        tables: vec![
            table("o", "b_orders", &["id"]),
            table("c", "b_cust", &["id"]),
            table("r", "b_regions", &["id"]),
            table("s", "b_shipments", &["id"]),
        ],
        joins: vec![
            s_o,
            rel("o_c", "o", "c", "c_id", "id"),
            rel("c_r", "c", "r", "r_id", "id"),
            rel("s_r", "s", "r", "r_id", "id"),
        ],
        dimensions: vec![dim("region", "r.name", "r")],
        metrics: vec![metric("revenue", "SUM(o.amount)", "o")],
        ..Default::default()
    };
    assert_answer(
        &view,
        &req(&["region"], &["revenue"], None),
        &[&["R1", "10"]],
    );
}

/// `fv`: carriers and airports reference regions; flights reference their
/// carrier, and airports twice (`dep`, `arr`), so airports are role-played.
fn fv() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("c", "f_carriers", &["id"]),
            table("r", "f_regions", &["id"]),
            table("f", "f_flights", &["id"]),
            table("a", "f_airports", &["id"]),
        ],
        joins: vec![
            rel("c_region", "c", "r", "r_id", "id"),
            rel("a_region", "a", "r", "r_id", "id"),
            rel("f_carrier", "f", "c", "c_id", "id"),
            rel("dep", "f", "a", "dep", "id"),
            rel("arr", "f", "a", "arr", "id"),
        ],
        dimensions: vec![dim("region", "r.name", "r")],
        metrics: vec![
            metric("fees", "SUM(c.fee)", "c"),
            metric("regions", "COUNT(r.id)", "r"),
        ],
        ..Default::default()
    }
}

/// A region metric by region reads regions alone (review of #342): it was
/// refused because airports, role-played, reference regions.
#[test]
fn a_metric_on_the_dimensions_own_table_is_not_refused_by_a_role_played_referencer() {
    assert_answer(
        &fv(),
        &req(&["region"], &["regions"], None),
        &[&["R1", "1"], &["R2", "1"]],
    );
}

/// The same in a tree: flights reference airports twice, airports
/// reference their country. A country metric by country reads countries
/// alone; it was answered only with countries declared first.
#[test]
fn a_metric_on_a_table_past_a_role_played_one_by_its_own_dimension_is_answered() {
    assert_answer(
        &kv(),
        &req(&["country"], &["countries"], None),
        &[&["FR", "1"], &["UK", "1"]],
    );
}

/// The same country metric by a flight dimension, with one-to-one
/// relationships, reaches flights from countries through airports, which
/// flights reference twice: which airport's country is a flight's? Refused in
/// every order. With countries declared first it joined the departure
/// airport, the first-declared relationship, silently.
#[test]
fn a_metric_on_a_table_past_a_role_played_one_by_a_dimension_beyond_it_is_refused() {
    // One-to-one, so no fan trap stands in front of the role check.
    let mut view = kv();
    for j in &mut view.joins {
        j.cardinality = Cardinality::OneToOne;
    }
    for (order, def) in every_order(&view) {
        let e = expand("v", &def, &req(&["flight"], &["countries"], None))
            .expect_err("flights reach countries through dep or arr");
        assert!(
            matches!(e, ExpandError::AmbiguousMetricPath { .. }),
            "TABLES ({order}): {e}"
        );
    }
}

/// `kv`: flights reference airports twice (`dep`, `arr`), airports
/// reference their country: a tree.
fn kv() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("f", "k_flights", &["id"]),
            table("a", "k_airports", &["id"]),
            table("k", "k_country", &["id"]),
        ],
        joins: vec![
            rel("dep", "f", "a", "dep", "id"),
            rel("arr", "f", "a", "arr", "id"),
            rel("a_k", "a", "k", "k_id", "id"),
        ],
        dimensions: vec![dim("country", "k.name", "k"), dim("flight", "f.id", "f")],
        metrics: vec![metric("countries", "COUNT(k.id)", "k")],
        ..Default::default()
    }
}

/// A metric on `pairs` naming a role (`USING (p_x1)`) by a customer
/// dimension follows that role (review of #342): customers are referenced by
/// orders and tickets too, so the walk from the role down to customers found
/// three relationships into them and refused the query. Pair 1's first visit
/// is customer B's.
#[test]
fn a_role_named_by_using_is_followed_past_to_a_shared_table() {
    let mut view = cv(true);
    let mut by_x1 = metric("pairs_by_first", "COUNT(p.id)", "p");
    by_x1.using_relationships = vec!["p_x1".to_string()];
    view.metrics.push(by_x1);
    assert_answer(
        &view,
        &req(&["name"], &["pairs_by_first"], None),
        &[&["B", "1"]],
    );
}

/// Carriers reference their region, and so do airports, which flights
/// reference twice (so airports are role-played); flights also reference
/// their carrier (review of #342). A carrier metric by region follows the one
/// chain `c -> r`, which no role-played table is on: the airports are reached
/// from carriers only by stepping back along `f -> c`. It was refused as an
/// ambiguous role in every order.
#[test]
fn a_role_played_table_off_the_chain_does_not_refuse_it() {
    assert_answer(
        &fv(),
        &req(&["region"], &["fees"], None),
        &[&["R1", "10"], &["R2", "20"]],
    );
}

/// Orders and stores both reference customers. A semi-additive order metric
/// whose `USING` names the orders' relationship takes its snapshot at the
/// latest signup of the orders' customers (review of #342): the snapshot
/// ordered by `c__o_cust.signup`, an alias no join brought in, when orders
/// were declared first.
#[test]
fn semi_additive_using_into_a_shared_table_is_answered() {
    let mut last = metric("last_amt", "SUM(o.amount)", "o");
    last.non_additive_by = vec![NonAdditiveDim {
        dimension: "signup".to_string(),
        ..Default::default()
    }];
    last.using_relationships = vec!["o_cust".to_string()];
    let view = SemanticViewDefinition {
        tables: vec![
            table("o", "n_orders", &["id"]),
            table("s", "n_stores", &["id"]),
            table("c", "n_cust", &["id"]),
        ],
        joins: vec![
            rel("o_cust", "o", "c", "c_id", "id"),
            rel("s_cust", "s", "c", "c_id", "id"),
        ],
        dimensions: vec![dim("signup", "c.signup", "c")],
        metrics: vec![last],
        ..Default::default()
    };
    assert_answer(&view, &req(&[], &["last_amt"], None), &[&["25"]]);
}

/// Orders reference their customer one-to-one and their store; both
/// reference a region (review of #342). A customer metric by region follows
/// `c -> r`, whichever relationship is declared first: the join tree from
/// orders reached the region through the store.
#[test]
fn a_metric_follows_its_own_chain_when_the_root_reaches_the_table_twice() {
    let mut o_cust = rel("o_cust", "o", "c", "c_id", "id");
    o_cust.cardinality = Cardinality::OneToOne;
    let view = SemanticViewDefinition {
        tables: vec![
            table("o", "g_orders", &["id"]),
            table("c", "g_cust", &["id"]),
            table("s", "g_stores", &["id"]),
            table("r", "g_regions", &["id"]),
        ],
        joins: vec![
            rel("o_store", "o", "s", "s_id", "id"),
            o_cust,
            rel("c_region", "c", "r", "r_id", "id"),
            rel("s_region", "s", "r", "r_id", "id"),
        ],
        dimensions: vec![dim("region", "r.name", "r")],
        metrics: vec![metric("cust_v", "SUM(c.v)", "c")],
        ..Default::default()
    };
    assert_answer(
        &view,
        &req(&["region"], &["cust_v"], None),
        &[&["R1", "10"]],
    );
}

/// `m` reaches `r` by the chain `m -> x -> r`; `a` references both (review of
/// #342). An `m` metric by an `r` dimension follows the chain in every order.
/// The review read the join tree's bridge round as reaching `m` through `a`
/// first; the chain round places `m` before it, so this was green before any
/// change and stays as a pin.
#[test]
fn a_chain_to_the_root_is_followed_before_a_bridge() {
    let view = SemanticViewDefinition {
        tables: vec![
            table("r", "m_root", &["id"]),
            table("x", "m_x", &["id"]),
            table("a", "m_a", &["id"]),
            table("m", "m_m", &["id"]),
        ],
        joins: vec![
            rel("x_r", "x", "r", "r_id", "id"),
            rel("m_x", "m", "x", "x_id", "id"),
            rel("a_r", "a", "r", "r_id", "id"),
            rel("a_m", "a", "m", "m_id", "id"),
        ],
        dimensions: vec![dim("root", "r.name", "r")],
        metrics: vec![metric("ms", "COUNT(m.id)", "m")],
        ..Default::default()
    };
    assert_answer(&view, &req(&["root"], &["ms"], None), &[&["R1", "2"]]);
}
