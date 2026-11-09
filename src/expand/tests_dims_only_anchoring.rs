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
                                    (4, NULL, 'lost', 5), (5, 9, 'stray', 7);",
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
