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
//! - the two sides of a bridge list the bridge's rows;
//! - members no single table reaches (two children of one parent) are refused
//!   as unrelated;
//! - a role-played table queried from itself or from above is answered, and
//!   refused only when the query has to cross one of its relationships;
//! - a FACTS query reads the same table's rows, every one of them (the second
//!   fixture, 2026-11-09).
//!
//! Each test asks its question of every table order and expects Snowflake's
//! answer from each. They pin ROWS: the generated SQL runs against an
//! in-memory DuckDB, one test per case so each reports on its own.

use super::*;
use crate::model::{Cardinality, Dimension, Fact, Join, Metric, SemanticViewDefinition, TableRef};

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
                                   (4, NULL, 'lost'), (5, 9, 'stray');
         CREATE TABLE wcustomers (cust_id INT, cust_name VARCHAR);
         CREATE TABLE worders (order_id INT, cust_id INT, status VARCHAR);
         CREATE TABLE tickets (ticket_id INT, cust_id INT, topic VARCHAR);
         INSERT INTO wcustomers VALUES (1, 'Ana'), (2, 'Ben'), (3, 'Cleo'), (4, 'Dee');
         INSERT INTO worders VALUES (1, 1, 'open'), (2, 1, 'shipped'), (3, 2, 'open'),
                                    (4, NULL, 'lost');
         INSERT INTO tickets VALUES (1, 1, 'billing'), (2, 1, 'login'), (3, 3, 'billing'),
                                    (4, NULL, 'spam');
         CREATE TABLE regions (region_id INT, region VARCHAR);
         CREATE TABLE airports (code VARCHAR, city VARCHAR, region_id INT);
         CREATE TABLE carriers (carrier_id INT, carrier VARCHAR);
         CREATE TABLE flights (flight_no VARCHAR, origin VARCHAR, dest VARCHAR, carrier_id INT);
         INSERT INTO regions VALUES (1, 'NA'), (2, 'EU'), (3, 'APAC');
         INSERT INTO airports VALUES ('JFK', 'NYC', 1), ('LHR', 'London', 2), ('CDG', 'Paris', 2),
                                     ('SFO', 'SF', 1);
         INSERT INTO carriers VALUES (1, 'Acme'), (2, 'Zed');
         INSERT INTO flights VALUES ('F1', 'JFK', 'LHR', 1), ('F2', 'LHR', 'CDG', 1),
                                    ('F3', 'CDG', NULL, 2), ('F4', 'JFK', 'XXX', NULL);
         CREATE TABLE fcustomers (cust_id INT, cust_name VARCHAR, credit INT);
         CREATE TABLE forders (order_id INT, cust_id INT, status VARCHAR, amount INT);
         INSERT INTO fcustomers VALUES (1, 'Ana', 100), (2, 'Ben', 100), (3, 'Cleo', 50);
         INSERT INTO forders VALUES (1, 1, 'open', 10), (2, 1, 'shipped', 10), (3, 2, 'open', 20),
                                    (4, NULL, 'lost', 5), (5, 9, 'stray', 7);
         CREATE TABLE reviews (review_id INT, book_id INT, stars INT);
         INSERT INTO reviews VALUES (1, 10, 5), (2, 10, 3), (3, 12, 4), (4, 13, 2),
                                    (5, NULL, 1), (6, 11, 4);
         CREATE TABLE nregions (region_id INT, region VARCHAR);
         CREATE TABLE norders (order_id INT, region_id INT);
         CREATE TABLE nitems (item_id INT, order_id INT, qty INT);
         CREATE TABLE nnotes (note_id INT, order_id INT, kind VARCHAR);
         INSERT INTO nregions VALUES (1, 'EU');
         INSERT INTO norders VALUES (1, 1), (2, NULL), (3, 1);
         INSERT INTO nitems VALUES (1, 1, 5), (2, 1, 1), (3, 2, 2), (4, 3, 7);
         INSERT INTO nnotes VALUES (1, 1, 'gift'), (2, 1, 'rush'), (3, 2, 'gift');",
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

/// The second fixture's `wv_*`: two children of one parent, `base` first.
fn children_def(base: &str) -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: first(
            vec![
                table("cu", "wcustomers", &["cust_id"]),
                table("o", "worders", &["order_id"]),
                table("t", "tickets", &["ticket_id"]),
            ],
            base,
        ),
        dimensions: vec![
            dim("cust_name", "cu.cust_name", "cu"),
            dim("status", "o.status", "o"),
            dim("topic", "t.topic", "t"),
        ],
        metrics: vec![Metric {
            name: "order_count".to_string(),
            expr: "COUNT(o.order_id)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        joins: vec![
            rel("o_cust", "o", "cu", "cust_id", "cust_id"),
            rel("t_cust", "t", "cu", "cust_id", "cust_id"),
        ],
        ..Default::default()
    }
}

/// The second fixture's `rv_*`: flights reference airports twice, `base` first.
fn role_played_def(base: &str) -> SemanticViewDefinition {
    let count = |name: &str, role: &str| Metric {
        name: name.to_string(),
        expr: "COUNT(f.flight_no)".to_string(),
        source_table: Some("f".to_string()),
        using_relationships: vec![role.to_string()],
        ..Default::default()
    };
    SemanticViewDefinition {
        tables: first(
            vec![
                table("f", "flights", &["flight_no"]),
                table("ap", "airports", &["code"]),
                table("rg", "regions", &["region_id"]),
                table("ca", "carriers", &["carrier_id"]),
            ],
            base,
        ),
        dimensions: vec![
            dim("flight_no", "f.flight_no", "f"),
            dim("city", "ap.city", "ap"),
            dim("region", "rg.region", "rg"),
            dim("carrier", "ca.carrier", "ca"),
        ],
        metrics: vec![count("departures", "f_origin"), count("arrivals", "f_dest")],
        joins: vec![
            rel("f_origin", "f", "ap", "origin", "code"),
            rel("f_dest", "f", "ap", "dest", "code"),
            rel("ap_region", "ap", "rg", "region_id", "region_id"),
            rel("f_carrier", "f", "ca", "carrier_id", "carrier_id"),
        ],
        ..Default::default()
    }
}

/// The second fixture's `fv_*`: a fact on each of a parent and a child.
fn facts_def(base: &str) -> SemanticViewDefinition {
    let fact = |name: &str, table: &str| Fact {
        name: name.to_string(),
        expr: format!("{table}.{name}"),
        source_table: Some(table.to_string()),
        ..Default::default()
    };
    SemanticViewDefinition {
        tables: first(
            vec![
                table("cu", "fcustomers", &["cust_id"]),
                table("o", "forders", &["order_id"]),
            ],
            base,
        ),
        dimensions: vec![
            dim("cust_name", "cu.cust_name", "cu"),
            dim("status", "o.status", "o"),
        ],
        facts: vec![fact("amount", "o"), fact("credit", "cu")],
        metrics: vec![Metric {
            name: "total".to_string(),
            expr: "SUM(o.amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        joins: vec![rel("o_cust", "o", "cu", "cust_id", "cust_id")],
        ..Default::default()
    }
}

/// Every row of the query, each column as text (`NULL` for a NULL), sorted;
/// or the error it is refused with.
fn try_run(def: &SemanticViewDefinition, req: &QueryRequest) -> Result<Vec<String>, ExpandError> {
    let sql = expand("v", def, req)?;
    let con = db();
    let mut stmt = con
        .prepare(&sql)
        .unwrap_or_else(|e| panic!("emitted SQL must bind, got: {e}\nSQL:\n{sql}"));
    let mut out: Vec<String> = stmt
        .query_map([], |r| {
            let n = r.as_ref().column_count();
            (0..n)
                .map(|i| {
                    Ok(match r.get::<_, duckdb::types::Value>(i)? {
                        duckdb::types::Value::Null => "NULL".to_string(),
                        duckdb::types::Value::Text(t) => t,
                        duckdb::types::Value::Int(n) => n.to_string(),
                        other => format!("{other:?}"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|cols| cols.join("/"))
        })
        .expect("query runs")
        .map(Result::unwrap)
        .collect();
    out.sort();
    Ok(out)
}

fn dims_req(dims: &[&str], where_clause: Option<&str>) -> QueryRequest {
    QueryRequest {
        where_clause: where_clause.map(str::to_string),
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: vec![],
    }
}

fn facts_req(facts: &[&str], where_clause: Option<&str>) -> QueryRequest {
    QueryRequest {
        where_clause: where_clause.map(str::to_string),
        facts: facts.iter().map(|f| FactName::new(*f)).collect(),
        dimensions: vec![],
        metrics: vec![],
    }
}

/// Asks `make(base)` for every `base` in `bases`, and expects `want` each time.
fn case_in_every_order(
    make: fn(&str) -> SemanticViewDefinition,
    bases: &[&str],
    req: &QueryRequest,
    want: &[&str],
) {
    for base in bases {
        assert_eq!(
            try_run(&make(base), req).unwrap_or_else(|e| panic!("{base} first: {e}")),
            sorted(want),
            "{req:?}, {base} declared first"
        );
    }
}

/// Asks `make(base)` for every `base` in `bases`, and expects a refusal each
/// time that `is_expected` accepts.
fn refused_in_every_order(
    make: fn(&str) -> SemanticViewDefinition,
    bases: &[&str],
    req: &QueryRequest,
    is_expected: fn(&ExpandError) -> bool,
) {
    for base in bases {
        match try_run(&make(base), req) {
            Err(e) if is_expected(&e) => {}
            other => panic!("{req:?}, {base} declared first: expected a refusal, got {other:?}"),
        }
    }
}

fn is_unrelated(e: &ExpandError) -> bool {
    matches!(e, ExpandError::UnrelatedTables { .. })
}

fn is_ambiguous(e: &ExpandError) -> bool {
    matches!(
        e,
        ExpandError::AmbiguousPath { .. } | ExpandError::AmbiguousDescendantPath { .. }
    )
}

/// Every row of the dims-only query (see [`try_run`]).
fn run(def: &SemanticViewDefinition, dims: &[&str], where_clause: Option<&str>) -> Vec<String> {
    try_run(def, &dims_req(dims, where_clause)).unwrap_or_else(|e| panic!("{e}"))
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

/// W1: dimensions on two children of one parent. Snowflake: "The entities 'T'
/// and 'O' are not related".
#[test]
fn w1_two_children_are_unrelated() {
    refused_in_every_order(
        children_def,
        &["cu", "o", "t"],
        &dims_req(&["status", "topic"], None),
        is_unrelated,
    );
}

/// W2: the parent beside both children does not relate them.
#[test]
fn w2_two_children_and_their_parent_are_unrelated() {
    refused_in_every_order(
        children_def,
        &["cu", "o", "t"],
        &dims_req(&["cust_name", "status", "topic"], None),
        is_unrelated,
    );
}

/// W3: one child filtered by the other.
#[test]
fn w3_child_filtered_by_its_sibling_is_unrelated() {
    refused_in_every_order(
        children_def,
        &["cu", "o", "t"],
        &dims_req(&["status"], Some("t.topic = 'login'")),
        is_unrelated,
    );
}

/// W4: the parent filtered by both children.
#[test]
fn w4_parent_filtered_by_both_children_is_unrelated() {
    refused_in_every_order(
        children_def,
        &["cu", "o", "t"],
        &dims_req(
            &["cust_name"],
            Some("o.status = 'open' AND t.topic = 'billing'"),
        ),
        is_unrelated,
    );
}

/// W5: one child and the parent list the child's rows, the other child aside.
#[test]
fn w5_one_child_and_the_parent() {
    case_in_every_order(
        children_def,
        &["cu", "o", "t"],
        &dims_req(&["cust_name", "status"], None),
        &["Ana/open", "Ana/shipped", "Ben/open", "NULL/lost"],
    );
}

/// R1: a role-played table alone lists its own rows, SF (no flights) too.
#[test]
fn r1_role_played_table_alone() {
    case_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["city"], None),
        &["London", "NYC", "Paris", "SF"],
    );
}

/// R2: a table past the role-played one, alone. APAC (no airport) too.
#[test]
fn r2_past_the_role_played_table_alone() {
    case_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["region"], None),
        &["APAC", "EU", "NA"],
    );
}

/// R3: the role-played table and its parent list the airports' rows.
#[test]
fn r3_role_played_table_and_its_parent() {
    case_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["city", "region"], None),
        &["London/EU", "NYC/NA", "Paris/EU", "SF/NA"],
    );
}

/// R4: the referencing table alone.
#[test]
fn r4_referencing_table_alone() {
    case_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["flight_no"], None),
        &["F1", "F2", "F3", "F4"],
    );
}

/// R5: across the two relationships. Snowflake: "Multi-path relationship ...
/// is not supported".
#[test]
fn r5_across_the_role_played_relationships_is_ambiguous() {
    refused_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["flight_no", "city"], None),
        is_ambiguous,
    );
}

/// R6: a hop past the role-played table, from below it.
#[test]
fn r6_past_the_role_played_table_from_below_is_ambiguous() {
    refused_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["flight_no", "region"], None),
        is_ambiguous,
    );
}

/// R7: the role-played table filtered from below.
#[test]
fn r7_role_played_table_filtered_from_below_is_ambiguous() {
    refused_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["city"], Some("f.flight_no = 'F1'")),
        is_ambiguous,
    );
}

/// R8: a single relationship from the role-playing table is answered.
#[test]
fn r8_single_relationship_beside_a_role_played_one() {
    case_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["flight_no", "carrier"], None),
        &["F1/Acme", "F2/Acme", "F3/Zed", "F4/NULL"],
    );
}

/// R9: the carrier alone.
#[test]
fn r9_other_parent_alone() {
    case_in_every_order(
        role_played_def,
        &["f", "ap"],
        &dims_req(&["carrier"], None),
        &["Acme", "Zed"],
    );
}

/// F1: a child fact alone lists every order's amount, 10 twice.
#[test]
fn f1_child_fact_alone() {
    case_in_every_order(
        facts_def,
        &["cu", "o"],
        &facts_req(&["amount"], None),
        &["10", "10", "20", "5", "7"],
    );
}

/// F2: a parent fact alone lists every customer's credit, Cleo's too.
#[test]
fn f2_parent_fact_alone() {
    case_in_every_order(
        facts_def,
        &["cu", "o"],
        &facts_req(&["credit"], None),
        &["100", "100", "50"],
    );
}

/// F6: facts from both tables list the orders' rows, a missing customer's
/// credit as NULL.
#[test]
fn f6_facts_from_child_and_parent() {
    case_in_every_order(
        facts_def,
        &["cu", "o"],
        &facts_req(&["amount", "credit"], None),
        &["10/100", "10/100", "20/100", "5/NULL", "7/NULL"],
    );
}

/// F7: a child fact filtered by the parent.
#[test]
fn f7_child_fact_filtered_by_parent() {
    case_in_every_order(
        facts_def,
        &["cu", "o"],
        &facts_req(&["amount"], Some("cu.cust_name = 'Ana'")),
        &["10", "10"],
    );
}

/// F8: a parent fact filtered by the child, once per matching order.
#[test]
fn f8_parent_fact_filtered_by_child() {
    case_in_every_order(
        facts_def,
        &["cu", "o"],
        &facts_req(&["credit"], Some("o.status = 'open'")),
        &["100", "100"],
    );
}

// ---------------------------------------------------------------------------
// Phase 7 PR 4d: members no table reaches by foreign keys, whose paths meet at
// a table with a metric (fixture 7's D1-D10 and N8). Snowflake lists the
// distinct member tuples that table reaches, with none padded with NULLs.
// ---------------------------------------------------------------------------

fn metric(name: &str, expr: &str, table: &str) -> Metric {
    Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(table.to_string()),
        ..Default::default()
    }
}

/// Fixture 7's `d*` views (`reviews -> books <- book_authors -> authors`),
/// with a metric on the reviews and, for `books_metric`, one on the books (D4,
/// D6) or not (D1, D8). The bridge row `(12, 99)` names no author, so a LEFT
/// JOIN would list Solo's review under a NULL country.
fn reviews_def(base: &str, books_metric: bool) -> SemanticViewDefinition {
    let mut metrics = vec![metric("review_count", "COUNT(rv.review_id)", "rv")];
    if books_metric {
        metrics.push(metric("total_pages", "SUM(b.pages)", "b"));
    }
    SemanticViewDefinition {
        tables: first(
            vec![
                table("a", "authors", &["author_id"]),
                table("b", "books", &["book_id"]),
                table("ba", "book_authors", &["book_id", "author_id"]),
                table("rv", "reviews", &["review_id"]),
            ],
            base,
        ),
        dimensions: vec![
            dim("country", "a.country", "a"),
            dim("name", "a.name", "a"),
            dim("title", "b.title", "b"),
            dim("stars", "rv.stars", "rv"),
        ],
        metrics,
        joins: vec![
            rel("ba_author", "ba", "a", "author_id", "author_id"),
            rel("ba_book", "ba", "b", "book_id", "book_id"),
            rel("rv_book", "rv", "b", "book_id", "book_id"),
        ],
        ..Default::default()
    }
}

fn reviews_with_books_metric(base: &str) -> SemanticViewDefinition {
    reviews_def(base, true)
}

fn reviews_without_books_metric(base: &str) -> SemanticViewDefinition {
    reviews_def(base, false)
}

const REVIEW_TABLES: [&str; 4] = ["a", "b", "ba", "rv"];

/// D4 / D6: stars with country meet at books, which has a metric. Review 4's
/// book has no author and review 5 no book, so neither is listed, and the
/// ghost bridge row adds no NULL country.
#[test]
fn d4_reviews_and_authors_meet_at_books_with_a_metric() {
    case_in_every_order(
        reviews_with_books_metric,
        &REVIEW_TABLES,
        &dims_req(&["stars", "country"], None),
        &["3/FR", "4/FR", "4/UK", "5/FR"],
    );
}

/// D1 / D7 / D8: the same query with no metric on books is refused, whatever
/// metric the reviews have.
#[test]
fn d8_reviews_and_authors_with_no_books_metric_are_unrelated() {
    refused_in_every_order(
        reviews_without_books_metric,
        &REVIEW_TABLES,
        &dims_req(&["stars", "country"], None),
        is_unrelated,
    );
}

/// D10: stars filtered on the authors' country: Duo's 3 and 5, Mixed's 4.
#[test]
fn d10_reviews_filtered_by_authors_meet_at_books() {
    case_in_every_order(
        reviews_with_books_metric,
        &REVIEW_TABLES,
        &dims_req(&["stars"], Some("a.country = 'FR'")),
        &["3", "4", "5"],
    );
}

/// A member on the meeting table itself is listed beside the rest.
#[test]
fn meeting_tables_own_member_is_listed_beside_the_others() {
    case_in_every_order(
        reviews_with_books_metric,
        &REVIEW_TABLES,
        &dims_req(&["title", "stars", "name"], None),
        &[
            "Duo/3/Ana",
            "Duo/3/Ben",
            "Duo/5/Ana",
            "Duo/5/Ben",
            "Mixed/4/Ana",
            "Mixed/4/Cleo",
            "Solo/4/Cleo",
        ],
    );
}

/// Fixture 7's `nv` in small: items and notes are children of orders, and
/// orders reference regions. `metric_on` names the tables with a metric.
fn notes_def(base: &str, metric_on: &[&str]) -> SemanticViewDefinition {
    let metrics = [
        ("order_count", "COUNT(o.order_id)", "o"),
        ("item_count", "COUNT(li.item_id)", "li"),
        ("note_count", "COUNT(n.note_id)", "n"),
        ("region_count", "COUNT(r.region_id)", "r"),
    ]
    .iter()
    .filter(|(_, _, t)| metric_on.contains(t))
    .map(|(name, expr, t)| metric(name, expr, t))
    .collect();
    SemanticViewDefinition {
        tables: first(
            vec![
                table("r", "nregions", &["region_id"]),
                table("o", "norders", &["order_id"]),
                table("li", "nitems", &["item_id"]),
                table("n", "nnotes", &["note_id"]),
            ],
            base,
        ),
        dimensions: vec![
            dim("region", "r.region", "r"),
            dim("qty", "li.qty", "li"),
            dim("kind", "n.kind", "n"),
        ],
        metrics,
        joins: vec![
            rel("o_region", "o", "r", "region_id", "region_id"),
            rel("li_order", "li", "o", "order_id", "order_id"),
            rel("n_order", "n", "o", "order_id", "order_id"),
        ],
        ..Default::default()
    }
}

fn notes_with_every_metric(base: &str) -> SemanticViewDefinition {
    notes_def(base, &["o", "li", "n", "r"])
}

fn notes_without_orders_metric(base: &str) -> SemanticViewDefinition {
    notes_def(base, &["li", "n", "r"])
}

const NOTE_TABLES: [&str; 4] = ["r", "o", "li", "n"];

/// N8: an item's qty with a note's kind meet at orders, which has a metric.
/// Order 3 (qty 7) has no notes, so no `7/NULL`.
#[test]
fn n8_items_and_notes_meet_at_orders_with_a_metric() {
    case_in_every_order(
        notes_with_every_metric,
        &NOTE_TABLES,
        &dims_req(&["qty", "kind"], None),
        &["1/gift", "1/rush", "2/gift", "5/gift", "5/rush"],
    );
}

/// With no metric on orders the same query is refused, though items and notes
/// have metrics: they are not where the paths meet (fixture 2's W1).
#[test]
fn n8_items_and_notes_with_no_orders_metric_are_unrelated() {
    refused_in_every_order(
        notes_without_orders_metric,
        &NOTE_TABLES,
        &dims_req(&["qty", "kind"], None),
        is_unrelated,
    );
}

/// A member above the meeting table is joined like the rest: order 2 has no
/// region, so its `2/gift` is not listed. Snowflake has not been asked about
/// this shape (TECH-DEBT #112).
#[test]
fn member_above_the_meeting_table_is_inner_joined() {
    case_in_every_order(
        notes_with_every_metric,
        &NOTE_TABLES,
        &dims_req(&["qty", "kind", "region"], None),
        &["1/gift/EU", "1/rush/EU", "5/gift/EU", "5/rush/EU"],
    );
}

/// The meeting table is the lowest one: regions also reach every member by
/// fanning out, but the paths meet at orders, and with no metric there the
/// query is refused (TECH-DEBT #112).
#[test]
fn meeting_table_is_the_lowest_one() {
    refused_in_every_order(
        notes_without_orders_metric,
        &NOTE_TABLES,
        &dims_req(&["qty", "kind", "region"], None),
        is_unrelated,
    );
}

/// Two places where paths meet (`li -> o <- sh -> c <- tr`): from either, a
/// member is reached by climbing and then fanning out again, so neither is the
/// meeting table and the query is refused, though every table has a metric.
#[test]
fn members_whose_paths_meet_twice_are_unrelated() {
    let def = SemanticViewDefinition {
        tables: vec![
            table("o", "norders", &["order_id"]),
            table("li", "nitems", &["item_id"]),
            table("sh", "nships", &["ship_id"]),
            table("c", "ncarriers", &["carrier_id"]),
            table("tr", "ntrucks", &["truck_id"]),
        ],
        dimensions: vec![dim("qty", "li.qty", "li"), dim("plate", "tr.plate", "tr")],
        metrics: vec![
            metric("order_count", "COUNT(o.order_id)", "o"),
            metric("item_count", "COUNT(li.item_id)", "li"),
            metric("ship_count", "COUNT(sh.ship_id)", "sh"),
            metric("carrier_count", "COUNT(c.carrier_id)", "c"),
            metric("truck_count", "COUNT(tr.truck_id)", "tr"),
        ],
        joins: vec![
            rel("li_order", "li", "o", "order_id", "order_id"),
            rel("sh_order", "sh", "o", "order_id", "order_id"),
            rel("sh_carrier", "sh", "c", "carrier_id", "carrier_id"),
            rel("tr_carrier", "tr", "c", "carrier_id", "carrier_id"),
        ],
        ..Default::default()
    };
    match expand("v", &def, &dims_req(&["qty", "plate"], None)) {
        Err(e) if is_unrelated(&e) => {}
        other => panic!("expected not related, got {other:?}"),
    }
}

/// A fan-out across an ASOF relationship walked backwards is not answered with
/// metrics (fixture 1's A5), so it does not meet the members either. Snowflake
/// has not been asked this dims-only shape (TECH-DEBT #112).
#[test]
fn members_meeting_across_an_asof_relationship_are_unrelated() {
    let mut def = notes_with_every_metric("o");
    let n_order = def
        .joins
        .iter_mut()
        .find(|j| j.name.as_deref() == Some("n_order"))
        .expect("n_order");
    n_order.fk_columns.push("noted_at".to_string());
    n_order.ref_columns.push("placed_at".to_string());
    n_order.asof = Some(1);
    match expand("v", &def, &dims_req(&["qty", "kind"], None)) {
        Err(e) if is_unrelated(&e) => {}
        other => panic!("expected not related, got {other:?}"),
    }
}

/// A range relationship is kept out the same way: a fan-out across one walked
/// backwards is refused with metrics too. Unprobed in Snowflake (TECH-DEBT
/// #112).
#[test]
fn members_meeting_across_a_range_relationship_are_unrelated() {
    let mut def = notes_with_every_metric("o");
    let n_order = def
        .joins
        .iter_mut()
        .find(|j| j.name.as_deref() == Some("n_order"))
        .expect("n_order");
    n_order.fk_columns = vec!["noted_at".to_string()];
    n_order.ref_columns = vec!["opened_at".to_string()];
    n_order.range_end = Some("closed_at".to_string());
    match expand("v", &def, &dims_req(&["qty", "kind"], None)) {
        Err(e) if is_unrelated(&e) => {}
        other => panic!("expected not related, got {other:?}"),
    }
}

/// Notes that reference orders twice (a second role) give the hop two
/// relationships and no one path to follow, so the members do not meet.
/// Unprobed in Snowflake (TECH-DEBT #112).
#[test]
fn members_meeting_across_a_role_played_hop_are_unrelated() {
    let mut def = notes_with_every_metric("o");
    def.joins
        .push(rel("n_order_again", "n", "o", "order_id2", "order_id"));
    match expand("v", &def, &dims_req(&["qty", "kind"], None)) {
        Err(e) if is_unrelated(&e) => {}
        other => panic!("expected not related, got {other:?}"),
    }
}

/// The emitted query inner-joins every hop from the meeting table.
#[test]
fn meeting_table_query_inner_joins_every_hop() {
    let sql = expand(
        "v",
        &notes_with_every_metric("r"),
        &dims_req(&["qty", "kind"], None),
    )
    .expect("answered");
    assert!(sql.contains("FROM \"norders\""), "{sql}");
    assert!(!sql.contains("LEFT JOIN"), "{sql}");
    assert_eq!(sql.matches("\nJOIN ").count(), 2, "{sql}");
}

/// A materialization's rows were grouped by whatever built it, and nothing ties
/// them to the tuples the meeting table reaches (a grouped metric keeps an
/// order with no notes under a `NULL` kind), so it does not answer a
/// meeting-table query.
#[test]
fn meeting_table_query_is_not_routed_to_a_materialization() {
    let mut def = notes_with_every_metric("o");
    def.materializations = vec![crate::model::Materialization {
        name: "by_qty_kind".to_string(),
        table: "nmat".to_string(),
        dimensions: vec!["qty".to_string(), "kind".to_string()],
        metrics: vec![],
    }];
    let sql = expand("v", &def, &dims_req(&["qty", "kind"], None)).expect("answered");
    assert!(!sql.contains("nmat"), "{sql}");
}
