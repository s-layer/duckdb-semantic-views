//! Tables that reach a role-played table without crossing its roles, and one
//! filter beside metrics on two roles (Snowflake fixtures 9 and 9b,
//! 2026-11-12; TECH-DEBT #111).
//!
//! Flights reference airports twice (`dep_airport`, `arr_airport`). Details,
//! gates and lounges each reference airports once, details reference
//! terminals, and airports reference regions. Snowflake asks a role only of a
//! metric whose own path to the airports crosses the flights' two
//! relationships:
//!
//! - one filter beside metrics on two roles is read along each metric's role
//!   (W3, W4, W7, W9, W10, R4);
//! - lounges reach the airports by their one relationship, so a lounge count
//!   by city, or filtered on it, names no role (L1, L3, L4, W11);
//! - regions sit above the airports, so a region count by city, gate or
//!   terminal names no role either, and counts each region once per group
//!   (P1, P2, P3, P5, P6).
//!
//! What Snowflake refuses stays refused: a filter beside a metric naming no
//! role (W6, W8), and a role followed by a fan-out (G1-G3).
//!
//! Each case asks several `TABLES` orders and expects Snowflake's rows from
//! each. The expressions are qualified; the fixture as Snowflake wrote it,
//! with bare columns, is `test/sql/phase7_bare_columns.test` (TECH-DEBT #83).

use super::*;
use crate::model::{Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef};

/// Fixture 9's data: flights 1 CDG -> LHR, 2 LHR -> CDG, 3 CDG -> JFK;
/// details CDG T2, LHR T5, JFK T4; gates CDG G1, CDG G2, LHR G3; lounges 1
/// CDG, 2 CDG, 3 JFK; CDG and LHR in EU, JFK in NA.
fn db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE airports (code VARCHAR, city VARCHAR, region_id INT);
         CREATE TABLE regions (region_id INT, region VARCHAR);
         CREATE TABLE flights (flight_id INT, dep VARCHAR, arr VARCHAR);
         CREATE TABLE airport_details (code VARCHAR, terminal VARCHAR);
         CREATE TABLE gates (gate_id INT, code VARCHAR, gate VARCHAR);
         CREATE TABLE terminals (terminal VARCHAR, operator VARCHAR);
         CREATE TABLE lounges (lounge_id INT, code VARCHAR);
         INSERT INTO airports VALUES ('CDG', 'Paris', 1), ('LHR', 'London', 1), ('JFK', 'New York', 2);
         INSERT INTO regions VALUES (1, 'EU'), (2, 'NA');
         INSERT INTO flights VALUES (1, 'CDG', 'LHR'), (2, 'LHR', 'CDG'), (3, 'CDG', 'JFK');
         INSERT INTO airport_details VALUES ('CDG', 'T2'), ('LHR', 'T5'), ('JFK', 'T4');
         INSERT INTO gates VALUES (1, 'CDG', 'G1'), (2, 'CDG', 'G2'), (3, 'LHR', 'G3');
         INSERT INTO terminals VALUES ('T2', 'ADP'), ('T5', 'HAL'), ('T4', 'PANYNJ');
         INSERT INTO lounges VALUES (1, 'CDG'), (2, 'CDG'), (3, 'JFK');",
    )
    .expect("fixture data");
    con
}

fn table(alias: &str, name: &str, pk: &str) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: name.to_string(),
        pk_columns: vec![pk.to_string()],
        ..Default::default()
    }
}

fn rel(name: &str, from: &str, fk: &str, to: &str, pk: &str, card: Cardinality) -> Join {
    Join {
        table: to.to_string(),
        from_alias: from.to_string(),
        fk_columns: vec![fk.to_string()],
        ref_columns: vec![pk.to_string()],
        name: Some(name.to_string()),
        cardinality: card,
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

fn metric(name: &str, table: &str, expr: &str, using: &[&str]) -> Metric {
    Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(table.to_string()),
        using_relationships: using.iter().map(|u| (*u).to_string()).collect(),
        ..Default::default()
    }
}

/// Fixture 9's `air` view, its tables declared in `order`.
fn air(order: &[usize]) -> SemanticViewDefinition {
    use Cardinality::{ManyToOne, OneToOne};
    let tables = [
        table("f", "flights", "flight_id"),
        table("ap", "airports", "code"),
        table("d", "airport_details", "code"),
        table("g", "gates", "gate_id"),
        table("t", "terminals", "terminal"),
        table("l", "lounges", "lounge_id"),
        table("rg", "regions", "region_id"),
    ];
    SemanticViewDefinition {
        tables: order.iter().map(|i| tables[*i].clone()).collect(),
        joins: vec![
            rel("dep_airport", "f", "dep", "ap", "code", ManyToOne),
            rel("arr_airport", "f", "arr", "ap", "code", ManyToOne),
            rel("d_airport", "d", "code", "ap", "code", OneToOne),
            rel("g_airport", "g", "code", "ap", "code", ManyToOne),
            rel("d_terminal", "d", "terminal", "t", "terminal", ManyToOne),
            rel("l_airport", "l", "code", "ap", "code", ManyToOne),
            rel("ap_region", "ap", "region_id", "rg", "region_id", ManyToOne),
        ],
        dimensions: vec![
            dim("flight", "f.flight_id", "f"),
            dim("city", "ap.city", "ap"),
            dim("terminal", "d.terminal", "d"),
            dim("gate", "g.gate", "g"),
            dim("operator", "t.operator", "t"),
            dim("region", "rg.region", "rg"),
        ],
        metrics: vec![
            metric("flight_count", "f", "COUNT(f.flight_id)", &[]),
            metric("arrivals", "f", "COUNT(f.flight_id)", &["arr_airport"]),
            metric("departures", "f", "COUNT(f.flight_id)", &["dep_airport"]),
            metric("airport_count", "ap", "COUNT(ap.code)", &[]),
            metric("lounge_count", "l", "COUNT(l.lounge_id)", &[]),
            metric("region_count", "rg", "COUNT(rg.region_id)", &[]),
        ],
        ..Default::default()
    }
}

/// Eight of the 5040 `TABLES` orders: each table first once (flights,
/// airports, lounges and regions among them), and reversed orders, so the
/// first-declared table and the order between the referencing tables vary.
const ORDERS: [[usize; 7]; 8] = [
    [0, 1, 2, 3, 4, 5, 6],
    [6, 5, 4, 3, 2, 1, 0],
    [1, 0, 2, 3, 4, 5, 6],
    [5, 6, 4, 3, 2, 1, 0],
    [6, 1, 0, 5, 2, 3, 4],
    [2, 3, 4, 5, 6, 0, 1],
    [3, 4, 2, 1, 6, 5, 0],
    [4, 2, 0, 6, 1, 3, 5],
];

fn req(dims: &[&str], mets: &[&str], where_clause: Option<&str>) -> QueryRequest {
    QueryRequest {
        where_clause: where_clause.map(str::to_string),
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    }
}

/// Every row of the query, each column as text (`NULL` for a NULL), sorted;
/// or the error it is refused with.
fn try_run(def: &SemanticViewDefinition, req: &QueryRequest) -> Result<Vec<String>, ExpandError> {
    let sql = expand("air", def, req)?;
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
                        duckdb::types::Value::BigInt(n) => n.to_string(),
                        duckdb::types::Value::Int(n) => n.to_string(),
                        duckdb::types::Value::HugeInt(n) => n.to_string(),
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

/// Asks the view, declared in every order, and expects `want` each time.
fn case(dims: &[&str], mets: &[&str], where_clause: Option<&str>, want: &[&str]) {
    let mut want: Vec<String> = want.iter().map(|r| (*r).to_string()).collect();
    want.sort();
    for order in ORDERS {
        let r = req(dims, mets, where_clause);
        assert_eq!(
            try_run(&air(&order), &r).unwrap_or_else(|e| panic!("{order:?}: {e}")),
            want,
            "{r:?}, tables in order {order:?}"
        );
    }
}

/// Asks the view, declared in every order, and expects a refusal each time.
fn refused(dims: &[&str], mets: &[&str], where_clause: Option<&str>) {
    for order in ORDERS {
        let r = req(dims, mets, where_clause);
        if let Ok(rows) = try_run(&air(&order), &r) {
            panic!("{r:?}, tables in order {order:?}: expected a refusal, got {rows:?}");
        }
    }
}

// --- One filter, metrics on two roles: each is filtered along its own ------

/// W3: arrivals at Paris 1 (flight 2), departures from it 2 (flights 1, 3).
#[test]
fn w3_one_city_filter_beside_two_roles() {
    case(
        &[],
        &["arrivals", "departures"],
        Some("city = 'Paris'"),
        &["1/2"],
    );
}

/// W4: the same through the one-to-one details.
#[test]
fn w4_one_terminal_filter_beside_two_roles() {
    case(
        &[],
        &["arrivals", "departures"],
        Some("terminal = 'T2'"),
        &["1/2"],
    );
}

/// W7: by terminal, filtered on the city: each role's T2 row.
#[test]
fn w7_two_roles_by_terminal_filtered_on_the_city() {
    case(
        &["terminal"],
        &["arrivals", "departures"],
        Some("city = 'Paris'"),
        &["T2/1/2"],
    );
}

/// W9: an `OR` across the city and the terminal: arrivals at Paris or T5
/// (flights 1, 2), departures from them (all three).
#[test]
fn w9_an_or_across_the_city_and_the_terminal() {
    case(
        &[],
        &["arrivals", "departures"],
        Some("city = 'Paris' OR terminal = 'T5'"),
        &["2/3"],
    );
}

/// W10: by city, filtered on the terminal.
#[test]
fn w10_two_roles_by_city_filtered_on_the_terminal() {
    case(
        &["city"],
        &["arrivals", "departures"],
        Some("terminal = 'T2'"),
        &["Paris/1/2"],
    );
}

/// R4 (fixture 10b): by a flight's own dimension, so a flight one role's
/// filter drops keeps its row for the other.
#[test]
fn r4_two_roles_by_flight_filtered_on_the_city() {
    case(
        &["flight"],
        &["arrivals", "departures"],
        Some("city = 'Paris'"),
        &["1/NULL/1", "2/1/NULL", "3/NULL/1"],
    );
}

/// W6, W8: a metric naming no role beside the filter is refused, alone or
/// beside one that names a role (Snowflake 010246).
#[test]
fn w6_w8_a_filter_beside_a_metric_naming_no_role_is_refused() {
    refused(&[], &["flight_count"], Some("city = 'Paris'"));
    refused(&[], &["arrivals", "flight_count"], Some("city = 'Paris'"));
}

// --- Lounges reach the airports by one relationship -----------------------

/// L1: lounges by their airport's city.
#[test]
fn l1_lounges_by_city() {
    case(
        &["city"],
        &["lounge_count"],
        None,
        &["New York/1", "Paris/2"],
    );
}

/// L3: lounges beside arrivals by city, each along its own path.
#[test]
fn l3_lounges_and_arrivals_by_city() {
    case(
        &["city"],
        &["lounge_count", "arrivals"],
        None,
        &["London/NULL/1", "New York/1/1", "Paris/2/1"],
    );
}

/// L4: lounges filtered on the city.
#[test]
fn l4_lounges_filtered_on_the_city() {
    case(&[], &["lounge_count"], Some("city = 'Paris'"), &["2"]);
}

/// W11: lounges and arrivals under one filter on the city.
#[test]
fn w11_lounges_and_arrivals_filtered_on_the_city() {
    case(
        &[],
        &["lounge_count", "arrivals"],
        Some("city = 'Paris'"),
        &["2/1"],
    );
}

/// L2, L5, L6 (controls, answered before): lounges by terminal, gate and
/// operator.
#[test]
fn l2_l5_l6_lounges_past_the_airports() {
    case(&["terminal"], &["lounge_count"], None, &["T2/2", "T4/1"]);
    case(
        &["gate"],
        &["lounge_count"],
        None,
        &["G1/2", "G2/2", "NULL/1"],
    );
    case(
        &["operator"],
        &["lounge_count"],
        None,
        &["ADP/2", "PANYNJ/1"],
    );
}

// --- Regions sit above the airports ---------------------------------------

/// P1: regions by their airports' city, each region once per city.
#[test]
fn p1_regions_by_city() {
    case(
        &["city"],
        &["region_count"],
        None,
        &["London/1", "New York/1", "Paris/1"],
    );
}

/// P2: regions by gate, through the airports to their gates.
#[test]
fn p2_regions_by_gate() {
    case(
        &["gate"],
        &["region_count"],
        None,
        &["G1/1", "G2/1", "G3/1", "NULL/1"],
    );
}

/// P3: regions by terminal, through the airports, then one-to-one.
#[test]
fn p3_regions_by_terminal() {
    case(
        &["terminal"],
        &["region_count"],
        None,
        &["T2/1", "T4/1", "T5/1"],
    );
}

/// P5: regions beside arrivals by city.
#[test]
fn p5_regions_and_arrivals_by_city() {
    case(
        &["city"],
        &["region_count", "arrivals"],
        None,
        &["London/1/1", "New York/1/1", "Paris/1/1"],
    );
}

/// P6: regions filtered on a city.
#[test]
fn p6_regions_filtered_on_a_city() {
    case(&[], &["region_count"], Some("city = 'Paris'"), &["1"]);
}

/// P4 (control, answered before): arrivals by the arriving airport's region.
#[test]
fn p4_arrivals_by_region() {
    case(&["region"], &["arrivals"], None, &["EU/2", "NA/1"]);
}

// --- What stays refused, or was answered before ---------------------------

/// G1-G3: a role followed by a fan-out to the gates (Snowflake 000002).
#[test]
fn g1_g3_a_role_then_a_fan_out_is_refused() {
    refused(&["gate"], &["arrivals"], None);
    refused(&["gate"], &["departures"], None);
    refused(&[], &["arrivals"], Some("gate = 'G1'"));
}

/// A1-A6 (controls): the airports' own metric by its own city, the details'
/// terminal, the gates, the operator, beside arrivals, and filtered.
#[test]
fn a1_a6_an_airport_metric() {
    case(
        &["city"],
        &["airport_count"],
        None,
        &["London/1", "New York/1", "Paris/1"],
    );
    case(
        &["terminal"],
        &["airport_count"],
        None,
        &["T2/1", "T4/1", "T5/1"],
    );
    case(
        &["gate"],
        &["airport_count"],
        None,
        &["G1/1", "G2/1", "G3/1", "NULL/1"],
    );
    case(
        &["operator"],
        &["airport_count"],
        None,
        &["ADP/1", "HAL/1", "PANYNJ/1"],
    );
    case(
        &["city"],
        &["airport_count", "arrivals"],
        None,
        &["London/1/1", "New York/1/1", "Paris/1/1"],
    );
    case(&[], &["airport_count"], Some("terminal = 'T2'"), &["1"]);
}

/// C1-C3 (controls): a table one chain further on, by each role.
#[test]
fn c1_c3_operators_by_role() {
    case(
        &["operator"],
        &["arrivals"],
        None,
        &["ADP/1", "HAL/1", "PANYNJ/1"],
    );
    case(&["operator"], &["departures"], None, &["ADP/2", "HAL/1"]);
    case(
        &["operator", "terminal"],
        &["arrivals", "departures"],
        None,
        &["ADP/T2/1/2", "HAL/T5/1/1", "PANYNJ/T4/1/NULL"],
    );
}
