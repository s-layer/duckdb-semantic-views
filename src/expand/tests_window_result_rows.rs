//! Window metrics computed over the query's result rows (Phase 7 PR 5).
//!
//! Snowflake computes a window metric over the rows the rest of the query
//! returns: the inner metric is aggregated per group exactly as it would be on
//! its own, and the window runs over those groups. So a window beside an
//! aggregate metric, a window grouped by a dimension across a bridge, and a
//! window filtered across one each read the rows the plain query gives
//! (Phase 7 fixture 4, W2-W6, 2026-11-10). Each of these used to be refused as
//! a fan trap, or as a window mixed with an aggregate.
//!
//! Each test asks every `TABLES` order and expects Snowflake's rows from each.
//! They pin ROWS: the generated SQL runs against an in-memory DuckDB, one test
//! per case so each reports on its own.

use super::*;
use crate::model::{
    Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef, WindowOrderBy,
    WindowSpec,
};

/// Fixture 4's data. Duo has two French authors, Mixed a French and a UK
/// author, Solo a UK author, Orphan none; Dan (DE) wrote nothing.
fn db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE authors (author_id INT, name VARCHAR, country VARCHAR);
         CREATE TABLE books (book_id INT, title VARCHAR, pages INT);
         CREATE TABLE book_authors (book_id INT, author_id INT);
         INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'), (3, 'Cleo', 'UK'),
                                    (4, 'Dan', 'DE');
         INSERT INTO books VALUES (10, 'Duo', 100), (11, 'Mixed', 200), (12, 'Solo', 300),
                                  (13, 'Orphan', 50);
         INSERT INTO book_authors VALUES (10, 1), (10, 2), (11, 1), (11, 3), (12, 3);
         CREATE TABLE airports (code VARCHAR, city VARCHAR);
         CREATE TABLE flights (flight_id INT, dep VARCHAR, arr VARCHAR);
         CREATE TABLE airport_details (airport_code VARCHAR, terminal VARCHAR);
         INSERT INTO airports VALUES ('CDG', 'Paris'), ('LHR', 'London'), ('JFK', 'New York');
         INSERT INTO flights VALUES (1, 'CDG', 'LHR'), (2, 'LHR', 'CDG'), (3, 'CDG', 'JFK');
         INSERT INTO airport_details VALUES ('CDG', 'T2'), ('LHR', 'T5'), ('JFK', 'T4');",
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

fn rel(name: &str, from: &str, to: &str, fk: &str, pk: &str, card: Cardinality) -> Join {
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

/// A window metric `function(inner) OVER (...)` on `table`.
fn window(
    name: &str,
    table: &str,
    function: &str,
    inner: &str,
    excluding: &[&str],
    order_by: &[&str],
) -> Metric {
    let ws = WindowSpec {
        window_function: function.to_string(),
        inner_metric: inner.to_string(),
        excluding_dims: excluding.iter().map(|d| (*d).to_string()).collect(),
        order_by: order_by
            .iter()
            .map(|d| WindowOrderBy {
                expr: (*d).to_string(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    Metric {
        name: name.to_string(),
        expr: crate::render_ddl::render_window_spec(&ws),
        source_table: Some(table.to_string()),
        window_spec: Some(ws),
        ..Default::default()
    }
}

/// Every order of three tables.
const ORDERS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

/// Fixture 4's `wu` view, its tables declared in `order`: authors and books
/// through the `book_authors` bridge, and two window metrics over the books'
/// `total_pages`.
fn books(order: [usize; 3]) -> SemanticViewDefinition {
    let tables = [
        table("a", "authors", &["author_id"]),
        table("b", "books", &["book_id"]),
        table("ba", "book_authors", &["book_id", "author_id"]),
    ];
    SemanticViewDefinition {
        tables: order.iter().map(|i| tables[*i].clone()).collect(),
        joins: vec![
            rel(
                "ba_author",
                "ba",
                "a",
                "author_id",
                "author_id",
                Cardinality::ManyToOne,
            ),
            rel(
                "ba_book",
                "ba",
                "b",
                "book_id",
                "book_id",
                Cardinality::ManyToOne,
            ),
        ],
        dimensions: vec![
            dim("country", "a.country", "a"),
            dim("title", "b.title", "b"),
        ],
        metrics: vec![
            metric("total_pages", "b", "SUM(b.pages)", &[]),
            window("avg_excl_title", "b", "AVG", "total_pages", &["title"], &[]),
            window(
                "running_by_title",
                "b",
                "SUM",
                "total_pages",
                &[],
                &["title"],
            ),
        ],
        ..Default::default()
    }
}

/// Fixture 6's `air` view, its tables declared in `order`: flights reference
/// airports twice, and the details reference airports one-to-one. The
/// windows run over `arrivals`, read on the arriving airport.
fn air(order: [usize; 3]) -> SemanticViewDefinition {
    let tables = [
        table("f", "flights", &["flight_id"]),
        table("ap", "airports", &["code"]),
        table("d", "airport_details", &["airport_code"]),
    ];
    SemanticViewDefinition {
        tables: order.iter().map(|i| tables[*i].clone()).collect(),
        joins: vec![
            rel(
                "dep_airport",
                "f",
                "ap",
                "dep",
                "code",
                Cardinality::ManyToOne,
            ),
            rel(
                "arr_airport",
                "f",
                "ap",
                "arr",
                "code",
                Cardinality::ManyToOne,
            ),
            rel(
                "d_airport",
                "d",
                "ap",
                "airport_code",
                "code",
                Cardinality::OneToOne,
            ),
        ],
        dimensions: vec![
            dim("city", "ap.city", "ap"),
            dim("terminal", "d.terminal", "d"),
        ],
        metrics: vec![
            metric("arrivals", "f", "COUNT(f.flight_id)", &["arr_airport"]),
            metric("departures", "f", "COUNT(f.flight_id)", &["dep_airport"]),
            window(
                "arrivals_running",
                "f",
                "SUM",
                "arrivals",
                &[],
                &["terminal"],
            ),
        ],
        ..Default::default()
    }
}

fn req(dims: &[&str], mets: &[&str], where_clause: Option<&str>) -> QueryRequest {
    QueryRequest {
        where_clause: where_clause.map(str::to_string),
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

/// Assert `request` gives `expected` on `view` in every `TABLES` order.
fn assert_answer(
    view: fn([usize; 3]) -> SemanticViewDefinition,
    request: &QueryRequest,
    expected: &[&[&str]],
) {
    for order in ORDERS {
        let sql = expand("v", &view(order), request)
            .unwrap_or_else(|e| panic!("TABLES order {order:?}: {e}"));
        assert_eq!(
            rows(&sql),
            table_of(expected),
            "TABLES order {order:?}\n{sql}"
        );
    }
}

/// W2: a running total beside the aggregate it runs over.
#[test]
fn w2_window_beside_its_inner_aggregate() {
    assert_answer(
        books,
        &req(&["title"], &["total_pages", "running_by_title"], None),
        &[
            &["Duo", "100", "100"],
            &["Mixed", "200", "300"],
            &["Orphan", "50", "350"],
            &["Solo", "300", "650"],
        ],
    );
}

/// W2, the window listed before the aggregate: the columns keep the
/// request's order.
#[test]
fn w2_columns_keep_the_request_order() {
    assert_answer(
        books,
        &req(&["title"], &["running_by_title", "total_pages"], None),
        &[
            &["Duo", "100", "100"],
            &["Mixed", "300", "200"],
            &["Orphan", "350", "50"],
            &["Solo", "650", "300"],
        ],
    );
}

/// W4: per (country, title) pages, averaged per country across the bridge.
#[test]
fn w4_excluding_window_across_a_bridge() {
    assert_answer(
        books,
        &req(
            &["country", "title"],
            &["total_pages", "avg_excl_title"],
            None,
        ),
        &[
            &["FR", "Duo", "100", "150.0"],
            &["FR", "Mixed", "200", "150.0"],
            &["UK", "Mixed", "200", "250.0"],
            &["UK", "Solo", "300", "250.0"],
            &["NULL", "Orphan", "50", "50.0"],
        ],
    );
}

/// W5: a running total by title across the bridge, over every result row.
#[test]
fn w5_running_window_across_a_bridge() {
    assert_answer(
        books,
        &req(
            &["country", "title"],
            &["total_pages", "running_by_title"],
            None,
        ),
        &[
            &["FR", "Duo", "100", "100"],
            &["FR", "Mixed", "200", "500"],
            &["UK", "Mixed", "200", "500"],
            &["NULL", "Orphan", "50", "550"],
            &["UK", "Solo", "300", "850"],
        ],
    );
}

/// W6: the running total filtered across the bridge to French books.
#[test]
fn w6_window_filtered_across_a_bridge() {
    assert_answer(
        books,
        &req(&["title"], &["running_by_title"], Some("country = 'FR'")),
        &[&["Duo", "100"], &["Mixed", "300"]],
    );
}

/// A window over a `USING` metric by a table reached back across the
/// role-played airports: the running total over R1's rows (TECH-DEBT #111).
#[test]
fn window_over_a_role_by_a_table_back_across_it() {
    assert_answer(
        air,
        &req(&["terminal"], &["arrivals_running"], None),
        &[&["T2", "1"], &["T4", "2"], &["T5", "3"]],
    );
}

/// ... beside a metric on the other role, each through its own airport (R4).
#[test]
fn window_beside_the_other_role() {
    assert_answer(
        air,
        &req(&["terminal"], &["arrivals_running", "departures"], None),
        &[&["T2", "1", "2"], &["T4", "2", "NULL"], &["T5", "3", "1"]],
    );
}

/// ... and filtered on the city its role reads: one flight arrives in Paris
/// (TECH-DEBT #111, the window half of a `WHERE` on a role).
#[test]
fn window_filtered_on_its_role() {
    assert_answer(
        air,
        &req(&["terminal"], &["arrivals_running"], Some("city = 'Paris'")),
        &[&["T2", "1"]],
    );
}

/// A window metric's own `USING` reaches an inner metric that names no role:
/// the window over `flight_count USING (arr_airport)` reads R1's rows.
#[test]
fn a_window_metrics_own_using_reaches_its_inner_metric() {
    let view = |order: [usize; 3]| {
        let mut def = air(order);
        def.metrics
            .push(metric("flight_count", "f", "COUNT(f.flight_id)", &[]));
        let mut w = window(
            "arr_running",
            "f",
            "SUM",
            "flight_count",
            &[],
            &["terminal"],
        );
        w.using_relationships = vec!["arr_airport".to_string()];
        def.metrics.push(w);
        def
    };
    assert_answer(
        view,
        &req(&["terminal"], &["arr_running"], None),
        &[&["T2", "1"], &["T4", "2"], &["T5", "3"]],
    );
}

/// A window over a window metric is refused: the inner window gives no
/// per-group rows of its own to run over.
#[test]
fn a_window_over_a_window_is_refused() {
    let mut def = air([0, 1, 2]);
    def.metrics.push(window(
        "running_of_running",
        "f",
        "SUM",
        "arrivals_running",
        &[],
        &["terminal"],
    ));
    let err = expand(
        "v",
        &def,
        &req(&["terminal"], &["running_of_running"], None),
    )
    .expect_err("a window over a window is refused");
    assert!(
        matches!(&err, ExpandError::WindowOverWindow { inner_metric, .. }
            if inner_metric == "arrivals_running"),
        "got: {err}"
    );
}

/// A window over a `PRIVATE` metric is queryable where its inner metric
/// alone is not.
#[test]
fn a_window_over_a_private_metric_is_queryable() {
    let view = |order: [usize; 3]| {
        let mut def = books(order);
        def.metrics[0].access = crate::model::AccessModifier::Private;
        def
    };
    assert_answer(
        view,
        &req(&["title"], &["running_by_title"], None),
        &[
            &["Duo", "100"],
            &["Mixed", "300"],
            &["Orphan", "350"],
            &["Solo", "650"],
        ],
    );
    let mut private = books([0, 1, 2]);
    private.metrics[0].access = crate::model::AccessModifier::Private;
    assert!(
        expand("v", &private, &req(&["title"], &["total_pages"], None)).is_err(),
        "the private inner metric alone stays refused"
    );
}

/// Reviewbot on #348: the query a window is computed over is not routed to a
/// materialization, as a query holding a window is not, so EXPLAIN (which
/// asks about the outer request) and the SQL agree.
#[test]
fn a_window_query_is_not_routed_to_a_materialization() {
    let view = |order: [usize; 3]| {
        let mut def = books(order);
        def.materializations.push(crate::model::Materialization {
            name: "pages_by_title".to_string(),
            table: "mat_pages_by_title".to_string(),
            dimensions: vec!["title".to_string()],
            metrics: vec!["total_pages".to_string()],
        });
        def
    };
    let request = req(&["title"], &["running_by_title"], None);
    let sql = expand("v", &view([0, 1, 2]), &request).expect("expands");
    assert!(
        !sql.contains("mat_pages_by_title"),
        "the inner query was routed to the materialization:\n{sql}"
    );
}

/// Fixture 6's view with a `legs` table under the flights, the flights given
/// no PRIMARY KEY, and a window with its own `USING` over `flight_count`.
fn air_with_legs() -> SemanticViewDefinition {
    let mut def = air([0, 1, 2]);
    def.tables[0].pk_columns.clear();
    def.tables.push(table("l", "legs", &["leg_id"]));
    def.joins.push(rel(
        "leg_flight",
        "l",
        "f",
        "flight_id",
        "flight_id",
        Cardinality::ManyToOne,
    ));
    def.dimensions.push(dim("kind", "l.kind", "l"));
    def.metrics
        .push(metric("flight_count", "f", "COUNT(f.flight_id)", &[]));
    let mut w = window(
        "arr_running",
        "f",
        "SUM",
        "flight_count",
        &[],
        &["terminal"],
    );
    w.using_relationships = vec!["arr_airport".to_string()];
    def.metrics.push(w);
    def
}

/// Reviewbot on #348: an error from the query a window's own `USING` reaches
/// its inner metric through names the window the user queried, not an
/// internal copy of the inner metric.
#[test]
fn an_error_through_a_windows_own_using_names_the_window() {
    let def = air_with_legs();
    let err = expand(
        "v",
        &def,
        &req(&["terminal", "kind"], &["arr_running"], None),
    )
    .expect_err("a fan-out of a table with no PRIMARY KEY is refused");
    let msg = err.to_string();
    assert!(
        msg.contains("arr_running") && !msg.contains("__sv_"),
        "got: {msg}"
    );
}

/// ... and a view declaring a metric named like an internal one keeps the
/// window's `USING`.
#[test]
fn a_windows_own_using_survives_a_metric_named_like_an_internal_one() {
    let view = |order: [usize; 3]| {
        let mut def = air(order);
        def.metrics
            .push(metric("flight_count", "f", "COUNT(f.flight_id)", &[]));
        def.metrics.push(metric(
            "__sv_window_0",
            "f",
            "COUNT(f.flight_id)",
            &["dep_airport"],
        ));
        let mut w = window(
            "arr_running",
            "f",
            "SUM",
            "flight_count",
            &[],
            &["terminal"],
        );
        w.using_relationships = vec!["arr_airport".to_string()];
        def.metrics.push(w);
        def
    };
    assert_answer(
        view,
        &req(&["terminal"], &["arr_running"], None),
        &[&["T2", "1"], &["T4", "2"], &["T5", "3"]],
    );
}

/// Reviewbot on #348: a window's own `USING` names a relationship from the
/// window's table, so it cannot be followed by an inner metric on another
/// table, or by a derived one (which has no table). Such a window is refused
/// rather than answered along the default path.
#[test]
fn a_windows_own_using_over_an_inner_metric_off_its_table_is_refused() {
    for (inner, inner_table, expr) in [
        ("airport_count", Some("ap"), "COUNT(ap.code)"),
        ("arrivals_twice", None, "arrivals * 2"),
    ] {
        let mut def = air([0, 1, 2]);
        def.metrics.push(Metric {
            name: inner.to_string(),
            expr: expr.to_string(),
            source_table: inner_table.map(str::to_string),
            ..Default::default()
        });
        let mut w = window("w", "f", "SUM", inner, &[], &["terminal"]);
        w.using_relationships = vec!["arr_airport".to_string()];
        def.metrics.push(w);
        let err = expand("v", &def, &req(&["terminal"], &["w"], None))
            .expect_err("a window USING over an inner metric off its table");
        assert!(
            matches!(&err, ExpandError::WindowUsingOffItsTable { metric_name, inner_metric, .. }
                if metric_name == "w" && inner_metric == inner),
            "{inner}: got: {err}"
        );
    }
}

/// Reviewbot on #348: a window whose clause names a dimension its inner
/// metric cannot be grouped by answers no query, so it lists nothing. Books
/// with no PRIMARY KEY cannot be counted once per bridge row's role.
#[test]
fn a_window_that_answers_no_query_lists_nothing() {
    let mut def = books([0, 1, 2]);
    for t in &mut def.tables {
        if t.alias == "b" {
            t.pk_columns.clear();
        }
    }
    def.dimensions.push(dim("role", "ba.role", "ba"));
    def.metrics
        .push(window("by_role", "b", "SUM", "total_pages", &[], &["role"]));
    let w = def.metrics.last().expect("window");
    let listed: Vec<&str> = crate::expand::dimensions_for_metric(&def, w)
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert!(listed.is_empty(), "listed: {listed:?}");
    assert!(
        expand("v", &def, &req(&["role"], &["by_role"], None)).is_err(),
        "no query of the window answers"
    );
}
