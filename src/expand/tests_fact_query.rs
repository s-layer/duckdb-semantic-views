//! Fact query expansion (unaggregated row-level SELECT).
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase46_fact_query_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::test_helpers::TestFixtureExt;
use crate::model::SemanticViewDefinition;

/// Build a multi-table def: orders (o) -> line_items (li), with a dim on o and facts on li.
///
/// TC-12: every def in this file used to declare a fourth table
/// `orders AS orders` with no primary key, joined to nothing. Being declared
/// first made it the graph root, so `expand` emitted
/// `FROM "orders" AS "orders" LEFT JOIN "line_items" AS "li" ON … = "o"."id"`
/// — a forward reference to an alias joined on the *next* line, which DuckDB
/// rejects with `Binder Error: Referenced table "o" not found`. The
/// substring assertions below all passed against SQL that could not bind,
/// which is exactly why they needed an executable sibling. The dead alias is
/// gone; see `fact_query_basic_is_row_level_against_real_data` for the
/// data-level statement of the same behaviour.
fn multi_table_def() -> SemanticViewDefinition {
    SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("li", "line_items", &["id"])
        .with_dimension("region", "o.region", Some("o"))
        .with_fact("net_price", "li.price * (1 - li.discount)", "li")
        .with_metric("total_revenue", "sum(li.price)", Some("li"))
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
}

/// The request `test_fact_query_basic` and its executable sibling both use:
/// one fact on the child table, one dimension on the parent.
fn basic_fact_request() -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("region"), FactName::new("net_price")],
        dimensions: vec![],
        metrics: vec![],
    }
}

#[test]
fn test_fact_query_basic() {
    let def = multi_table_def();
    let req = basic_fact_request();
    let sql = expand("test_view", &def, &req).unwrap();
    assert!(
        !sql.contains("GROUP BY"),
        "Fact queries must NOT have GROUP BY: {sql}"
    );
    assert!(sql.contains("o.region"), "Must include dim expr: {sql}");
    assert!(
        sql.contains("li.price * (1 - li.discount)"),
        "Must include fact expr: {sql}"
    );
    assert!(sql.contains("FROM"), "Must have FROM clause: {sql}");
    assert!(sql.contains("LEFT JOIN"), "Must include JOIN for li: {sql}");
}

/// What the structural assertions above are *for*, stated in numbers: the same
/// def and request, run against real rows.
///
/// TC-12 (code-review 2026-08-08): `contains("LEFT JOIN")` and
/// `!contains("GROUP BY")` were never executed against data, so they could not
/// distinguish "joins the child at row grain" from "emits a string containing
/// the words LEFT JOIN". Three claims are checked here that no substring can
/// make:
///
/// * the result is at **line-item** grain, not order grain — two line items on
///   one order produce two rows, and the parent's dimension repeats;
/// * the fact expression is evaluated per row rather than aggregated;
/// * an order with no line items contributes **no** row, i.e. the `LEFT JOIN`
///   is fenced by the child-key `IS NOT NULL` guard rather than manufacturing
///   a NULL fact row.
#[cfg(not(feature = "extension"))]
#[test]
fn fact_query_basic_is_row_level_against_real_data() {
    let sql = expand("test_view", &multi_table_def(), &basic_fact_request()).unwrap();

    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE orders (id INTEGER, region VARCHAR);
         -- order 3 has no line items at all
         INSERT INTO orders VALUES (1, 'E'), (2, 'W'), (3, 'S');
         CREATE TABLE line_items (id INTEGER, order_id INTEGER, price INTEGER, discount DOUBLE);
         INSERT INTO line_items VALUES (1, 1, 100, 0.0), (2, 1, 50, 0.5), (3, 2, 10, 0.0);",
    )
    .expect("setup");

    let mut stmt = con
        .prepare(&format!(
            "SELECT region, net_price FROM ({sql}) ORDER BY 1, 2"
        ))
        .expect("the emitted fact query must bind");
    let rows: Vec<(String, f64)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
        .expect("run")
        .map(|r| r.expect("row"))
        .collect();

    assert_eq!(
        rows,
        vec![
            ("E".to_string(), 25.0),  // line item 2: 50 * (1 - 0.5)
            ("E".to_string(), 100.0), // line item 1: 100 * (1 - 0.0)
            ("W".to_string(), 10.0),  // line item 3: 10 * (1 - 0.0)
        ],
        "a fact query is one row per line item with the parent's dimension \
         repeated, and the childless order 'S' contributes none"
    );
}

#[test]
fn test_fact_query_no_dimensions() {
    let def = multi_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("net_price")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test_view", &def, &req).unwrap();
    assert!(
        !sql.contains("GROUP BY"),
        "Fact queries must NOT have GROUP BY: {sql}"
    );
    assert!(
        sql.contains("li.price * (1 - li.discount)"),
        "Must include fact expr: {sql}"
    );
    assert!(
        !sql.contains("DISTINCT"),
        "Fact queries without dims should not use DISTINCT: {sql}"
    );
}

#[test]
fn test_fact_query_inline_facts() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("li", "line_items", &["id"])
        .with_fact("net_price", "li.price * (1 - li.discount)", "li")
        .with_fact("line_total", "net_price * li.quantity", "li")
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("line_total")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test_view", &def, &req).unwrap();
    // line_total's expression should have net_price inlined (parenthesized)
    assert!(
        sql.contains("(li.price * (1 - li.discount))"),
        "Must inline net_price into line_total: {sql}"
    );
}

#[test]
fn test_fact_query_unknown_fact() {
    let def = multi_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("nonexistent")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExpandError::UnknownFact { .. }),
        "Expected UnknownFact, got: {err}"
    );
}

#[test]
fn test_fact_query_duplicate_fact() {
    let def = multi_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("net_price"), FactName::new("net_price")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExpandError::DuplicateFact { .. }),
        "Expected DuplicateFact, got: {err}"
    );
}

#[test]
fn test_fact_query_private_fact() {
    let def = multi_table_def().with_private_fact("raw_price", "li.price", "li");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("raw_price")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExpandError::PrivateFact { .. }),
        "Expected PrivateFact, got: {err}"
    );
}

#[test]
fn test_fact_path_violation() {
    // Fan shape: o -> li, o -> payments (divergent paths)
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("li", "line_items", &["id"])
        .with_table("p", "payments", &["id"])
        .with_fact("net_price", "li.price * (1 - li.discount)", "li")
        .with_dimension("pay_status", "CAST(p.amount AS VARCHAR)", Some("p"))
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
        .with_pkfk_join("p_to_o", "p", "o", &["order_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("pay_status"), FactName::new("net_price")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExpandError::FactPathViolation { .. }),
        "Expected FactPathViolation, got: {err}"
    );
}

/// Fan-in onto the base table: `li` and `s` both reference the base table `o`,
/// which in turn references `c`.
///
/// This is the only legal fan-in — `RelationshipGraph::check_no_diamonds`
/// exempts the root and rejects every other multi-parent node as an ambiguous
/// join diamond.
///
/// TECH-DEBT #37: the parent map used to take `reverse[node].first()`, so the
/// base table was handed a "parent" — whichever child was declared first — and
/// chains ran through the root and out the far branch. `c`'s chain became
/// `[c, o, li]`, which never reaches `s`.
fn fan_in_def() -> SemanticViewDefinition {
    SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("li", "line_items", &["id"])
        .with_table("s", "shipments", &["id"])
        .with_table("c", "customers", &["id"])
        .with_fact("ship_cost", "s.cost", "s")
        .with_fact("line_price", "li.price", "li")
        .with_dimension("carrier", "s.carrier", Some("s"))
        .with_dimension("country", "c.country", Some("c"))
        // Declared first -- this ordering is what gave the root its bogus parent.
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
        .with_pkfk_join("s_to_o", "s", "o", &["order_id"], &["id"])
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"])
}

#[test]
fn fact_with_dimension_across_fan_in_is_allowed() {
    // TECH-DEBT #37: `s -> o -> c` is many-to-one at every hop, so a fact on the
    // base table with a dimension on `c` joins without multiplying rows. The
    // unrelated fan-in sibling `li` must not hide `c` from it.
    let def = fan_in_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("country"), FactName::new("ship_cost")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    assert!(
        result.is_ok(),
        "s -> o -> c is safe at every hop; the fan-in sibling must not cause a \
         FactPathViolation: {result:?}"
    );
}

#[test]
fn fact_across_fan_in_siblings_still_violates() {
    // The over-widening guard. `li` and `s` are the two fan-in siblings:
    // reaching one from the other traverses a many-to-one edge BACKWARDS
    // (`o -> li` or `o -> s`), which multiplies rows. Once the parent map is
    // rooted at the base table both siblings share the ancestor `o`, so a check
    // that only asked about ancestry would be tempted to accept them — the path
    // must be checked for fan-out DIRECTION, not merely for connectivity.
    let def = fan_in_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("carrier"), FactName::new("line_price")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    assert!(
        matches!(result, Err(ExpandError::FactPathViolation { .. })),
        "Expected FactPathViolation for fan-in siblings li/s, got: {result:?}"
    );
}

#[test]
fn fact_reaches_a_multi_hop_chain_across_fan_in() {
    // The safe direction holds for more than one hop past the fan-in:
    // s -> o -> c -> r is many-to-one throughout.
    let def = fan_in_def()
        .with_table("r", "regions", &["id"])
        .with_dimension("region_name", "r.name", Some("r"))
        .with_pkfk_join("c_to_r", "c", "r", &["region_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("region_name"), FactName::new("ship_cost")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    assert!(
        result.is_ok(),
        "s -> o -> c -> r is safe at every hop: {result:?}"
    );
}

#[test]
fn test_fact_path_valid_linear() {
    // Chain: o -> li -> details (linear path)
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("li", "line_items", &["id"])
        .with_table("d", "details", &["id"])
        .with_fact("detail_val", "d.value", "d")
        .with_dimension("region", "o.region", Some("o"))
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
        .with_pkfk_join("d_to_li", "d", "li", &["line_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("region"), FactName::new("detail_val")],
        dimensions: vec![],
        metrics: vec![],
    };
    let result = expand("test_view", &def, &req);
    assert!(result.is_ok(), "Linear path should be valid: {result:?}");
}

#[test]
fn test_fact_query_with_output_type() {
    let mut def = multi_table_def();
    def.facts[0].output_type = Some("DECIMAL(10,2)".to_string());
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("net_price")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test_view", &def, &req).unwrap();
    assert!(
        sql.contains("CAST("),
        "Must wrap fact in CAST when output_type is set: {sql}"
    );
    assert!(
        sql.contains("DECIMAL(10,2)"),
        "Must include output type: {sql}"
    );
}
