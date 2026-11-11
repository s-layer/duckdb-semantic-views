//! COUNT(*) -> COUNT(pk) rewrite behaviour.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::count_star_rewrite_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::test_helpers::{orders_view, TestFixtureExt};
use crate::model::WindowSpec;

/// `orders` (base) + `line_items` child with a declared PK.
fn child_count_def() -> crate::model::SemanticViewDefinition {
    orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_dimension("region", "region", None)
        .with_table("li", "line_items", &["id"])
        .with_metric("item_count", "COUNT(*)", Some("li"))
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"])
}

/// A request that still takes the BASE-ANCHORED path for a child metric: the
/// dimension `region` is unqualified (no source table, as only YAML / legacy
/// JSON can declare it), and an unqualified expression would rebind if the FROM
/// moved, so the query is not re-anchored at the metric's home table
/// (TECH-DEBT #71 residual, TECH-DEBT #82). This is where the SG-8 no-PK error
/// is still reachable, so the error tests below use it; every QUALIFIED
/// request is anchored at `li` itself, where `COUNT(*)` needs no key.
fn fallback_req() -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("item_count")],
    }
}

#[test]
fn test_child_count_star_rewritten_exact_sql() {
    // TECH-DEBT #71: a child metric is anchored at its OWN table, so there is
    // no NULL-extended row to fence and the query is a plain scan. The SG-8
    // rewrite to `COUNT(<pk>)` still happens (it runs at resolution, before the
    // anchor is chosen) and is neutral here: a declared PK is non-NULL.
    let def = child_count_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("item_count")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    let expected = "\
SELECT
    COUNT(\"li\".\"id\") AS \"item_count\"
FROM \"line_items\" AS \"li\"";
    assert_eq!(sql, expected);
}

#[test]
fn test_child_count_star_rewritten_with_base_dimension() {
    let def = child_count_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("item_count")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        sql.contains("COUNT(\"li\".\"id\") AS \"item_count\""),
        "child COUNT(*) must be rewritten to COUNT(pk): {sql}"
    );
    assert!(sql.contains("GROUP BY"), "grouped query expected: {sql}");
}

#[test]
fn test_base_table_count_star_unchanged() {
    // Metrics on the base table keep plain COUNT(*): the base table
    // is never NULL-extended by the synthesized LEFT JOINs.
    let def = child_count_def().with_metric("order_count", "COUNT(*)", Some("orders"));
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("order_count")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    let expected = "\
SELECT
    COUNT(*) AS \"order_count\"
FROM \"orders\" AS \"orders\"";
    assert_eq!(sql, expected);
}

#[test]
fn test_unqualified_count_star_metric_unchanged() {
    // Legacy single-table shape: metric declared without a source
    // table (None) is a base-table/derived metric — no rewrite.
    let def = orders_view();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("order_count")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        sql.contains("count(*) AS \"order_count\""),
        "COUNT(*) without a non-base source table must be preserved: {sql}"
    );
}

#[test]
fn test_child_count_star_without_pk_errors() {
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &[]) // no PK declared
        .with_dimension("region", "region", None)
        .with_metric("item_count", "COUNT(*)", Some("li"))
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"]);
    // Anchored at `li` itself (TECH-DEBT #71) there is no NULL-extended row,
    // so a qualified request needs no PK and counts every child row.
    let sql = expand("orders", &def, &count_req()).expect("home-anchored COUNT(*) needs no PK");
    assert_eq!(
        sql,
        "SELECT\n    COUNT(*) AS \"item_count\"\nFROM \"line_items\" AS \"li\""
    );
    // On the base-anchored fallback the rewrite is still impossible.
    let err = expand("orders", &def, &fallback_req()).unwrap_err();
    match &err {
        ExpandError::CountStarRequiresPrimaryKey {
            view_name,
            metric_name,
            table_alias,
        } => {
            assert_eq!(view_name, "orders");
            assert_eq!(metric_name, "item_count");
            assert_eq!(table_alias, "li");
        }
        other => panic!("Expected CountStarRequiresPrimaryKey, got: {other}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains("no PRIMARY KEY declared") && msg.contains("COUNT(*)"),
        "error must explain the rewrite requirement: {msg}"
    );
}

#[test]
fn test_unrelated_metric_still_works_when_sibling_count_star_lacks_pk() {
    // The no-PK failure is scoped to queries that actually use the
    // metric: other metrics on the same view keep working.
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &[]) // no PK declared
        .with_metric("item_count", "COUNT(*)", Some("li"))
        .with_metric("revenue", "SUM(li.amount)", Some("li"))
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("revenue")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(sql.contains("SUM(li.amount)"), "SQL: {sql}");
}

#[test]
fn test_derived_metric_reaching_no_pk_count_star_errors() {
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &[]) // no PK declared
        .with_dimension("region", "region", None)
        .with_metric("item_count", "COUNT(*)", Some("li"))
        .with_metric("double_items", "item_count * 2", None)
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"]);
    // The base-anchored fallback (see `fallback_req`): the unqualified
    // dimension keeps the query off the home-anchored path.
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("double_items")],
    };
    let err = expand("orders", &def, &req).unwrap_err();
    match &err {
        ExpandError::CountStarRequiresPrimaryKey { metric_name, .. } => {
            assert_eq!(
                metric_name, "item_count",
                "error must name the failing base metric"
            );
        }
        other => panic!("Expected CountStarRequiresPrimaryKey, got: {other}"),
    }
}

#[test]
fn test_derived_metric_inherits_rewritten_count_star() {
    let def = child_count_def().with_metric("double_items", "item_count * 2", None);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("double_items")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        sql.contains("(COUNT(\"li\".\"id\")) * 2 AS \"double_items\""),
        "derived metric must inline the REWRITTEN child count: {sql}"
    );
}

#[test]
fn test_window_inner_aggregate_gets_rewrite() {
    // Window path: the inner aggregate is emitted from the shared
    // resolved expressions, so the rewrite must appear in the CTE.
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &["id"])
        .with_dimension("product", "li.product", Some("li"))
        .with_metric("item_count", "COUNT(*)", Some("li"))
        .with_metric("rolling_items", "AVG(item_count)", None)
        .with_window_spec(
            "rolling_items",
            WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "item_count".to_string(),
                ..Default::default()
            },
        )
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("product")],
        metrics: vec![MetricName::new("rolling_items")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        sql.contains("COUNT(\"li\".\"id\") AS \"item_count\""),
        "window CTE inner aggregate must use the rewritten count: {sql}"
    );
}

#[test]
fn test_window_inner_aggregate_quoted_name_gets_rewrite() {
    // EXP-6 (code-review 2026-07-18): a base metric declared with a QUOTED
    // name must still resolve through the shared resolved-expressions map in
    // the window CTE. `inline_derived_metrics` keys that map on the stored
    // (quote-retaining) name, while the window path looks it up via
    // `normalize_ident_part` (quote-stripped). Before the keying was unified,
    // the lookup missed and fell back to the metric's RAW expression — losing
    // the SG-8 COUNT(*)->COUNT(pk) rewrite, so the CTE counted NULL-extended
    // LEFT-JOIN rows (one per childless order): a silent overcount.
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &["id"])
        .with_dimension("product", "li.product", Some("li"))
        // Stored with literal quote characters and mixed case — the shape a
        // `METRICS ("Item_Count" AS COUNT(*))` declaration produces.
        .with_metric("\"Item_Count\"", "COUNT(*)", Some("li"))
        .with_metric("rolling_items", "AVG(\"Item_Count\")", None)
        .with_window_spec(
            "rolling_items",
            WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "\"Item_Count\"".to_string(),
                ..Default::default()
            },
        )
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("product")],
        metrics: vec![MetricName::new("rolling_items")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    // The column keeps the stored name's logical spelling, which DuckDB
    // matches case-insensitively.
    assert!(
        sql.contains("COUNT(\"li\".\"id\") AS \"Item_Count\""),
        "window CTE inner aggregate on a QUOTED-name base metric must still use \
         the rewritten count: {sql}"
    );
    assert!(
        !sql.contains("COUNT(*)"),
        "raw COUNT(*) must not leak into the CTE — the SG-8 rewrite was lost to a \
         quote-keyed lookup miss: {sql}"
    );
}

#[test]
fn test_semi_additive_co_query_uses_rewritten_count() {
    // Semi-additive path: a same-grain COUNT(*) co-metric on the
    // child table decomposes into a CTE capture of the child PK and
    // an outer COUNT over it (NULL-extended rows excluded). The
    // base-table COUNT(*) rejection in parse_snapshot_aggregate is
    // untouched (covered by semi_additive tests).
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_dimension("customer_id", "customer_id", None)
        .with_table("li", "line_items", &["id"])
        .with_dimension("report_date", "li.report_date", Some("li"))
        .with_metric("balance", "SUM(li.balance)", Some("li"))
        .with_metric("txn_count", "COUNT(*)", Some("li"))
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"])
        .with_non_additive_by(
            "balance",
            &[(
                "report_date",
                crate::model::SortOrder::Desc,
                crate::model::NullsOrder::First,
            )],
        );
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("customer_id")],
        metrics: vec![MetricName::new("balance"), MetricName::new("txn_count")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        sql.contains("\"li\".\"id\" AS \"__sv_reg_1\""),
        "CTE must capture the rewritten count argument: {sql}"
    );
    assert!(
        sql.contains("COUNT(\"__sv_reg_1\") AS \"txn_count\""),
        "outer select must re-aggregate the captured PK column: {sql}"
    );
}

// EXP-21 (code-review 2026-08-06): the SG-8 rewrite matched only the literal
// `*` argument, so `COUNT(1)` — the same idiom spelled differently — walked
// straight past every guard and counted the NULL-extended LEFT JOIN row that
// `COUNT(*)` is rewritten precisely to exclude. Verified against DuckDB with
// one childless order: `COUNT(1)` returned 2 next to `COUNT(*)`'s 1, in the
// same result row.
//
// The correction generalizes: an aggregate over a CONSTANT never sees a NULL
// argument, so nothing tells it a NULL-extended row is not a real one. Guarding
// the constant with the source table's PK restores the empty-group semantics
// the aggregate would have had on its own rows (`SUM(1)`/`LIST(1)` alike, and
// NULL rather than 0/1 for a childless parent).

/// `orders` (base) + `line_items` child with a declared PK, metrics spelled
/// with constant arguments instead of `*`.
fn constant_arg_def(expr: &str) -> crate::model::SemanticViewDefinition {
    orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_dimension("region", "region", None)
        .with_table("li", "line_items", &["id"])
        .with_metric("item_count", expr, Some("li"))
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"])
}

fn count_req() -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("item_count")],
    }
}

#[test]
fn test_child_count_one_is_guarded_by_the_pk() {
    let sql = expand("orders", &constant_arg_def("COUNT(1)"), &count_req()).unwrap();
    assert!(
        sql.contains("COUNT(CASE WHEN \"li\".\"id\" IS NOT NULL THEN 1 END)"),
        "COUNT(1) on a LEFT-JOINed child must not count NULL-extended rows: {sql}"
    );
}

#[test]
fn test_child_sum_of_a_constant_is_guarded_by_the_pk() {
    let sql = expand("orders", &constant_arg_def("SUM(1)"), &count_req()).unwrap();
    assert!(
        sql.contains("SUM(CASE WHEN \"li\".\"id\" IS NOT NULL THEN 1 END)"),
        "SUM(<constant>) inflates by one per childless parent just as COUNT(1) does: {sql}"
    );
}

#[test]
fn test_child_count_of_a_string_constant_is_guarded_by_the_pk() {
    let sql = expand("orders", &constant_arg_def("COUNT('x')"), &count_req()).unwrap();
    assert!(
        sql.contains("COUNT(CASE WHEN \"li\".\"id\" IS NOT NULL THEN 'x' END)"),
        "a string constant is as NULL-insensitive as a numeric one: {sql}"
    );
}

// A parenthesized literal is the same constant wearing a hat. `is_constant_literal`
// scanned the argument text as-is, so `COUNT((1))` failed the numeric check on the
// leading `(` and fell through unguarded — the identical over-count `COUNT(1)` had,
// reachable by anyone who writes a redundant paren. Raised by review on #203.
//
// Redundant parens are stripped only when the opening paren's match IS the final
// character: `(1)+(2)` also starts with `(` and ends with `)`, but its outer parens
// are not a pair, and blindly peeling them would hand the literal check the garbage
// `1)+(2`.
#[test]
fn test_child_count_of_a_parenthesized_constant_is_guarded_by_the_pk() {
    let sql = expand("orders", &constant_arg_def("COUNT((1))"), &count_req()).unwrap();
    assert!(
        sql.contains("COUNT(CASE WHEN \"li\".\"id\" IS NOT NULL THEN (1) END)"),
        "a parenthesized literal is as constant as a bare one: {sql}"
    );
}

#[test]
fn test_child_count_of_a_doubly_parenthesized_constant_is_guarded_by_the_pk() {
    let sql = expand(
        "orders",
        &constant_arg_def("COUNT(( ( 1 ) ))"),
        &count_req(),
    )
    .unwrap();
    assert!(
        sql.contains("CASE WHEN \"li\".\"id\" IS NOT NULL THEN ( ( 1 ) ) END"),
        "paren-stripping must iterate, and whitespace between them is not significant: {sql}"
    );
}

// The control that keeps the stripping honest: a non-pair `(`...`)` must not be
// peeled into nonsense and misread as constant.
//
// Since EXP-25 the guard no longer decides by argument shape — every aggregate
// argument is wrapped — so the classification is only load-bearing where the
// guard is IMPOSSIBLE: with no PRIMARY KEY on the joined table, a
// constant-argument aggregate must error while a row-dependent one must still
// answer. Both halves are asserted here so the paren-peeling stays covered.
#[test]
fn test_a_non_constant_paren_expression_is_not_misread_as_constant() {
    // With a PK, the argument is guarded whatever it is (the neutrality of that
    // wrap on real rows is pinned numerically in `tests_phantom_row_guard`).
    let sql = expand(
        "orders",
        &constant_arg_def("SUM((\"li\".\"qty\") + (1))"),
        &count_req(),
    )
    .unwrap();
    assert!(
        sql.contains("SUM(CASE WHEN \"li\".\"id\" IS NOT NULL THEN (\"li\".\"qty\") + (1) END)"),
        "every aggregate argument is guarded once a PK exists: {sql}"
    );

    // Without a PK there is nothing to guard with. `(x) + (1)` reads a real
    // column, so the phantom row is already excluded by its own NULL and the
    // query must still answer — peeling the non-pair parens would have
    // classified it as the constant `qty") + (1` and failed it loudly.
    let no_pk = |expr: &str| {
        orders_view()
            .clear_dimensions()
            .clear_metrics()
            .with_dimension("region", "region", None)
            .with_table("li", "line_items", &[]) // no PK declared
            .with_metric("item_count", expr, Some("li"))
            .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"])
    };
    // On the base-anchored fallback, where the no-PK classification runs.
    let sql = expand(
        "orders",
        &no_pk("SUM((\"li\".\"qty\") + (1))"),
        &fallback_req(),
    )
    .expect("a row-dependent argument needs no PK");
    assert!(
        !sql.contains("CASE WHEN"),
        "there is no PK to guard with: {sql}"
    );
    // …while the genuinely parenthesized constant still trips the no-PK error
    // on the base-anchored fallback (home-anchored it needs no PK at all).
    expand("orders", &no_pk("SUM(( ( 1 ) ))"), &count_req())
        .expect("home-anchored, a constant argument needs no PK");
    let err = expand("orders", &no_pk("SUM(( ( 1 ) ))"), &fallback_req())
        .expect_err("a constant argument with no PK is unguardable");
    assert!(
        matches!(err, ExpandError::CountStarRequiresPrimaryKey { .. }),
        "expected CountStarRequiresPrimaryKey, got: {err:?}"
    );
}

#[test]
fn test_child_count_one_without_a_pk_errors() {
    // Same fate as COUNT(*) with no PK: the rewrite is impossible, so the
    // query must fail loudly rather than over-count (SG-8).
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &[]) // no PK declared
        .with_dimension("region", "region", None)
        .with_metric("item_count", "COUNT(1)", Some("li"))
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"]);
    // Home-anchored (TECH-DEBT #71) it needs no PK; on the base-anchored
    // fallback it still cannot be guarded.
    expand("orders", &def, &count_req()).expect("home-anchored COUNT(1) needs no PK");
    let err = expand("orders", &def, &fallback_req()).unwrap_err();
    assert!(
        matches!(
            err,
            ExpandError::CountStarRequiresPrimaryKey {
                ref metric_name,
                ref table_alias,
                ..
            } if metric_name == "item_count" && table_alias == "li"
        ),
        "expected CountStarRequiresPrimaryKey, got: {err:?}"
    );
}

// Controls: the guard must fire on a JOINED table only, and never on the base
// table. Before EXP-25 it also had to fire on constant arguments only; that
// half is gone — the wrap is neutral on real rows for ANY argument, and
// restricting it by argument shape is exactly what leaked four ways.

#[test]
fn test_base_table_count_one_unchanged() {
    // The base table is never NULL-extended, so there is nothing to guard.
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_metric("n", "COUNT(1)", Some("orders"));
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("n")],
    };
    let sql = expand("orders", &def, &req).unwrap();
    assert!(
        sql.contains("COUNT(1) AS \"n\"") && !sql.contains("CASE WHEN"),
        "a base-table constant aggregate must be left alone: {sql}"
    );
}

#[test]
fn test_child_count_of_a_column_is_guarded_too() {
    // A column argument is already NULL on a NULL-extended row, so the guard
    // changes nothing here — which is the point: EXP-26 showed that deciding
    // guard-or-not by inspecting the argument is what lets
    // `SUM(COALESCE(li.qty, 99))` through, and "already NULL" is a property of
    // the column, not of every expression built from it.
    let sql = expand("orders", &constant_arg_def("COUNT(li.sku)"), &count_req()).unwrap();
    assert!(
        sql.contains("COUNT(CASE WHEN \"li\".\"id\" IS NOT NULL THEN li.sku END)"),
        "a column argument is guarded like any other: {sql}"
    );
}

#[test]
fn test_child_min_of_a_constant_is_guarded_by_the_pk() {
    // EXP-25: MIN/MAX/AVG over a constant ARE multiplicity-invariant — and
    // still wrong, because the phantom row is not a duplicate but a row that
    // should not exist. `MIN(1)` on a childless parent returned 1 where the
    // empty-group answer is NULL (pinned numerically in
    // `tests_phantom_row_guard::exp25_min_constant_is_null_for_a_childless_parent`).
    let sql = expand("orders", &constant_arg_def("MIN(1)"), &count_req()).unwrap();
    assert!(
        sql.contains("MIN(CASE WHEN \"li\".\"id\" IS NOT NULL THEN 1 END)"),
        "MIN over a constant needs the existence guard: {sql}"
    );
}

/// Reviewbot, #250: the SG-8 exemption for a CHILD-anchored `__sv_snapshot`.
/// `line_items` declares no PRIMARY KEY, so on the base-anchored path its
/// `COUNT(1)` cannot be PK-guarded (EXP-21) and the query errors. Co-queried with a
/// `NON ADDITIVE BY` metric on the same table, the snapshot CTE anchors at
/// `li` itself (TECH-DEBT #71): there is no NULL-extended row to exclude, so
/// the query is answered, and `COUNT(1)` counts every child row. (`COUNT(*)`
/// itself is rejected by the snapshot decomposition, SG-5, on either path.)
///
/// Executes the SQL in-memory, so gated off the `extension` feature like the
/// other number-pinning tests; the default-features run covers it.
#[cfg(not(feature = "extension"))]
#[test]
fn test_child_count_star_without_pk_is_answered_from_a_child_anchored_snapshot() {
    let def = orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &[]) // no PK declared
        .with_dimension("report_date", "li.report_date", Some("li"))
        .with_metric("balance", "SUM(li.balance)", Some("li"))
        .with_metric("txn_count", "COUNT(1)", Some("li"))
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"])
        .with_non_additive_by(
            "balance",
            &[(
                "report_date",
                crate::model::SortOrder::Desc,
                crate::model::NullsOrder::First,
            )],
        );
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("balance"), MetricName::new("txn_count")],
    };
    let sql = expand("orders", &def, &req)
        .expect("a child-anchored snapshot has no NULL-extended row, so needs no PK");
    assert!(
        sql.contains("__sv_snapshot") && sql.contains("FROM \"line_items\" AS \"li\""),
        "the snapshot CTE must be anchored at the child: {sql}"
    );
    assert!(
        !sql.contains("FROM \"orders\""),
        "nothing may be base-anchored, or COUNT(1) would see the phantom row: {sql}"
    );

    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE orders (id INT, region VARCHAR); \
         CREATE TABLE line_items (order_id INT, report_date DATE, balance INT); \
         INSERT INTO orders VALUES (1, 'EU'), (2, 'US'); \
         INSERT INTO line_items VALUES (1, DATE '2024-01-01', 10), \
             (1, DATE '2024-01-02', 20), (NULL, DATE '2024-01-02', 5);",
    )
    .expect("load the fixture");
    let got: (i64, i64) = conn
        .query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or_else(|e| panic!("query failed: {e}\n{sql}"));
    // `NON ADDITIVE BY (report_date DESC)` keeps the rows that sort last,
    // 2024-01-01: 10. Three child rows, the orphan included; the childless
    // order 2 contributes no phantom row (base-anchored, it would be 4).
    assert_eq!(got, (10, 3));
}
