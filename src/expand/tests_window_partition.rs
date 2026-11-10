//! What a window metric's `OVER` clause partitions by, pinned as ROWS.
//!
//! Snowflake (the 2026-11-10 fixture 4, cases W2, W3 and W5) runs a window
//! with no `PARTITION BY` over all the result rows: `SUM(total_pages) OVER
//! (ORDER BY title)` is a running total across every row, and rows with the
//! same `ORDER BY` value are peers. `PARTITION BY EXCLUDING` partitions by the
//! queried dimensions it does not name, and `PARTITION BY` by the ones it
//! names. We used to partition a window with no `PARTITION BY` by every
//! queried dimension, so each row was its own partition.
//!
//! The view is one table of (book, country) rows, so a book with authors in
//! two countries has two rows; that is W5's shape without the bridge.

use super::*;
use crate::model::{
    Dimension, Metric, NullsOrder, SemanticViewDefinition, SortOrder, TableRef, WindowOrderBy,
    WindowSpec,
};

/// Duo: FR; Mixed: FR and UK; Solo: UK; Orphan: no country. By title the
/// pages are Duo 100, Mixed 400, Orphan 50, Solo 300 (850 in all).
fn db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE book_rows (title VARCHAR, country VARCHAR, pages INT);
         INSERT INTO book_rows VALUES
           ('Duo', 'FR', 100), ('Mixed', 'FR', 200), ('Mixed', 'UK', 200),
           ('Solo', 'UK', 300), ('Orphan', NULL, 50);",
    )
    .expect("fixture data");
    con
}

fn dim(name: &str) -> Dimension {
    Dimension {
        name: name.to_string(),
        expr: format!("b.{name}"),
        source_table: Some("b".to_string()),
        ..Default::default()
    }
}

fn order(by: &str) -> Vec<WindowOrderBy> {
    vec![WindowOrderBy {
        expr: by.to_string(),
        order: SortOrder::Asc,
        nulls: NullsOrder::Last,
    }]
}

fn window(name: &str, func: &str, extra_args: &[&str], spec: WindowSpec) -> Metric {
    let args: String = extra_args.iter().map(|a| format!(", {a}")).collect();
    Metric {
        name: name.to_string(),
        expr: format!("{func}(total_pages{args}) OVER (...)"),
        source_table: Some("b".to_string()),
        window_spec: Some(WindowSpec {
            window_function: func.to_string(),
            inner_metric: "total_pages".to_string(),
            extra_args: extra_args.iter().map(|a| (*a).to_string()).collect(),
            ..spec
        }),
        ..Default::default()
    }
}

fn view() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![TableRef {
            alias: "b".to_string(),
            table: "book_rows".to_string(),
            ..Default::default()
        }],
        dimensions: vec![dim("title"), dim("country")],
        metrics: vec![
            Metric {
                name: "total_pages".to_string(),
                expr: "SUM(b.pages)".to_string(),
                source_table: Some("b".to_string()),
                ..Default::default()
            },
            // `SUM(total_pages) OVER (ORDER BY title)`
            window(
                "running_by_title",
                "SUM",
                &[],
                WindowSpec {
                    order_by: order("title"),
                    ..Default::default()
                },
            ),
            // `SUM(total_pages) OVER ()`
            window("grand_total", "SUM", &[], WindowSpec::default()),
            // `LAG(total_pages, 1) OVER (ORDER BY title)`
            window(
                "previous_title",
                "LAG",
                &["1"],
                WindowSpec {
                    order_by: order("title"),
                    ..Default::default()
                },
            ),
            // `AVG(total_pages) OVER (PARTITION BY EXCLUDING title)`
            window(
                "avg_excl_title",
                "AVG",
                &[],
                WindowSpec {
                    excluding_dims: vec!["title".to_string()],
                    ..Default::default()
                },
            ),
            // `SUM(total_pages) OVER (PARTITION BY country ORDER BY title)`
            window(
                "running_in_country",
                "SUM",
                &[],
                WindowSpec {
                    partition_dims: vec!["country".to_string()],
                    order_by: order("title"),
                    ..Default::default()
                },
            ),
        ],
        ..Default::default()
    }
}

/// The rows the query gives, each column as text (`NULL` for a NULL), sorted.
fn rows(dims: &[&str], mets: &[&str]) -> Vec<Vec<String>> {
    let request = QueryRequest {
        // PIN: no case here filters; the window's partition is the question.
        where_clause: None,
        // PIN: a window metric is an aggregate shape; FACTS returns unaggregated rows.
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    };
    let sql = expand("wv", &view(), &request).unwrap_or_else(|e| panic!("{e}"));
    let con = db();
    let mut stmt = con
        .prepare(&sql)
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

/// W2 (the window alone): a running total over the titles.
#[test]
fn order_by_without_partition_runs_over_all_rows() {
    assert_eq!(
        rows(&["title"], &["running_by_title"]),
        table_of(&[
            &["Duo", "100"],
            &["Mixed", "500"],
            &["Orphan", "550"],
            &["Solo", "850"],
        ])
    );
}

/// W5's shape: one total across both countries, not one per country, and the
/// two Mixed rows are peers (same title), so both carry 500.
#[test]
fn order_by_without_partition_ignores_the_other_dimension() {
    assert_eq!(
        rows(&["country", "title"], &["running_by_title"]),
        table_of(&[
            &["FR", "Duo", "100"],
            &["FR", "Mixed", "500"],
            &["UK", "Mixed", "500"],
            &["NULL", "Orphan", "550"],
            &["UK", "Solo", "850"],
        ])
    );
}

/// An empty `OVER ()` is the whole result: the grand total on every row.
#[test]
fn empty_over_clause_is_the_whole_result() {
    assert_eq!(
        rows(&["title"], &["grand_total"]),
        table_of(&[
            &["Duo", "850"],
            &["Mixed", "850"],
            &["Orphan", "850"],
            &["Solo", "850"],
        ])
    );
}

/// `LAG` over all rows reads the previous title's pages.
#[test]
fn lag_without_partition_reads_the_previous_row() {
    assert_eq!(
        rows(&["title"], &["previous_title"]),
        table_of(&[
            &["Duo", "NULL"],
            &["Mixed", "100"],
            &["Orphan", "400"],
            &["Solo", "50"],
        ])
    );
}

/// W3: excluding the only queried dimension leaves no partition, so the
/// average is over every title. A guard: this already ran over all rows.
#[test]
fn excluding_every_queried_dimension_averages_all_rows() {
    assert_eq!(
        rows(&["title"], &["avg_excl_title"]),
        table_of(&[
            &["Duo", "212.5"],
            &["Mixed", "212.5"],
            &["Orphan", "212.5"],
            &["Solo", "212.5"],
        ])
    );
}

/// `PARTITION BY EXCLUDING` still partitions by the queried dimensions it
/// does not name. A guard.
#[test]
fn excluding_partitions_by_the_other_queried_dimensions() {
    assert_eq!(
        rows(&["country", "title"], &["avg_excl_title"]),
        table_of(&[
            &["FR", "Duo", "150.0"],
            &["FR", "Mixed", "150.0"],
            &["UK", "Mixed", "250.0"],
            &["UK", "Solo", "250.0"],
            &["NULL", "Orphan", "50.0"],
        ])
    );
}

/// An explicit `PARTITION BY` partitions by what it names. A guard.
#[test]
fn explicit_partition_runs_within_each_partition() {
    assert_eq!(
        rows(&["country", "title"], &["running_in_country"]),
        table_of(&[
            &["FR", "Duo", "100"],
            &["FR", "Mixed", "300"],
            &["UK", "Mixed", "200"],
            &["UK", "Solo", "500"],
            &["NULL", "Orphan", "50"],
        ])
    );
}
