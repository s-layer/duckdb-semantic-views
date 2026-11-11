//! A table reached back across a role-played table (TECH-DEBT #111).
//!
//! Flights reference airports twice (`dep_airport`, `arr_airport`), and
//! `airport_details` references airports one-to-one. A flight metric whose
//! `USING` names one role, grouped by or filtered on a detail, hops up to that
//! role's airport and back down to its details. Snowflake answers it so
//! (Phase 7 fixture 6, R1-R7, 2026-11-11), each metric by its own role, and
//! refuses it when no `USING` names a role.
//!
//! Each test asks every `TABLES` order and expects Snowflake's rows from each.
//! They pin ROWS: the generated SQL runs against an in-memory DuckDB, one test
//! per case so each reports on its own.

use super::*;
use crate::model::{Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef};

/// Fixture 6's data: flights 1 CDG -> LHR, 2 LHR -> CDG, 3 CDG -> JFK;
/// terminals CDG T2, LHR T5, JFK T4. The details' key is named apart from the
/// airports' (`airport_code`, `code`), so an ON clause that walks their
/// relationship the wrong way round fails to bind.
fn db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE airports (code VARCHAR, city VARCHAR);
         CREATE TABLE flights (flight_id INT, dep VARCHAR, arr VARCHAR);
         CREATE TABLE airport_details (airport_code VARCHAR, terminal VARCHAR);
         INSERT INTO airports VALUES ('CDG', 'Paris'), ('LHR', 'London'), ('JFK', 'New York');
         INSERT INTO flights VALUES (1, 'CDG', 'LHR'), (2, 'LHR', 'CDG'), (3, 'CDG', 'JFK');
         INSERT INTO airport_details VALUES ('CDG', 'T2'), ('LHR', 'T5'), ('JFK', 'T4');",
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

fn rel(name: &str, from: &str, fk: &str, to: &str, card: Cardinality) -> Join {
    Join {
        table: to.to_string(),
        from_alias: from.to_string(),
        fk_columns: vec![fk.to_string()],
        ref_columns: vec!["code".to_string()],
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

/// Fixture 6's `air` view, its tables declared in `order`.
fn air(order: &[usize]) -> SemanticViewDefinition {
    let tables = [
        table("f", "flights", "flight_id"),
        table("ap", "airports", "code"),
        table("d", "airport_details", "airport_code"),
    ];
    SemanticViewDefinition {
        tables: order.iter().map(|i| tables[*i].clone()).collect(),
        joins: vec![
            rel("dep_airport", "f", "dep", "ap", Cardinality::ManyToOne),
            rel("arr_airport", "f", "arr", "ap", Cardinality::ManyToOne),
            rel(
                "d_airport",
                "d",
                "airport_code",
                "ap",
                Cardinality::OneToOne,
            ),
        ],
        dimensions: vec![
            dim("city", "ap.city", "ap"),
            dim("terminal", "d.terminal", "d"),
        ],
        metrics: vec![
            metric("flight_count", "f", "COUNT(f.flight_id)", &[]),
            metric("arrivals", "f", "COUNT(f.flight_id)", &["arr_airport"]),
            metric("departures", "f", "COUNT(f.flight_id)", &["dep_airport"]),
            metric("airport_count", "ap", "COUNT(ap.code)", &[]),
        ],
        ..Default::default()
    }
}

const ORDERS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
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

/// R1: arrivals by the arriving airport's terminal.
#[test]
fn r1_arrivals_by_terminal_follow_the_arriving_airport() {
    case(
        &["terminal"],
        &["arrivals"],
        None,
        &["T2/1", "T4/1", "T5/1"],
    );
}

/// R2: departures by the departing airport's terminal.
#[test]
fn r2_departures_by_terminal_follow_the_departing_airport() {
    case(&["terminal"], &["departures"], None, &["T2/2", "T5/1"]);
}

/// R3: with no `USING` the way to the details crosses either role, and is
/// refused (Snowflake 010246, multi-path).
#[test]
fn r3_a_metric_naming_no_role_is_refused() {
    for order in ORDERS {
        match try_run(&air(&order), &req(&["terminal"], &["flight_count"], None)) {
            Err(ExpandError::AmbiguousDescendantPath {
                role_playing_table, ..
            }) => assert_eq!(role_playing_table, "ap"),
            other => panic!("{order:?}: expected a refusal, got {other:?}"),
        }
    }
}

/// R4: each metric by its own role's terminal; no flight departs from JFK.
#[test]
fn r4_two_roles_by_terminal_each_follow_their_own() {
    case(
        &["terminal"],
        &["arrivals", "departures"],
        None,
        &["T2/1/2", "T4/1/NULL", "T5/1/1"],
    );
}

/// R5: a filter on the terminal follows the metric's role: flight 2 lands
/// at CDG (T2).
#[test]
fn r5_a_filter_on_the_terminal_follows_the_metrics_role() {
    case(&[], &["arrivals"], Some("terminal = 'T2'"), &["1"]);
}

/// R6: the arriving airport's city and terminal.
#[test]
fn r6_city_and_terminal_of_the_arriving_airport() {
    case(
        &["city", "terminal"],
        &["arrivals"],
        None,
        &["London/T5/1", "New York/T4/1", "Paris/T2/1"],
    );
}

/// R7: the terminals alone, read from the details.
#[test]
fn r7_terminals_alone() {
    case(&["terminal"], &[], None, &["T2", "T4", "T5"]);
}

/// A metric on the role-played table by its own dimension reads its own rows
/// and names no role, beside the details that also reference it (the
/// CHANGELOG's promise, which the details' relationship broke).
#[test]
fn an_airport_metric_by_its_own_city() {
    case(
        &["city"],
        &["airport_count"],
        None,
        &["London/1", "New York/1", "Paris/1"],
    );
}

/// The same by the one-to-one details' terminal.
#[test]
fn an_airport_metric_by_its_details_terminal() {
    case(
        &["terminal"],
        &["airport_count"],
        None,
        &["T2/1", "T4/1", "T5/1"],
    );
}

/// R5's reading on the role-played table itself: arrivals filtered on the
/// arriving airport's city. Flight 2 lands in Paris.
#[test]
fn a_filter_on_the_city_follows_the_metrics_role() {
    case(&[], &["arrivals"], Some("city = 'Paris'"), &["1"]);
    case(&[], &["departures"], Some("ap.city = 'Paris'"), &["2"]);
}

/// R5 and R6 together: the arriving airport's city, filtered on its terminal.
#[test]
fn a_city_filtered_on_the_terminal_follows_the_metrics_role() {
    case(
        &["city"],
        &["arrivals"],
        Some("terminal = 'T2'"),
        &["Paris/1"],
    );
}

/// Two metrics naming two roles share one predicate, which neither role is
/// read for: refused, as before (fixture 9's W3 and W4 ask Snowflake).
#[test]
fn a_filter_beside_two_roles_is_refused() {
    for order in ORDERS {
        for filter in ["terminal = 'T2'", "city = 'Paris'"] {
            match try_run(
                &air(&order),
                &req(&[], &["arrivals", "departures"], Some(filter)),
            ) {
                Err(ExpandError::AmbiguousWhereClausePath {
                    role_playing_table, ..
                }) => assert_eq!(role_playing_table, "ap"),
                other => panic!("{order:?} {filter}: expected a refusal, got {other:?}"),
            }
        }
    }
}

/// With no `USING` the filter names no role (fixture 9's W6 asks Snowflake).
#[test]
fn a_filter_beside_no_role_is_refused() {
    for order in ORDERS {
        match try_run(
            &air(&order),
            &req(&[], &["flight_count"], Some("terminal = 'T2'")),
        ) {
            Err(ExpandError::AmbiguousWhereClausePath {
                role_playing_table, ..
            }) => assert_eq!(role_playing_table, "ap"),
            other => panic!("{order:?}: expected a refusal, got {other:?}"),
        }
    }
}
