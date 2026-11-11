//! Queries on a view whose relationships reach one table by two paths through
//! different tables: a diamond. Orders reach regions through customers and
//! through stores.
//!
//! Snowflake (the 2026-11-10 fixture 3, cases D2-D13) refuses a query that
//! would have to pick one of the two paths itself, with 010246 "Multi-path
//! relationship between the dimension entity 'R' and the base metric or
//! dimension entity 'O' is not supported". A metric whose `USING` names the
//! first relationship of one path is answered through that path, and a query
//! that stays on one path is answered as usual. We used to follow the
//! first-declared path silently.
//!
//! Each test asks its question of both TABLES orders. They pin ROWS: the
//! generated SQL runs against an in-memory DuckDB, one test per case so each
//! reports on its own.

use super::*;
use crate::model::{Cardinality, Dimension, Fact, Join, Metric, SemanticViewDefinition, TableRef};

/// o1: Ana (EU) at store S-US, 10; o2: Bo (US) at S-US, 20; o3: Ana at S-EU,
/// 40. By customer region: EU 50, US 20. By store region: EU 40, US 30. Line
/// items: o1 has SKUs a and b, o3 SKU a, o2 none.
fn db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE regions (region_id INT, region_name VARCHAR);
         CREATE TABLE customers (cust_id INT, cust_name VARCHAR, region_id INT);
         CREATE TABLE stores (store_id INT, store_name VARCHAR, region_id INT);
         CREATE TABLE orders (order_id INT, cust_id INT, store_id INT, amount INT);
         INSERT INTO regions VALUES (1, 'EU'), (2, 'US');
         INSERT INTO customers VALUES (1, 'Ana', 1), (2, 'Bo', 2);
         INSERT INTO stores VALUES (1, 'S-US', 2), (2, 'S-EU', 1);
         INSERT INTO orders VALUES (1, 1, 1, 10), (2, 2, 1, 20), (3, 1, 2, 40);
         CREATE TABLE line_items (line_id INT, order_id INT, sku VARCHAR);
         INSERT INTO line_items VALUES (1, 1, 'a'), (2, 1, 'b'), (3, 3, 'a');",
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

fn dim(name: &str, expr: &str, table: &str) -> Dimension {
    Dimension {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(table.to_string()),
        ..Default::default()
    }
}

fn metric(name: &str, expr: &str, table: Option<&str>, using: &[&str]) -> Metric {
    Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: table.map(str::to_string),
        using_relationships: using.iter().map(|r| (*r).to_string()).collect(),
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

/// The fixture's diamond view, with its tables in the order `order` names
/// them: `"rcso"` is dv_r (regions first) and `"ocsr"` dv_o.
fn diamond(order: &str) -> SemanticViewDefinition {
    let tables = order
        .chars()
        .map(|c| match c {
            'o' => table("o", "orders", "order_id"),
            'c' => table("c", "customers", "cust_id"),
            's' => table("s", "stores", "store_id"),
            'r' => table("r", "regions", "region_id"),
            other => panic!("no table {other}"),
        })
        .collect();
    SemanticViewDefinition {
        tables,
        joins: vec![
            rel("o_cust", "o", "c", "cust_id", "cust_id"),
            rel("o_store", "o", "s", "store_id", "store_id"),
            rel("c_region", "c", "r", "region_id", "region_id"),
            rel("s_region", "s", "r", "region_id", "region_id"),
        ],
        dimensions: vec![
            dim("order_id", "o.order_id", "o"),
            dim("cust_name", "c.cust_name", "c"),
            dim("store_name", "s.store_name", "s"),
            dim("region_name", "r.region_name", "r"),
        ],
        metrics: vec![
            metric("revenue", "SUM(o.amount)", Some("o"), &[]),
            metric("rev_by_cust", "SUM(o.amount)", Some("o"), &["o_cust"]),
            metric("rev_by_store", "SUM(o.amount)", Some("o"), &["o_store"]),
            metric("customer_count", "COUNT(c.cust_id)", Some("c"), &[]),
            metric("both_paths", "rev_by_cust + rev_by_store", None, &[]),
        ],
        facts: vec![Fact {
            name: "amount".to_string(),
            expr: "o.amount".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Every TABLES order of the four tables. CREATE accepts each (Snowflake does),
/// and each must give the same answer.
const ORDERS: [&str; 24] = [
    "ocsr", "ocrs", "oscr", "osrc", "orcs", "orsc", "cosr", "cors", "csor", "csro", "cros", "crso",
    "socr", "sorc", "scor", "scro", "sroc", "srco", "rocs", "rosc", "rcos", "rcso", "rsoc", "rsco",
];

fn req(dims: &[&str], mets: &[&str], where_clause: Option<&str>) -> QueryRequest {
    QueryRequest {
        where_clause: where_clause.map(str::to_string),
        // PIN: no case here queries facts; `facts_query` builds its own.
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

/// The rows the query gives on the view in table order `order`.
fn answer(order: &str, request: &QueryRequest) -> Vec<Vec<String>> {
    let sql = expand("dv", &diamond(order), request)
        .unwrap_or_else(|e| panic!("TABLES order {order}: {e}"));
    rows(&sql)
}

/// Assert the query is refused on every table order as a query with two
/// paths, naming both.
fn assert_multi_path(request: &QueryRequest) {
    for order in ORDERS {
        match expand("dv", &diamond(order), request) {
            Ok(sql) => panic!(
                "TABLES order {order}: expected a multi-path refusal, got rows {:?}\nSQL:\n{sql}",
                rows(&sql)
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("more than one path")
                        && msg.contains("o -> c -> r")
                        && msg.contains("o -> s -> r"),
                    "TABLES order {order}: expected a multi-path refusal naming both paths, \
                     got: {msg}"
                );
            }
        }
    }
}

fn assert_answer(request: &QueryRequest, expected: &[&[&str]]) {
    for order in ORDERS {
        assert_eq!(
            answer(order, request),
            table_of(expected),
            "TABLES order {order}"
        );
    }
}

/// D2 / D12: revenue by region with no `USING` has two paths to regions.
#[test]
fn d12_metric_by_a_dimension_two_paths_away_is_refused() {
    assert_multi_path(&req(&["region_name"], &["revenue"], None));
}

/// D10: orders and regions, no metric: each order reaches a region twice.
#[test]
fn d10_dims_only_across_two_paths_is_refused() {
    assert_multi_path(&req(&["order_id", "region_name"], &[], None));
}

/// D11: a filter on regions needs a path to them as much as a dimension does.
#[test]
fn d11_filter_two_paths_away_is_refused() {
    assert_multi_path(&req(&[], &["revenue"], Some("r.region_name = 'EU'")));
}

/// A FACTS query reads the orders' rows, so it needs one path to regions too.
#[test]
fn facts_query_across_two_paths_is_refused() {
    let request = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("amount"), FactName::new("region_name")],
        dimensions: vec![],
        metrics: vec![],
    };
    assert_multi_path(&request);
}

/// A metric beside the multi-path one does not make it answerable: each
/// metric needs its own path, as with a role-played table.
#[test]
fn metric_without_using_beside_one_with_is_refused() {
    assert_multi_path(&req(&["region_name"], &["rev_by_cust", "revenue"], None));
}

/// D3 / D13: `USING (o_cust)` follows the customer path.
#[test]
fn d13_using_the_customer_path() {
    assert_answer(
        &req(&["region_name"], &["rev_by_cust"], None),
        &[&["EU", "50"], &["US", "20"]],
    );
}

/// D4: `USING (o_store)` follows the store path.
#[test]
fn d4_using_the_store_path() {
    assert_answer(
        &req(&["region_name"], &["rev_by_store"], None),
        &[&["EU", "40"], &["US", "30"]],
    );
}

/// D5: both paths in one query, each metric through its own.
#[test]
fn d5_both_paths_in_one_query() {
    assert_answer(
        &req(&["region_name"], &["rev_by_cust", "rev_by_store"], None),
        &[&["EU", "50", "40"], &["US", "20", "30"]],
    );
}

/// A derived metric over both: each component through its own path.
#[test]
fn derived_metric_over_both_paths() {
    assert_answer(
        &req(&["region_name"], &["both_paths"], None),
        &[&["EU", "90"], &["US", "50"]],
    );
}

/// A `USING` metric filtered on regions follows its path for the filter too.
#[test]
fn using_metric_filtered_on_its_path() {
    assert_answer(
        &req(&[], &["rev_by_store"], Some("r.region_name = 'EU'")),
        &[&["40"]],
    );
}

/// D6: customers are one hop from orders, one path.
#[test]
fn d6_one_path_by_customer() {
    assert_answer(
        &req(&["cust_name"], &["revenue"], None),
        &[&["Ana", "50"], &["Bo", "20"]],
    );
}

/// D7: a customer metric by region: customers reach regions one way.
#[test]
fn d7_customer_metric_by_region() {
    assert_answer(
        &req(&["region_name"], &["customer_count"], None),
        &[&["EU", "1"], &["US", "1"]],
    );
}

/// D8: regions alone.
#[test]
fn d8_dims_only_regions() {
    assert_answer(&req(&["region_name"], &[], None), &[&["EU"], &["US"]]);
}

/// D9: customers and their regions, one path from customers.
#[test]
fn d9_dims_only_customers_and_regions() {
    assert_answer(
        &req(&["cust_name", "region_name"], &[], None),
        &[&["Ana", "EU"], &["Bo", "US"]],
    );
}

/// Revenue by customer and store: each one hop from orders.
#[test]
fn both_middle_tables_are_one_path_each() {
    assert_answer(
        &req(&["cust_name", "store_name"], &["revenue"], None),
        &[
            &["Ana", "S-EU", "40"],
            &["Ana", "S-US", "10"],
            &["Bo", "S-US", "20"],
        ],
    );
}

/// A window metric over `rev_by_store` reads the rows `rev_by_store` gives
/// by region, along the store path: EU 40, US 30, summed over every row
/// (TECH-DEBT #109).
#[test]
fn window_over_a_pinned_path_follows_it() {
    for order in ORDERS {
        let mut def = diamond(order);
        def.metrics.push(Metric {
            name: "store_window".to_string(),
            expr: "SUM(rev_by_store)".to_string(),
            source_table: Some("o".to_string()),
            window_spec: Some(crate::model::WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "rev_by_store".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec![],
                order_by: vec![],
                frame_clause: None,
            }),
            ..Default::default()
        });
        let request = req(&["region_name"], &["rev_by_store", "store_window"], None);
        let sql =
            expand("dv", &def, &request).unwrap_or_else(|e| panic!("TABLES order {order}: {e}"));
        assert_eq!(
            rows(&sql),
            table_of(&[&["EU", "40", "70"], &["US", "30", "70"]]),
            "TABLES order {order}"
        );
    }
}

/// `diamond(order)` plus `last_by_store`, a semi-additive `rev_by_store`
/// (the latest order's amount, along the store path), and `label`, a
/// dimension declared on no table.
fn diamond_with_unfollowed_shapes(order: &str) -> SemanticViewDefinition {
    let mut def = diamond(order);
    let mut last = metric("last_by_store", "SUM(o.amount)", Some("o"), &["o_store"]);
    last.non_additive_by = vec![crate::model::NonAdditiveDim {
        dimension: "order_id".to_string(),
        order: crate::model::SortOrder::Desc,
        ..Default::default()
    }];
    def.metrics.push(last);
    def.dimensions.push(Dimension {
        name: "label".to_string(),
        expr: "'all'".to_string(),
        source_table: None,
        ..Default::default()
    });
    def
}

/// Assert the query is refused on every table order because it cannot follow
/// the path a metric's `USING` picks.
fn assert_unfollowed(request: &QueryRequest) {
    for order in ORDERS {
        match expand("dv", &diamond_with_unfollowed_shapes(order), request) {
            Ok(sql) => panic!(
                "TABLES order {order}: expected the USING path to be refused as unfollowed, \
                 got rows {:?}\nSQL:\n{sql}",
                rows(&sql)
            ),
            Err(err) => assert!(
                matches!(err, ExpandError::UnfollowedUsingPath { .. }),
                "TABLES order {order}: got {err}"
            ),
        }
    }
}

/// An active semi-additive metric alone on a pinned path: the planner leaves a
/// lone snapshot group to the snapshot CTE, which joins along the usual walk
/// (through customers), so the query is refused (TECH-DEBT #109).
#[test]
fn semi_additive_alone_on_a_pinned_path_is_refused() {
    assert_unfollowed(&req(&["region_name"], &["last_by_store"], None));
}

/// The same metric beside one on the other path: two groups, and the snapshot
/// group with a pinned path is declined, so the query is refused
/// (TECH-DEBT #109).
#[test]
fn semi_additive_beside_another_path_is_refused() {
    assert_unfollowed(&req(
        &["region_name"],
        &["rev_by_cust", "last_by_store"],
        None,
    ));
}

/// A dimension declared on no table cannot be placed under a grain, so the
/// planner declines, and the query is refused rather than joined through
/// customers (TECH-DEBT #109).
#[test]
fn member_on_no_table_beside_a_pinned_path_is_refused() {
    assert_unfollowed(&req(&["region_name", "label"], &["rev_by_store"], None));
}

/// The same member in a filter.
#[test]
fn filter_on_no_table_beside_a_pinned_path_is_refused() {
    assert_unfollowed(&req(
        &["region_name"],
        &["rev_by_store"],
        Some("label = 'all'"),
    ));
}

/// A dimension below the metric's grain beside a pinned path: each order counts
/// once per (store region, SKU), whichever TABLES order (Phase 7 PR 4c), and
/// filtered on a SKU each matching order once.
#[test]
fn fan_out_beside_a_pinned_path_counts_once_per_group() {
    for order in ORDERS {
        let mut def = diamond(order);
        def.tables.push(table("li", "line_items", "line_id"));
        def.joins
            .push(rel("li_order", "li", "o", "order_id", "order_id"));
        def.dimensions.push(dim("sku", "li.sku", "li"));
        let run = |dims: &[&str], filter: Option<&str>| {
            let sql = expand("dv", &def, &req(dims, &["rev_by_store"], filter))
                .unwrap_or_else(|e| panic!("TABLES order {order}: {e}"));
            rows(&sql)
        };
        assert_eq!(
            run(&["region_name", "sku"], None),
            table_of(&[
                &["EU", "a", "40"],
                &["US", "a", "10"],
                &["US", "b", "10"],
                &["US", "NULL", "20"],
            ]),
            "TABLES order {order}"
        );
        assert_eq!(
            run(&["region_name"], Some("sku = 'a'")),
            table_of(&[&["EU", "40"], &["US", "10"]]),
            "TABLES order {order}"
        );
    }
}

/// A query with no metrics whose rows cannot be placed, because a member is
/// declared on no table, is still refused when two of its members are joined
/// by two paths: the base-anchored emission would join one of them.
#[test]
fn dims_only_with_a_member_on_no_table_is_refused() {
    for order in ORDERS {
        let err = expand(
            "dv",
            &diamond_with_unfollowed_shapes(order),
            &req(&["order_id", "region_name", "label"], &[], None),
        )
        .expect_err("two paths from orders to regions");
        assert!(
            matches!(err, ExpandError::MultiPath { .. }) && err.to_string().contains("o -> s -> r"),
            "TABLES order {order}: got {err}"
        );
    }
}

/// The same with the member in a filter.
#[test]
fn dims_only_filtered_on_no_table_is_refused() {
    for order in ORDERS {
        let err = expand(
            "dv",
            &diamond_with_unfollowed_shapes(order),
            &req(&["order_id", "region_name"], &[], Some("label = 'all'")),
        )
        .expect_err("two paths from orders to regions");
        assert!(
            matches!(err, ExpandError::MultiPath { .. }) && err.to_string().contains("o -> s -> r"),
            "TABLES order {order}: got {err}"
        );
    }
}

/// And a FACTS query beside such a member.
#[test]
fn facts_with_a_member_on_no_table_is_refused() {
    for order in ORDERS {
        let request = QueryRequest {
            where_clause: None,
            facts: vec![
                FactName::new("amount"),
                FactName::new("region_name"),
                FactName::new("label"),
            ],
            dimensions: vec![],
            metrics: vec![],
        };
        let err = expand("dv", &diamond_with_unfollowed_shapes(order), &request)
            .expect_err("two paths from orders to regions");
        assert!(
            matches!(err, ExpandError::MultiPath { .. }) && err.to_string().contains("o -> s -> r"),
            "TABLES order {order}: got {err}"
        );
    }
}

/// A `USING` metric grouped by the table its relationship names: one path, so
/// the `USING` chooses nothing, and the table is joined as usual
/// (TECH-DEBT #81 used to leave it unbindable).
#[test]
fn using_metric_by_the_table_its_relationship_names() {
    assert_answer(
        &req(&["cust_name"], &["rev_by_cust"], None),
        &[&["Ana", "50"], &["Bo", "20"]],
    );
}

/// The same through the other relationship.
#[test]
fn using_metric_by_the_other_middle_table() {
    assert_answer(
        &req(&["store_name"], &["rev_by_store"], None),
        &[&["S-EU", "40"], &["S-US", "30"]],
    );
}

/// The store path to regions beside a dimension on the customer side.
#[test]
fn store_path_region_beside_customer() {
    assert_answer(
        &req(&["cust_name", "region_name"], &["rev_by_store"], None),
        &[
            &["Ana", "EU", "40"],
            &["Ana", "US", "10"],
            &["Bo", "US", "20"],
        ],
    );
}

/// The store path to regions beside the store it goes through.
#[test]
fn store_path_region_beside_its_store() {
    assert_answer(
        &req(&["store_name", "region_name"], &["rev_by_store"], None),
        &[&["S-EU", "EU", "40"], &["S-US", "US", "30"]],
    );
}
