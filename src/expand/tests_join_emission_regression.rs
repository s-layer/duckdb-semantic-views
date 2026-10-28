//! Join-clause emission ordering regressions.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::join_emission_regression_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::test_helpers::TestFixtureExt;
use crate::model::SemanticViewDefinition;

/// li (base) -> o -> c chain with configurable relationship
/// declaration order.
fn li_o_c_def(o_to_c_first: bool) -> SemanticViewDefinition {
    let def = SemanticViewDefinition::default()
        .with_table("li", "line_items", &["id"])
        .with_table("o", "orders", &["id"])
        .with_table("c", "customers", &["id"])
        .with_dimension("customer_name", "c.name", Some("c"))
        .with_metric("total_qty", "sum(li.qty)", Some("li"));
    if o_to_c_first {
        def.with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"])
            .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
    } else {
        def.with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
            .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"])
    }
}

/// SG-2: the join emitted for alias `o` must be the edge that connects
/// `o` to the already-emitted base (li -> o), not whichever declared
/// relationship mentions `o` first (o -> c would forward-reference the
/// not-yet-joined `c`). Both declaration orders must produce identical
/// SQL with no forward references and no dropped joins.
#[test]
fn sg2_join_selection_is_declaration_order_independent() {
    let expected = "\
SELECT
    c.name AS \"customer_name\",
    sum(li.qty) AS \"total_qty\"
FROM \"line_items\" AS \"li\"
LEFT JOIN \"orders\" AS \"o\" ON \"li\".\"order_id\" = \"o\".\"id\"
LEFT JOIN \"customers\" AS \"c\" ON \"o\".\"customer_id\" = \"c\".\"id\"
GROUP BY
    1";
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("customer_name")],
        metrics: vec![MetricName::new("total_qty")],
    };
    let sql_a = expand("test", &li_o_c_def(true), &req).unwrap();
    let sql_b = expand("test", &li_o_c_def(false), &req).unwrap();
    assert_eq!(sql_a, expected, "o->c declared first must be correct");
    assert_eq!(sql_b, expected, "li->o declared first must be correct");
}

/// SG-2: a child table (`li`) with FKs to two parents (`p` declared
/// first, then `o` = base). Query needing only the li -> o edge must
/// emit it — not the first-declared li -> p edge, whose ON clause
/// would reference the never-joined `p`.
#[test]
fn sg2_two_parent_child_picks_connecting_edge() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("li", "line_items", &["id"])
        .with_table("p", "products", &["id"])
        .with_dimension("region", "o.region", Some("o"))
        .with_metric("qty", "sum(li.qty)", Some("li"))
        .with_pkfk_join("li_to_p", "li", "p", &["product_id"], &["id"])
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("qty")],
    };
    let sql = expand("test", &def, &req).unwrap();
    // TECH-DEBT #71: the metric is anchored at its own table `li` and joins OUT
    // to `o` — through the li -> o edge, not the first-declared li -> p one.
    let expected = "\
SELECT
    o.region AS \"region\",
    sum(CASE WHEN \"li\".\"id\" IS NOT NULL THEN li.qty END) AS \"qty\"
FROM \"line_items\" AS \"li\"
LEFT JOIN \"orders\" AS \"o\" ON \"li\".\"order_id\" = \"o\".\"id\"
GROUP BY
    1";
    assert_eq!(sql, expected, "must join o via li->o, not li->p");
    assert!(
        !sql.contains("\"p\"."),
        "ON clause must not reference the never-joined p: {sql}"
    );

    // The base-anchored resolver the test was written for still emits the
    // dimensions-only path, which is where SG-2's edge choice now lives for a
    // query reaching `li` from the base.
    let def = def.with_dimension("item_qty", "li.qty", Some("li"));
    let dims_only = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region"), DimensionName::new("item_qty")],
        metrics: vec![],
    };
    let sql = expand("test", &def, &dims_only).unwrap();
    assert!(
        sql.contains(
            "FROM \"orders\" AS \"o\"\nLEFT JOIN \"line_items\" AS \"li\" \
             ON \"li\".\"order_id\" = \"o\".\"id\""
        ),
        "the resolver must join li via li->o: {sql}"
    );
    assert!(
        !sql.contains("\"p\"."),
        "ON clause must not reference the never-joined p: {sql}"
    );
}

/// ld -> li -> o (base) chain: metric two hops below the root.
fn ld_li_o_def() -> SemanticViewDefinition {
    SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("li", "line_items", &["id"])
        .with_table("ld", "line_item_details", &["id"])
        .with_dimension("region", "o.region", Some("o"))
        .with_metric("detail_qty", "sum(ld.qty)", Some("ld"))
        .with_fact("detail_amount", "ld.amount", "ld")
        .with_pkfk_join("ld_to_li", "ld", "li", &["line_item_id"], &["id"])
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
}

/// SG-10: a needed table two hops below the root must pull in its
/// intermediate (`li`) and join in dependency order (li before ld),
/// with each ON clause referencing only already-joined tables.
#[test]
fn sg10_fk_side_chain_includes_intermediate_join() {
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("detail_qty")],
    };
    let sql = expand("test", &ld_li_o_def(), &req).unwrap();
    // TECH-DEBT #71: anchored at the metric's own table two hops below the
    // base, the walk OUT to the dimension still pulls in the intermediate `li`,
    // in dependency order. (The base-anchored resolver's walk DOWN is pinned by
    // the facts-path test below.)
    let expected = "\
SELECT
    o.region AS \"region\",
    sum(CASE WHEN \"ld\".\"id\" IS NOT NULL THEN ld.qty END) AS \"detail_qty\"
FROM \"line_item_details\" AS \"ld\"
LEFT JOIN \"line_items\" AS \"li\" ON \"ld\".\"line_item_id\" = \"li\".\"id\"
LEFT JOIN \"orders\" AS \"o\" ON \"li\".\"order_id\" = \"o\".\"id\"
GROUP BY
    1";
    assert_eq!(sql, expected);

    // Reversed declaration order must produce the same SQL.
    let mut def_rev = ld_li_o_def();
    def_rev.joins.reverse();
    let sql_rev = expand("test", &def_rev, &req).unwrap();
    assert_eq!(sql_rev, expected, "declaration order must not matter");
}

/// SG-10 (facts path): `expand_facts` previously joined only the
/// fact's direct source table with no path walk; the intermediate
/// `li` join was missing entirely.
#[test]
fn sg10_fact_source_chain_includes_intermediate_join() {
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("detail_amount")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test", &ld_li_o_def(), &req).unwrap();
    // EXP-28: the queried fact lives on `ld` alone, so `ld` is the query's
    // grain and the two LEFT JOINs above it would otherwise manufacture one
    // all-NULL row per orderless/detail-less ancestor.
    let expected = "\
SELECT
    ld.amount AS \"detail_amount\"
FROM \"orders\" AS \"o\"
LEFT JOIN \"line_items\" AS \"li\" ON \"li\".\"order_id\" = \"o\".\"id\"
LEFT JOIN \"line_item_details\" AS \"ld\" ON \"ld\".\"line_item_id\" = \"li\".\"id\"
WHERE \"ld\".\"id\" IS NOT NULL";
    assert_eq!(sql, expected);
}

/// SG-12: a user table alias containing `__` is a bare alias, not a
/// role-playing scoped alias. It must be joined normally — previously
/// the emitter re-parsed the alias at the first `__`, looked up a
/// relationship named after the suffix, and silently dropped the join.
#[test]
fn sg12_bare_alias_containing_double_underscore_joins_normally() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("my__dim", "dim_table", &["id"])
        .with_dimension("dim_name", "my__dim.name", Some("my__dim"))
        .with_metric("cnt", "count(*)", Some("o"))
        .with_pkfk_join("o_to_dim", "o", "my__dim", &["dim_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("dim_name")],
        metrics: vec![MetricName::new("cnt")],
    };
    let sql = expand("test", &def, &req).unwrap();
    let expected = "\
SELECT
    my__dim.name AS \"dim_name\",
    count(*) AS \"cnt\"
FROM \"orders\" AS \"o\"
LEFT JOIN \"dim_table\" AS \"my__dim\" ON \"o\".\"dim_id\" = \"my__dim\".\"id\"
GROUP BY
    1";
    assert_eq!(sql, expected);
}

/// SG-12: role-playing scoped aliases keep the documented
/// `{alias}__{relationship}` SQL alias format, and the scoped alias is
/// used on the PK side of the ON clause.
#[test]
fn sg12_role_playing_scoped_alias_format_preserved() {
    let def = SemanticViewDefinition::default()
        .with_table("f", "flights", &["flight_id"])
        .with_table("a", "airports", &["airport_code"])
        .with_dimension("city", "a.city", Some("a"))
        .with_metric("departure_count", "COUNT(*)", Some("f"))
        .with_using_relationship("departure_count", &["dep_airport"])
        .with_pkfk_join(
            "dep_airport",
            "f",
            "a",
            &["departure_code"],
            &["airport_code"],
        )
        .with_pkfk_join(
            "arr_airport",
            "f",
            "a",
            &["arrival_code"],
            &["airport_code"],
        );
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("city")],
        metrics: vec![MetricName::new("departure_count")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("LEFT JOIN \"airports\" AS \"a__dep_airport\""),
        "Scoped alias format {{alias}}__{{rel}} must be preserved: {sql}"
    );
    assert!(
        sql.contains("\"f\".\"departure_code\" = \"a__dep_airport\".\"airport_code\""),
        "ON clause must use the scoped alias on the PK side: {sql}"
    );
}

// ---------------------------------------------------------------------------
// PAR-3: joins come from declared `source_table`, never from scanning an
// expression for foreign aliases
// ---------------------------------------------------------------------------

/// PAR-3 (code-review 2026-08-03), **pinning a documented limitation — see
/// TECH-DEBT #52**, not behaviour to preserve.
///
/// A member expression may reference base-table columns of its own logical
/// table only; a raw column of *another* logical table (`c.discount` on a
/// metric declared `source_table = o`) is not a supported reference. Snowflake
/// rejects the same shape — "Expressions cannot refer to base table columns
/// from other tables" — but rejects it at CREATE time, while here the DDL is
/// accepted and the expression is emitted verbatim, pulling no join for `c`.
/// DuckDB then raises a binder error for the unknown alias at query time.
///
/// The number is never wrong: `c` is alias-qualified, so with no `c` in scope
/// the query cannot bind, and the failure is loud. What is missing is the
/// CREATE-time validation that would name the real problem. TECH-DEBT #52
/// records that; this test exists so the interim behaviour is visible rather
/// than merely absent, and must be *replaced* — not deleted — when the
/// validator lands.
#[test]
fn par3_cross_table_column_reference_pulls_no_join() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("c", "customers", &["id"])
        .with_dimension("order_status", "o.status", Some("o"))
        .with_metric("mixed_margin", "SUM(o.amount - c.discount)", Some("o"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("order_status")],
        metrics: vec![MetricName::new("mixed_margin")],
    };
    let sql = expand("test", &def, &req).expect("the expander itself does not police scoping");
    assert!(
        !sql.contains("customers"),
        "the `c.discount` reference must not pull a join — joins come from \
         `source_table` and fact references, never from a raw foreign column: {sql}"
    );
    assert!(
        sql.contains("SUM(o.amount - c.discount)"),
        "the expression is emitted verbatim, leaving `c` unbound: {sql}"
    );
}

/// PAR-6 (found while verifying PAR-3, TECH-DEBT #53). The *named-fact*
/// cross-table form is the one Snowflake supports ("define facts on source
/// tables, then refer to these expressions from connected logical tables") and
/// the one this codebase built deliberately: `fact_replacement_map` keys each
/// fact by its own `source_table.name` so that "a fact referenced across tables
/// in its own-qualified form is then actually inlined, not just detected".
///
/// Only the inlining half was wired up. `join_resolver` collected aliases from
/// each member's declared `source_table` and from *queried* facts, never from
/// facts a metric merely references — so `c.discount` was spliced into a metric
/// on `o` while `customers` stayed out of the FROM clause, and the emitted SQL
/// could not bind. The referenced fact's table is now collected too.
#[test]
fn par6_cross_table_fact_reference_pulls_its_join() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("c", "customers", &["id"])
        .with_fact("cust_discount", "c.discount", "c")
        .with_dimension("order_status", "o.status", Some("o"))
        .with_metric("mixed_margin", "SUM(o.amount - c.cust_discount)", Some("o"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("order_status")],
        metrics: vec![MetricName::new("mixed_margin")],
    };
    let sql = expand("test", &def, &req).expect("a cross-table fact reference is answerable");
    assert!(
        sql.contains("(c.discount)"),
        "the referenced fact must be inlined at its reference site: {sql}"
    );
    assert!(
        sql.contains(r#"LEFT JOIN "customers" AS "c""#),
        "the referenced fact's table must be joined so the expression binds: {sql}"
    );
}

/// The same reference, but the fact sits on a table that **fans** relative to
/// the metric's: `li` is a child of `o`, so joining it multiplies each order
/// row and `SUM(o.amount - …)` would count an order once per line item.
///
/// Joining is only safe when the path from the metric's table to the fact's is
/// non-fanning, which is the same rule the fence already applies to a queried
/// dimension and to a `where_clause` member. A fanning one errors rather than
/// silently inflating — the failure PAR-6's fix would otherwise introduce, since
/// before it this query raised no error only because the join was missing.
#[test]
fn par6_cross_table_fact_on_a_fanning_table_errors() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("li", "line_items", &["id"])
        .with_fact("item_cost", "li.cost", "li")
        .with_dimension("order_status", "o.status", Some("o"))
        .with_metric("bad_margin", "SUM(o.amount - li.item_cost)", Some("o"))
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("order_status")],
        metrics: vec![MetricName::new("bad_margin")],
    };
    let err = expand("test", &def, &req)
        .expect_err("a fact on a fanning table must not be joined into the aggregate");
    let msg = err.to_string();
    assert!(
        msg.contains("fan trap detected"),
        "must report the fan trap rather than emitting an inflated aggregate: {msg}"
    );
    assert!(
        msg.contains("item_cost") && msg.contains("li_to_o"),
        "the message must name the referenced fact and the fanning relationship: {msg}"
    );
}

/// The form that *does* work today, and therefore the only cross-table
/// workaround worth documenting: query the fact directly. `fact_source_tables`
/// feeds `join_resolver`, so the fact's table is joined. Pinned next to #53 so
/// that a regression here would not leave the limitation without any escape.
#[test]
fn queried_cross_table_fact_pulls_its_join() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("c", "customers", &["id"])
        .with_fact("cust_discount", "c.discount", "c")
        .with_dimension("order_status", "o.status", Some("o"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![
            FactName::new("order_status"),
            FactName::new("cust_discount"),
        ],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test", &def, &req).expect("a queried cross-table fact is supported");
    assert!(
        sql.contains("LEFT JOIN \"customers\" AS \"c\""),
        "a queried fact must pull its own table's join: {sql}"
    );
}

/// Copilot review of #200: a DERIVED metric whose base component references a
/// cross-table fact, on the PER-GRAIN path. `per_grain::anchor_needs` scans
/// each group's metrics, and a derived metric's own expression names METRICS,
/// not facts -- the fact references live in the base metrics inlined into it.
/// Scanning only the derived expression reintroduced PAR-6 inside the grain
/// CTE: `(SUM(o.amount - (p.markup))) * 2` rendered over a FROM clause with no
/// join for `p`.
#[test]
fn par6_derived_metric_fact_reference_joins_inside_the_grain_cte() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("c", "customers", &["id"])
        .with_table("p", "products", &["id"])
        .with_fact("prod_markup", "p.markup", "p")
        .with_dimension("region", "c.region", Some("c"))
        .with_metric("net", "SUM(o.amount - p.prod_markup)", Some("o"))
        .with_metric("net_x2", "net * 2", None)
        .with_metric("total_discount", "SUM(c.discount)", Some("c"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"])
        .with_pkfk_join("o_to_p", "o", "p", &["product_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("net_x2"), MetricName::new("total_discount")],
    };
    let sql = expand("test", &def, &req).expect("two grains, one reaching a cross-table fact");
    assert!(
        sql.contains("(p.markup)"),
        "the fact must still be inlined through the derived metric: {sql}"
    );
    assert!(
        sql.contains(r#"LEFT JOIN "products" AS "p""#),
        "the grain CTE that inlines the fact must join its table: {sql}"
    );
}

/// PAR-6's facts-path sibling: a QUERIED fact whose own expression references a
/// fact on a third table. `inline_facts` splices it in exactly as it does
/// inside a metric, but `fact_source_tables` carries only each queried fact's
/// own declared table, so the chain's far end was never joined.
#[test]
fn par6_queried_fact_chaining_to_another_table_pulls_its_join() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("p", "products", &["id"])
        .with_fact("prod_markup", "p.markup", "p")
        .with_fact("net_line", "o.amount - p.prod_markup", "o")
        .with_dimension("order_id", "o.id", Some("o"))
        .with_metric("total", "SUM(o.amount)", Some("o"))
        .with_pkfk_join("o_to_p", "o", "p", &["product_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("order_id"), FactName::new("net_line")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test", &def, &req).expect("a fact chaining across tables is answerable");
    assert!(
        sql.contains("(p.markup)"),
        "the chained fact must be inlined: {sql}"
    );
    assert!(
        sql.contains(r#"LEFT JOIN "products" AS "p""#),
        "the chained fact's table must be joined: {sql}"
    );
}

/// The same reference on the FACTS query path, which builds its SELECT list
/// from a separate site.
#[test]
fn td54_dimension_fact_reference_is_inlined_on_the_facts_path() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_fact("net_line", "o.amount - o.discount", "o")
        .with_fact("raw_amount", "o.amount", "o")
        .with_dimension("band", "o.net_line", Some("o"))
        .with_metric("total", "SUM(o.amount)", Some("o"));
    let req = QueryRequest {
        where_clause: None,
        facts: vec![FactName::new("band"), FactName::new("raw_amount")],
        dimensions: vec![],
        metrics: vec![],
    };
    let sql = expand("test", &def, &req).expect("a fact query with such a dimension is answerable");
    assert!(
        sql.contains("(o.amount - o.discount) AS \"band\""),
        "the facts path must inline the dimension's fact too: {sql}"
    );
}

/// And inside a `where_clause` predicate, whose member expressions are spliced
/// into the WHERE rather than the SELECT list.
#[test]
fn td54_dimension_fact_reference_is_inlined_in_a_where_clause() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_fact("net_line", "o.amount - o.discount", "o")
        .with_dimension("band", "o.net_line", Some("o"))
        .with_dimension("status", "o.status", Some("o"))
        .with_metric("total", "SUM(o.amount)", Some("o"));
    let req = QueryRequest {
        where_clause: Some("band > 10".to_string()),
        facts: vec![],
        dimensions: vec![DimensionName::new("status")],
        metrics: vec![MetricName::new("total")],
    };
    let sql = expand("test", &def, &req).expect("a predicate over such a dimension is answerable");
    assert!(
        sql.contains("(o.amount - o.discount)") && sql.contains("WHERE"),
        "the predicate must carry the inlined expression: {sql}"
    );
    assert!(
        !sql.contains("o.net_line"),
        "no un-inlined fact name may survive into the predicate: {sql}"
    );
}

/// TECH-DEBT #54, cross-table half: with dimension expressions now fact-inlined,
/// a dimension reaching a fact on ANOTHER table needs that table joined, exactly
/// as a metric does (PAR-6). This is the collection #200 removed as premature —
/// it joined a relation for a reference that was never substituted — restored
/// now that the substitution happens.
#[test]
fn td54_cross_table_fact_reference_in_a_dimension_pulls_its_join() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("c", "customers", &["id"])
        .with_fact("cust_tier", "c.tier", "c")
        .with_dimension("tier_label", "c.cust_tier", Some("o"))
        .with_metric("total", "SUM(o.amount)", Some("o"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("tier_label")],
        metrics: vec![MetricName::new("total")],
    };
    let sql = expand("test", &def, &req).expect("a cross-table fact in a dimension is answerable");
    assert!(
        sql.contains("(c.tier) AS \"tier_label\""),
        "the fact must be inlined into the dimension: {sql}"
    );
    assert!(
        sql.contains(r#"LEFT JOIN "customers" AS "c""#),
        "the reached fact's table must be joined: {sql}"
    );
}

/// The same, inside a grain CTE: the per-grain planner projects dimension
/// expressions as its group keys, so each CTE must join what those expressions
/// reach. Copilot flagged this call site while reviewing #200 — correctly about
/// where it was heading, though it was unreachable until dimensions inlined.
///
/// Topology matters here. An earlier draft put the dimension on the base table
/// with a parent-table metric, which the metric x dimension fence rejects for a
/// reason that has nothing to do with facts — the test passed through its error
/// branch and proved nothing. The chain below (`o` -> `c` -> `seg`, all
/// many-to-one) keeps every grain able to reach the dimension without fanning,
/// so the query really is answered and the assertions really run.
#[test]
fn td54_cross_table_dimension_fact_joins_inside_the_grain_cte() {
    let def = SemanticViewDefinition::default()
        .with_table("o", "orders", &["id"])
        .with_table("c", "customers", &["id"])
        .with_table("seg", "segments", &["id"])
        .with_fact("seg_name", "seg.name", "seg")
        .with_dimension("seg_label", "seg.seg_name", Some("c"))
        .with_metric("order_total", "SUM(o.amount)", Some("o"))
        .with_metric("cust_total", "SUM(c.balance)", Some("c"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"])
        .with_pkfk_join("c_to_seg", "c", "seg", &["segment_id"], &["id"]);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("seg_label")],
        metrics: vec![
            MetricName::new("order_total"),
            MetricName::new("cust_total"),
        ],
    };
    let sql = expand("test", &def, &req)
        .expect("two grains, both reaching the dimension without fanning");
    assert!(
        sql.contains("__sv_grain_1"),
        "this must be the per-grain path, or the test is not exercising it: {sql}"
    );
    assert!(
        sql.contains("(seg.name)"),
        "the dimension's fact must be inlined in the grain CTEs: {sql}"
    );
    assert_eq!(
        sql.matches(r#"LEFT JOIN "segments" AS "seg""#).count(),
        2,
        "BOTH grain CTEs project the dimension, so both must join its fact's table: {sql}"
    );
}
