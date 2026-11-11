//! Many-to-many bridges (Phase 7.3): a metric grouped by a dimension reached
//! through a table that references both sides.
//!
//! Snowflake counts each row of the metric's table once per group, never once
//! per bridge row (the 7.0 fixture's B1-B10, whose numbers these tests reuse):
//! a book with two French authors counts once for FR, a book with a French and
//! a UK author counts under both, and a book with no authors lands in the NULL
//! group. These tests pin NUMBERS: each executes the generated SQL against an
//! in-memory DuckDB, one test per case so each reports on its own.

use super::*;
use crate::model::{
    Cardinality, Dimension, Join, Metric, NonAdditiveDim, SemanticViewDefinition, TableRef,
};

/// The fixture's data:
///
/// | book   | pages | authors (role)          |
/// |--------|-------|-------------------------|
/// | Duo    | 100   | Ana FR (lead), Ben FR (co) |
/// | Mixed  | 200   | Ana FR (lead), Cleo UK (co) |
/// | Solo   | 300   | Cleo UK (lead)          |
/// | Orphan | 50    | none                    |
///
/// Dan (DE) wrote nothing. FR and UK are in Europe. Sales: Ana bought books 1
/// and 2, Ben the toy and book 1, Cleo nothing.
fn db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE authors (author_id INT, name VARCHAR, country VARCHAR);
         CREATE TABLE books (book_id INT, title VARCHAR, pages INT, isbn VARCHAR);
         CREATE TABLE book_authors (book_id INT, author_id INT, role VARCHAR);
         CREATE TABLE countries (country VARCHAR, continent VARCHAR);
         CREATE TABLE reviews (review_id INT, book_id INT, stars INT);
         INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'), (3, 'Cleo', 'UK'),
                                    (4, 'Dan', 'DE');
         INSERT INTO books VALUES (10, 'Duo', 100, 'i10'), (11, 'Mixed', 200, 'i11'),
                                  (12, 'Solo', 300, 'i12'), (13, 'Orphan', 50, 'i13');
         INSERT INTO book_authors VALUES (10, 1, 'lead'), (10, 2, 'co'), (11, 1, 'lead'),
                                         (11, 3, 'co'), (12, 3, 'lead');
         INSERT INTO countries VALUES ('FR', 'Europe'), ('UK', 'Europe'), ('DE', 'Europe');
         INSERT INTO reviews VALUES (1, 10, 5), (2, 10, 3), (3, 11, 4);
         CREATE TABLE editions (edition_id INT, book_id INT, format VARCHAR);
         INSERT INTO editions VALUES (1, 10, 'hard'), (2, 10, 'soft'), (3, 11, 'hard'),
                                     (4, 12, 'soft');
         CREATE TABLE customers (cust_id INT, cust_name VARCHAR);
         CREATE TABLE products (product_id INT, category VARCHAR);
         CREATE TABLE sales (sale_id INT, cust_id INT, product_id INT, amount INT);
         INSERT INTO customers VALUES (1, 'Ana'), (2, 'Ben'), (3, 'Cleo');
         INSERT INTO products VALUES (1, 'book'), (2, 'book'), (3, 'toy');
         INSERT INTO sales VALUES (1, 1, 1, 10), (2, 1, 2, 20), (3, 2, 3, 30), (4, 2, 1, 40);",
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

fn metric(name: &str, expr: &str, table: Option<&str>) -> Metric {
    Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: table.map(str::to_string),
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

/// The fixture's `p7_bridge`, with `countries` past the authors (B9) and
/// `reviews` under the books, a plain child that is no bridge. `first` is the
/// alias declared first, the base table: a bridge must not depend on it.
fn bridge_def(first: &str) -> SemanticViewDefinition {
    let mut tables = vec![
        table("b", "books", &["book_id"]),
        table("a", "authors", &["author_id"]),
        table("ba", "book_authors", &["book_id", "author_id"]),
        table("c", "countries", &["country"]),
        table("r", "reviews", &["review_id"]),
    ];
    let at = tables.iter().position(|t| t.alias == first).expect("alias");
    let base = tables.remove(at);
    tables.insert(0, base);
    SemanticViewDefinition {
        tables,
        dimensions: vec![
            dim("country", "a.country", "a"),
            dim("author", "a.name", "a"),
            dim("title", "b.title", "b"),
            dim("role", "ba.role", "ba"),
            dim("continent", "c.continent", "c"),
            dim("stars", "r.stars", "r"),
        ],
        metrics: vec![
            metric("total_pages", "SUM(b.pages)", Some("b")),
            metric("book_count", "COUNT(*)", Some("b")),
            metric("distinct_books", "COUNT(DISTINCT b.book_id)", Some("b")),
            metric("author_count", "COUNT(a.author_id)", Some("a")),
            metric("review_count", "COUNT(*)", Some("r")),
            metric("pages_per_author", "total_pages / author_count", None),
        ],
        joins: vec![
            rel("ba_author", "ba", "a", "author_id", "author_id"),
            rel("ba_book", "ba", "b", "book_id", "book_id"),
            rel("a_country", "a", "c", "country", "country"),
            rel("r_book", "r", "b", "book_id", "book_id"),
        ],
        ..Default::default()
    }
}

/// [`bridge_def`] with `editions`, a second plain child of books beside
/// `reviews`: Duo has a hardback and a paperback, Mixed a hardback, Solo a
/// paperback, Orphan none.
fn sibling_def(first: &str) -> SemanticViewDefinition {
    let mut def = bridge_def("b");
    def.tables.push(table("e", "editions", &["edition_id"]));
    let at = def
        .tables
        .iter()
        .position(|t| t.alias == first)
        .expect("alias");
    let base = def.tables.remove(at);
    def.tables.insert(0, base);
    def.dimensions.push(dim("format", "e.format", "e"));
    def.metrics
        .push(metric("edition_count", "COUNT(e.edition_id)", Some("e")));
    def.joins
        .push(rel("e_book", "e", "b", "book_id", "book_id"));
    def
}

/// The fixture's `p7_fact_bridge`: customers and products through sales.
fn fact_bridge_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("c", "customers", &["cust_id"]),
            table("p", "products", &["product_id"]),
            table("s", "sales", &["sale_id"]),
        ],
        dimensions: vec![dim("category", "p.category", "p")],
        metrics: vec![
            metric("customer_count", "COUNT(c.cust_id)", Some("c")),
            metric("revenue", "SUM(s.amount)", Some("s")),
        ],
        joins: vec![
            rel("sale_customer", "s", "c", "cust_id", "cust_id"),
            rel("sale_product", "s", "p", "product_id", "product_id"),
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

fn run(
    def: &SemanticViewDefinition,
    dims: &[&str],
    mets: &[&str],
    where_clause: Option<&str>,
) -> Vec<Vec<String>> {
    let sql = expand("v", def, &req(dims, mets, where_clause)).unwrap_or_else(|e| panic!("{e}"));
    rows(&sql)
}

/// The error a refused query gives.
fn refusal(def: &SemanticViewDefinition, dims: &[&str], mets: &[&str]) -> ExpandError {
    match expand("v", def, &req(dims, mets, None)) {
        Ok(sql) => panic!("expected a refusal, got SQL:\n{sql}"),
        Err(e) => e,
    }
}

/// B1: each book counts once per country, the authorless book in the NULL
/// group. Joining the bridge into the aggregate would give FR 400 / 3.
#[test]
fn metric_by_a_dimension_across_the_bridge_counts_once_per_group() {
    for first in ["b", "a", "ba"] {
        assert_eq!(
            run(
                &bridge_def(first),
                &["country"],
                &["total_pages", "book_count", "distinct_books"],
                None
            ),
            table_of(&[
                &["FR", "300", "2", "2"],
                &["UK", "500", "2", "2"],
                &["NULL", "50", "1", "1"],
            ]),
            "base table {first}"
        );
    }
}

/// B2: with no dimension the total is the books' own, not inflated.
#[test]
fn metric_alone_is_not_inflated_by_the_bridge() {
    assert_eq!(
        run(
            &bridge_def("a"),
            &[],
            &["total_pages", "book_count", "distinct_books"],
            None
        ),
        table_of(&[&["650", "4", "4"]])
    );
}

/// B3: grouping by the author: Duo under Ana and under Ben.
#[test]
fn metric_by_the_far_side_key_dimension() {
    assert_eq!(
        run(&bridge_def("a"), &["author"], &["total_pages"], None),
        table_of(&[
            &["Ana", "300"],
            &["Ben", "100"],
            &["Cleo", "500"],
            &["NULL", "50"],
        ])
    );
}

/// B5: a filter across the bridge keeps a book when ANY author matches, and
/// still counts it once.
#[test]
fn filter_across_the_bridge_counts_each_row_once() {
    assert_eq!(
        run(
            &bridge_def("b"),
            &[],
            &["total_pages", "book_count"],
            Some("country = 'FR'")
        ),
        table_of(&[&["300", "2"]])
    );
}

/// A filter both of whose values a book matches still counts it once: Mixed
/// has a FR and a UK author.
#[test]
fn filter_matching_two_bridge_rows_of_one_book_counts_it_once() {
    assert_eq!(
        run(
            &bridge_def("b"),
            &[],
            &["total_pages", "book_count"],
            Some("country IN ('FR', 'UK')")
        ),
        table_of(&[&["600", "3"]])
    );
}

/// A filter OR-ing a member on the metric's own table with one across the
/// bridge is evaluated row by row: Duo by its title, Solo and Mixed by Cleo.
#[test]
fn filter_mixing_both_sides_of_the_bridge() {
    assert_eq!(
        run(
            &bridge_def("b"),
            &[],
            &["author_count"],
            Some("title = 'Duo' OR country = 'UK'")
        ),
        table_of(&[&["3"]])
    );
}

/// B6: the other direction, a metric on the authors (here the base table) by
/// a book dimension. Dan, with no book, is in the NULL group.
#[test]
fn metric_on_the_base_table_across_the_bridge() {
    assert_eq!(
        run(&bridge_def("a"), &["title"], &["author_count"], None),
        table_of(&[
            &["Duo", "2"],
            &["Mixed", "2"],
            &["Solo", "1"],
            &["NULL", "1"],
        ])
    );
}

/// B7: a dimension on the bridge itself groups each book once per role.
#[test]
fn dimension_on_the_bridge_itself() {
    assert_eq!(
        run(&bridge_def("b"), &["role"], &["total_pages"], None),
        table_of(&[&["co", "300"], &["lead", "600"], &["NULL", "50"]])
    );
}

/// B7 with two bridged dimensions: one per (role, country) pair.
#[test]
fn two_dimensions_across_the_bridge() {
    assert_eq!(
        run(
            &bridge_def("b"),
            &["role", "country"],
            &["total_pages"],
            None
        ),
        table_of(&[
            &["co", "FR", "100"],
            &["co", "UK", "200"],
            &["lead", "FR", "300"],
            &["lead", "UK", "300"],
            &["NULL", "NULL", "50"],
        ])
    );
}

/// A dimension on the metric's own table beside a bridged one.
#[test]
fn own_table_dimension_beside_a_bridged_one() {
    assert_eq!(
        run(
            &bridge_def("a"),
            &["country", "title"],
            &["total_pages"],
            None
        ),
        table_of(&[
            &["FR", "Duo", "100"],
            &["FR", "Mixed", "200"],
            &["UK", "Mixed", "200"],
            &["UK", "Solo", "300"],
            &["NULL", "Orphan", "50"],
        ])
    );
}

/// B9: a hop past the bridge, still once per group.
#[test]
fn dimension_past_the_bridge() {
    assert_eq!(
        run(
            &bridge_def("b"),
            &["continent"],
            &["total_pages", "book_count"],
            None
        ),
        table_of(&[&["Europe", "600", "3"], &["NULL", "50", "1"]])
    );
}

/// Metrics on both sides of the bridge, each at its own grain, outer-merged:
/// DE has an author and no pages, the NULL group pages and no author.
#[test]
fn metrics_on_both_sides_of_the_bridge() {
    assert_eq!(
        run(
            &bridge_def("b"),
            &["country"],
            &["total_pages", "author_count"],
            None
        ),
        table_of(&[
            &["DE", "NULL", "1"],
            &["FR", "300", "2"],
            &["UK", "500", "1"],
            &["NULL", "50", "NULL"],
        ])
    );
}

/// A derived metric across the bridge decomposes into its two grains.
#[test]
fn derived_metric_across_the_bridge() {
    assert_eq!(
        run(&bridge_def("b"), &["country"], &["pages_per_author"], None),
        table_of(&[
            &["DE", "NULL"],
            &["FR", "150.0"],
            &["UK", "500.0"],
            &["NULL", "NULL"],
        ])
    );
}

/// B10: a fact table as the bridge, with a metric on it co-queried. Cleo
/// bought nothing, so the NULL group has a customer and no revenue.
#[test]
fn fact_table_as_the_bridge() {
    assert_eq!(
        run(
            &fact_bridge_def(),
            &["category"],
            &["customer_count", "revenue"],
            None
        ),
        table_of(&[
            &["book", "2", "70"],
            &["toy", "1", "30"],
            &["NULL", "1", "NULL"],
        ])
    );
}

/// A bridged group deduplicates its anchor rows by the anchor's PRIMARY KEY.
/// A UNIQUE key can't stand in for it: it may hold any number of NULLs, and
/// rows that share a NULL key would each join every NULL-key pair (code review
/// on PR #339). Here Solo and Orphan both have a NULL isbn, so keyed by isbn
/// each title would report 350 pages instead of its own. The query is refused,
/// and the message names the missing PRIMARY KEY.
#[test]
fn bridge_without_a_primary_key_is_refused() {
    let mut def = bridge_def("a");
    let books = def.tables.iter_mut().find(|t| t.alias == "b").expect("b");
    books.pk_columns.clear();
    books.unique_constraints = vec![vec!["isbn".to_string()]];
    books.definition = Some(
        "SELECT book_id, title, pages, \
         CASE WHEN book_id IN (12, 13) THEN NULL ELSE isbn END AS isbn FROM books"
            .to_string(),
    );
    def.joins[1].ref_columns = vec!["isbn".to_string()];
    def.joins[1].fk_columns = vec!["isbn".to_string()];
    // book_authors has no isbn column, so key the bridge rows through a
    // query-defined table that adds it.
    let ba = def.tables.iter_mut().find(|t| t.alias == "ba").expect("ba");
    ba.definition = Some("SELECT x.*, 'i' || x.book_id AS isbn FROM book_authors AS x".to_string());
    let e = refusal(&def, &["title", "country"], &["total_pages"]);
    assert!(
        matches!(&e, ExpandError::FanTrap { detail } if detail.counts_once),
        "{e}"
    );
    assert!(e.to_string().contains("PRIMARY KEY"), "{e}");
}

/// A dimension on a plain child, a table whose only relationship is to the
/// metric's, fans the metric's rows out: each book counts once per review
/// rating, as Snowflake counts each order once per note kind (fixture 7, N1).
/// Duo has reviews of 5 and 3 stars, Mixed one of 4; Solo and Orphan none.
#[test]
fn dimension_on_a_plain_child_counts_once_per_group() {
    for first in ["b", "a", "ba", "r"] {
        assert_eq!(
            run(&bridge_def(first), &["stars"], &["total_pages"], None),
            table_of(&[
                &["3", "100"],
                &["4", "200"],
                &["5", "100"],
                &["NULL", "350"]
            ]),
            "base table {first}"
        );
    }
}

/// Two fan-outs on one path (authors -> bridge -> books -> reviews) count each
/// author once per rating (fixture 7, N4): Ana wrote Duo (5, 3) and Mixed (4),
/// Ben Duo, Cleo Mixed and Solo (no review), Dan nothing.
#[test]
fn two_fan_outs_count_once_per_group() {
    assert_eq!(
        run(&bridge_def("a"), &["stars"], &["author_count"], None),
        table_of(&[&["3", "2"], &["4", "2"], &["5", "2"], &["NULL", "2"]])
    );
}

/// A fan-out after a hop up (reviews -> books <- bridge -> authors) counts each
/// review once per country, as Snowflake does (fixture 3's H1): reviews 1 and
/// 2 are of Duo (two French authors), review 3 of Mixed (French and British).
#[test]
fn fan_out_after_a_hop_up_counts_once_per_group() {
    for first in ["b", "a", "ba", "r"] {
        assert_eq!(
            run(&bridge_def(first), &["country"], &["review_count"], None),
            table_of(&[&["FR", "3"], &["UK", "1"]]),
            "base table {first}"
        );
    }
}

/// After a hop up the anchor may declare no PRIMARY KEY to deduplicate its
/// rows by; the query is then refused, and the message says a bridge is the
/// reason and asks for the key.
#[test]
fn fan_out_after_a_hop_up_without_a_primary_key_is_refused() {
    let mut def = bridge_def("b");
    let reviews = def.tables.iter_mut().find(|t| t.alias == "r").expect("r");
    reviews.pk_columns.clear();
    def.metrics
        .push(metric("total_stars", "SUM(r.stars)", Some("r")));
    let e = refusal(&def, &["country"], &["total_stars"]);
    assert!(
        matches!(&e, ExpandError::FanTrap { detail } if detail.counts_once),
        "{e}"
    );
    assert!(e.to_string().contains("PRIMARY KEY"), "{e}");
}

/// After a hop up, a fan-out into a plain sibling child counts each row once
/// per group too (Snowflake's fixture 6, K19: items by a note on their order).
/// Reviews 1 and 2 are of Duo (hardback and paperback), review 3 of Mixed
/// (hardback).
#[test]
fn reviews_by_a_sibling_child_after_a_hop_up_count_once_per_group() {
    for first in ["b", "a", "ba", "r", "e"] {
        assert_eq!(
            run(&sibling_def(first), &["format"], &["review_count"], None),
            table_of(&[&["hard", "3"], &["soft", "2"]]),
            "base table {first}"
        );
    }
}

/// Filtered on a sibling child after a hop up: the reviews of a book with a
/// paperback, each once.
#[test]
fn filter_on_a_sibling_child_after_a_hop_up_counts_each_row_once() {
    assert_eq!(
        run(
            &sibling_def("b"),
            &[],
            &["review_count"],
            Some("format = 'soft'")
        ),
        table_of(&[&["2"]])
    );
}

/// The other direction, editions by a review column: Duo's two editions under
/// stars 5 and 3, Mixed's under 4, and Solo's (no review) in the NULL group.
#[test]
fn editions_by_a_review_column_count_once_per_group() {
    assert_eq!(
        run(&sibling_def("e"), &["stars"], &["edition_count"], None),
        table_of(&[&["3", "2"], &["4", "1"], &["5", "2"], &["NULL", "1"]])
    );
}

/// A sibling child and a table across the bridge in one query: each review
/// once per (format, country).
#[test]
fn sibling_child_beside_the_bridge_after_a_hop_up() {
    assert_eq!(
        run(
            &sibling_def("r"),
            &["format", "country"],
            &["review_count"],
            None
        ),
        table_of(&[
            &["hard", "FR", "3"],
            &["hard", "UK", "1"],
            &["soft", "FR", "2"]
        ])
    );
}

/// With no PRIMARY KEY on the metric's table there is nothing to deduplicate
/// by, and the query is refused, asking for the key (Snowflake's 010933 in
/// fixture 6, K14).
#[test]
fn sibling_child_after_a_hop_up_without_a_primary_key_is_refused() {
    let mut def = sibling_def("b");
    let reviews = def.tables.iter_mut().find(|t| t.alias == "r").expect("r");
    reviews.pk_columns.clear();
    def.metrics
        .push(metric("total_stars", "SUM(r.stars)", Some("r")));
    let e = refusal(&def, &["format"], &["total_stars"]);
    assert!(
        matches!(&e, ExpandError::FanTrap { detail } if detail.counts_once),
        "{e}"
    );
    assert!(e.to_string().contains("PRIMARY KEY"), "{e}");
}

/// A semi-additive metric across the bridge is refused, and the message says a
/// bridge is the reason.
#[test]
fn semi_additive_metric_across_the_bridge_is_refused() {
    let mut def = bridge_def("b");
    let mut last = metric("last_pages", "SUM(b.pages)", Some("b"));
    last.non_additive_by = vec![NonAdditiveDim {
        dimension: "title".to_string(),
        ..Default::default()
    }];
    def.metrics.push(last);
    let e = refusal(&def, &["country"], &["last_pages"]);
    assert!(
        matches!(&e, ExpandError::FanTrap { detail } if detail.counts_once),
        "{e}"
    );
    assert!(e.to_string().contains("counted once per group only"), "{e}");
}

/// A window metric across the bridge runs over the rows its inner metric
/// gives by country (B1: FR 300, UK 500, the authorless Orphan 50), as
/// Snowflake does (fixture 4, W4).
#[test]
fn window_metric_across_the_bridge_reads_each_group_once() {
    for first in ["b", "a", "ba"] {
        let mut def = bridge_def(first);
        def.metrics.push(Metric {
            name: "avg_pages".to_string(),
            expr: "AVG(total_pages)".to_string(),
            source_table: Some("b".to_string()),
            window_spec: Some(crate::model::WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "total_pages".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["country".to_string()],
                order_by: vec![],
                frame_clause: None,
            }),
            ..Default::default()
        });
        assert_eq!(
            run(&def, &["country"], &["avg_pages"], None),
            table_of(&[&["FR", "300.0"], &["UK", "500.0"], &["NULL", "50.0"]]),
            "{first} first"
        );
    }
}

/// A window metric across the bridge after a hop up reads the rows its inner
/// metric gives: Duo's two reviews and Mixed's one count for FR, Mixed's for
/// UK. A semi-additive metric there is still refused, as Snowflake refuses it
/// (fixture 3, S7).
#[test]
fn window_across_the_bridge_after_a_hop_up_reads_its_inner_rows() {
    let mut def = bridge_def("b");
    def.metrics.push(Metric {
        name: "avg_reviews".to_string(),
        expr: "AVG(review_count)".to_string(),
        source_table: Some("r".to_string()),
        window_spec: Some(crate::model::WindowSpec {
            window_function: "AVG".to_string(),
            inner_metric: "review_count".to_string(),
            extra_args: vec![],
            excluding_dims: vec![],
            partition_dims: vec!["country".to_string()],
            order_by: vec![],
            frame_clause: None,
        }),
        ..Default::default()
    });
    assert_eq!(
        run(&def, &["country"], &["review_count", "avg_reviews"], None),
        table_of(&[&["FR", "3", "3.0"], &["UK", "1", "1.0"]]),
    );
}

/// The semi-additive half of the case above stays refused (fixture 3, S7).
#[test]
fn semi_additive_across_the_bridge_after_a_hop_up_is_refused() {
    let mut def = bridge_def("b");
    let mut last = metric("last_stars", "SUM(r.stars)", Some("r"));
    last.non_additive_by = vec![NonAdditiveDim {
        dimension: "stars".to_string(),
        ..Default::default()
    }];
    def.metrics.push(last);
    let e = refusal(&def, &["country"], &["last_stars"]);
    assert!(
        matches!(&e, ExpandError::FanTrap { detail } if detail.counts_once),
        "{e}"
    );
}

/// A plain child and a table across the bridge in one query: each book once
/// per (country, rating). Solo (UK) has no review, Orphan neither an author
/// nor a review.
#[test]
fn plain_child_beside_the_bridge_counts_once_per_group() {
    assert_eq!(
        run(
            &bridge_def("r"),
            &["country", "stars"],
            &["total_pages"],
            None
        ),
        table_of(&[
            &["FR", "3", "100"],
            &["FR", "4", "200"],
            &["FR", "5", "100"],
            &["UK", "4", "200"],
            &["UK", "NULL", "300"],
            &["NULL", "NULL", "50"],
        ])
    );
}

/// A dimension's output cast is applied before the bridge's dedup, so two
/// raw values that cast to one value still count the book once (code review on
/// PR #339). Each author id casts to `true`: Duo and Mixed each have two
/// authors, and would count twice if the cast came after the DISTINCT.
#[test]
fn output_cast_applies_before_the_dedup() {
    let mut def = bridge_def("b");
    let mut has = dim("has_author", "a.author_id", "a");
    has.output_type = Some("BOOLEAN".to_string());
    def.dimensions.push(has);
    assert_eq!(
        run(&def, &["has_author"], &["total_pages"], None),
        table_of(&[&["true", "600"], &["NULL", "50"]])
    );
}
