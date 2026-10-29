//! Per-grain ("own-grain") metric aggregation — the Snowflake-parity path
//! (TECH-DEBT #35, v0.12.0).
//!
//! v0.11.0 closed the *silent-wrong-answer* half of the multi-grain problem: a
//! metric whose grain is not the base-table grain raised `RootGrainFanTrap` /
//! `MetricFanTrap` instead of aggregating over the multiplied join. These tests
//! pin the other half — those queries are now **answered**, by pre-aggregating
//! each metric at its own grain in a CTE and joining the results, the way
//! Snowflake computes them.
//!
//! The fence itself is unchanged for everything per-grain does not supersede: a
//! dimension BELOW a metric's grain still errors (see the `still_errors` tests),
//! and `fan_trap.rs`'s own unit tests keep pinning the base-anchored fence used
//! by the paths that are not per-grain eligible.

use super::*;
use crate::expand::test_helpers::{minimal_def, TestFixtureExt};
use crate::model::{Cardinality, NullsOrder, SemanticViewDefinition, SortOrder, WindowSpec};

/// Rename the base table declared by [`minimal_def`] so the fixture's physical
/// table name differs from its alias (`"orders" AS "o"`), the shape real DDL
/// produces — and the shape that makes an anchor assertion meaningful.
fn base_table(mut def: SemanticViewDefinition, table: &str, pk: &str) -> SemanticViewDefinition {
    def.tables[0].table = table.to_string();
    def.tables[0].pk_columns = vec![pk.to_string()];
    def
}

/// root `o` (orders) --customer_id--> `c` (customers).
///
/// `c` is a PARENT of the base table: the base-anchored `FROM orders LEFT JOIN
/// customers` duplicates each customer row once per order, so any metric on `c`
/// is inflated there (EXP-1). Its own grain is `customers`.
fn orders_with_parent_customers() -> SemanticViewDefinition {
    base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("c", "customers", &["id"])
        .with_dimension("segment", "c.segment", Some("c"))
        .with_dimension("order_status", "o.status", Some("o"))
        .with_metric("total_balance", "SUM(c.balance)", Some("c"))
        .with_metric("order_count", "COUNT(*)", Some("o"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"])
}

/// root `o` (orders) <--order_id-- `li` (line_items): `li` is a CHILD of the
/// base table (the "many" side). A metric on `o` and a metric on `li` are at
/// different grains separated by a fan-out edge — the classic two-fact-table
/// query, `MetricFanTrap` before v0.12.0.
fn orders_with_child_line_items() -> SemanticViewDefinition {
    base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &["id"])
        .with_dimension("order_status", "o.status", Some("o"))
        .with_metric("order_total", "SUM(o.amount)", Some("o"))
        .with_metric("item_count", "COUNT(li.id)", Some("li"))
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
}

/// The same two-fact shape, but the child table declares **no** PRIMARY KEY and
/// its metric is a bare `COUNT(*)` — the shape the base-anchored path cannot
/// emit safely (`CountStarRequiresPrimaryKey`, SG-8) but per-grain can, because
/// the child anchors its own CTE instead of being LEFT JOINed.
fn orders_with_pkless_child() -> SemanticViewDefinition {
    base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &[])
        .with_dimension("order_status", "o.status", Some("o"))
        .with_metric("order_total", "SUM(o.amount)", Some("o"))
        .with_metric("item_count", "COUNT(*)", Some("li"))
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
}

fn req(dims: &[&str], mets: &[&str]) -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    }
}

// ---------------------------------------------------------------------------
// GRAIN-01 / GRAIN-02: a metric on a parent of the base table
// ---------------------------------------------------------------------------

/// GRAIN-01: queried alone, a parent-table metric is aggregated over its OWN
/// table — not over `FROM orders LEFT JOIN customers` (which counts each
/// customer once per order). v0.11.0 raised `RootGrainFanTrap` here.
#[test]
fn parent_grain_metric_alone_aggregates_at_its_own_grain() {
    let def = orders_with_parent_customers();
    let sql = expand("sales", &def, &req(&[], &["total_balance"]))
        .expect("a parent-grain metric must be answerable at its own grain");
    assert!(
        sql.contains(r#"FROM "customers" AS "c""#),
        "must be anchored at the metric's own table, got:\n{sql}"
    );
    assert!(
        !sql.contains("orders"),
        "the base table must not appear — joining it is what inflated the metric:\n{sql}"
    );
    assert!(
        sql.contains(
            r#"SUM(CASE WHEN "c"."id" IS NOT NULL THEN c.balance END) AS "total_balance""#
        ),
        "must emit the metric expression, got:\n{sql}"
    );
}

/// GRAIN-02: grouping that metric by a dimension on its own table groups at its
/// own grain.
#[test]
fn parent_grain_metric_with_own_table_dimension() {
    let def = orders_with_parent_customers();
    let sql = expand("sales", &def, &req(&["segment"], &["total_balance"]))
        .expect("parent-grain metric + parent-grain dimension must be answerable");
    assert!(
        sql.contains(r#"FROM "customers" AS "c""#),
        "anchored at customers, got:\n{sql}"
    );
    assert!(
        sql.contains("GROUP BY"),
        "must group by the dimension:\n{sql}"
    );
    assert!(!sql.contains("orders"), "no base-table join:\n{sql}");
}

// ---------------------------------------------------------------------------
// GRAIN-03 / GRAIN-05: two metrics at different grains
// ---------------------------------------------------------------------------

/// GRAIN-03 + GRAIN-05: a base-grain metric and a parent-grain metric queried
/// together, with no dimensions: each is computed in its own CTE and the two
/// one-row results are combined with a CROSS JOIN.
#[test]
fn metrics_at_two_grains_no_dimensions_cross_join() {
    let def = orders_with_parent_customers();
    let sql = expand("sales", &def, &req(&[], &["order_count", "total_balance"]))
        .expect("two grains must be answerable per-grain");
    assert!(
        sql.starts_with("WITH __sv_grain_0 AS ("),
        "expected per-grain CTEs, got:\n{sql}"
    );
    assert!(
        sql.contains("__sv_grain_1"),
        "expected a second grain CTE, got:\n{sql}"
    );
    assert!(
        sql.contains("CROSS JOIN"),
        "dimensionless grain results combine with CROSS JOIN, got:\n{sql}"
    );
    assert!(
        !sql.contains("FULL OUTER JOIN"),
        "no dimensions to join on, got:\n{sql}"
    );
}

/// GRAIN-03: the two-fact-table shape — a metric on the base table and a metric
/// on a child table, grouped by a base-table dimension. Each grain aggregates
/// separately, then the results are joined on the dimension with a NULL-safe
/// comparison so a group present on only one side survives.
#[test]
fn metrics_at_two_grains_with_dimension_full_outer_join() {
    let def = orders_with_child_line_items();
    let sql = expand(
        "sales",
        &def,
        &req(&["order_status"], &["order_total", "item_count"]),
    )
    .expect("base-grain + child-grain metrics must be answerable per-grain");
    assert!(
        sql.contains("FULL OUTER JOIN"),
        "grain results join on the shared dimension, got:\n{sql}"
    );
    assert!(
        sql.contains("IS NOT DISTINCT FROM"),
        "the join key comparison must be NULL-safe, got:\n{sql}"
    );
    assert!(
        sql.contains("COALESCE"),
        "the output dimension must coalesce the per-grain keys, got:\n{sql}"
    );
    // The child grain reaches the base-table dimension through its FK — safe.
    assert!(
        sql.contains(r#"FROM "line_items" AS "li""#),
        "the child metric anchors its own CTE, got:\n{sql}"
    );
    assert!(
        sql.contains(r#"FROM "orders" AS "o""#),
        "the base-grain metric keeps its own CTE, got:\n{sql}"
    );
}

/// GRAIN-08: `COUNT(*)` on a table with no declared PRIMARY KEY is answerable
/// per-grain. The `CountStarRequiresPrimaryKey` guard exists only because the
/// base-anchored path LEFT JOINs the metric's table (NULL-extended rows would
/// be counted); when the table anchors its own CTE there is nothing to extend.
#[test]
fn count_star_without_primary_key_is_answerable_per_grain() {
    let def = orders_with_pkless_child();
    let sql = expand("sales", &def, &req(&[], &["order_total", "item_count"]))
        .expect("COUNT(*) at its own grain needs no PRIMARY KEY");
    assert!(
        sql.contains("COUNT(*)"),
        "the count stays a plain COUNT(*) at its own grain, got:\n{sql}"
    );
}

// ---------------------------------------------------------------------------
// GRAIN-06: single-grain queries are untouched
// ---------------------------------------------------------------------------

/// GRAIN-06: a query whose metrics all sit at the base grain keeps the flat,
/// base-anchored SQL — the per-grain path must not capture queries the existing
/// path already answers correctly.
#[test]
fn single_grain_query_keeps_flat_base_anchored_sql() {
    let def = orders_with_parent_customers();
    let sql = expand("sales", &def, &req(&["segment"], &["order_count"]))
        .expect("base-grain metric with a parent dimension is safe today");
    assert!(
        sql.starts_with("SELECT\n"),
        "must stay a flat SELECT, got:\n{sql}"
    );
    assert!(
        !sql.contains("__sv_grain"),
        "no per-grain CTEs for a single-grain query, got:\n{sql}"
    );
    assert!(
        sql.contains(r#"FROM "orders" AS "o""#) && sql.contains("LEFT JOIN"),
        "keeps the base-anchored join, got:\n{sql}"
    );
}

/// GRAIN-06: a child-grain metric alone is *already* correct base-anchored
/// (its rows are not multiplied by the LEFT JOIN to its parent), so it must not
/// be re-routed either.
#[test]
fn child_grain_metric_alone_keeps_flat_sql() {
    let def = orders_with_child_line_items();
    let sql = expand("sales", &def, &req(&["order_status"], &["item_count"]))
        .expect("a child-grain metric is safe base-anchored");
    assert!(
        !sql.contains("__sv_grain"),
        "single grain must not route per-grain, got:\n{sql}"
    );
}

// ---------------------------------------------------------------------------
// GRAIN-07: what per-grain does NOT make answerable
// ---------------------------------------------------------------------------

/// GRAIN-07: a dimension BELOW a metric's own grain still errors. Per-grain
/// aggregation cannot define this: the metric's rows genuinely fan across the
/// dimension's values (one order spans many line-item statuses). Snowflake
/// requires dimensions to be reachable through many-to-one relationships too.
#[test]
fn dimension_below_metric_grain_still_errors() {
    let def = orders_with_child_line_items().with_dimension("item_sku", "li.sku", Some("li"));
    let err = expand("sales", &def, &req(&["item_sku"], &["order_total"]))
        .expect_err("a dimension below the metric's grain must stay rejected");
    assert!(
        matches!(err, ExpandError::FanTrap { .. }),
        "expected FanTrap, got: {err}"
    );
}

/// GRAIN-07: the same rule applied to the parent-grain metric — grouping a
/// customer-grain metric by an order-grain dimension fans the customer rows.
#[test]
fn parent_grain_metric_with_base_dimension_still_errors() {
    let def = orders_with_parent_customers();
    let err = expand("sales", &def, &req(&["order_status"], &["total_balance"]))
        .expect_err("customer-grain metric grouped by an order-grain dimension must be rejected");
    assert!(
        matches!(err, ExpandError::FanTrap { .. }),
        "expected FanTrap, got: {err}"
    );
}

/// GRAIN-07: `OneToOne` edges never fan, so a query across a one-to-one
/// boundary stays on the flat path (nothing to fix, nothing to route).
#[test]
fn one_to_one_parent_metric_stays_flat() {
    let mut def = orders_with_parent_customers();
    def.joins[0].cardinality = Cardinality::OneToOne;
    let sql = expand("sales", &def, &req(&[], &["total_balance"]))
        .expect("a OneToOne parent is not fanned by the base-anchored FROM");
    assert!(
        !sql.contains("__sv_grain"),
        "OneToOne needs no per-grain rewrite, got:\n{sql}"
    );
}

// ---------------------------------------------------------------------------
// GRAIN-04: a derived metric whose components span grains
// ---------------------------------------------------------------------------

/// `ratio = order_total / item_count` fuses an order-grain aggregate and a
/// line-item-grain one. Its components are computed at their own grains and the
/// division is evaluated over the pre-aggregates. v0.11.0 raised
/// `MetricFanTrap` naming the derived metric.
#[test]
fn derived_metric_spanning_grains_is_assembled_from_components() {
    let def = orders_with_child_line_items().with_metric(
        "avg_order_size",
        "order_total / item_count",
        None,
    );
    let sql = expand("sales", &def, &req(&[], &["avg_order_size"]))
        .expect("a derived metric spanning grains must be answerable");
    assert!(
        sql.contains(r#"FROM "orders" AS "o""#) && sql.contains(r#"FROM "line_items" AS "li""#),
        "each component must be aggregated at its own grain, got:\n{sql}"
    );
    assert!(
        sql.contains(r#""__sv_grain_0"."__sv_m0" / "__sv_grain_1"."__sv_m0" AS "avg_order_size""#),
        "the derived expression must combine the two component columns, got:\n{sql}"
    );
    // The components are aggregates in the CTEs, never in the outer SELECT.
    let outer = sql.split_once("\nSELECT\n").expect("outer SELECT").1;
    assert!(
        !outer.contains("SUM(") && !outer.contains("COUNT("),
        "the outer SELECT combines pre-aggregates, it does not re-aggregate:\n{outer}"
    );
}

/// The same, grouped by a dimension both grains can reach.
#[test]
fn derived_metric_spanning_grains_with_dimension() {
    let def = orders_with_child_line_items().with_metric(
        "avg_order_size",
        "order_total / item_count",
        None,
    );
    let sql = expand("sales", &def, &req(&["order_status"], &["avg_order_size"]))
        .expect("derived multi-grain metric with a shared dimension must be answerable");
    assert!(
        sql.contains("FULL OUTER JOIN") && sql.contains("IS NOT DISTINCT FROM"),
        "components join on the shared dimension, got:\n{sql}"
    );
    assert!(
        sql.matches("GROUP BY").count() == 2,
        "each component CTE groups by the dimension, got:\n{sql}"
    );
}

/// A derived metric that reaches its multi-grain components through ANOTHER
/// derived metric: `nested = avg_order_size * 2` where `avg_order_size =
/// order_total / item_count`. The intermediate derived metric has no grain of
/// its own — it must be inlined over the same component columns rather than
/// left as a dangling reference.
#[test]
fn derived_metric_nested_over_multi_grain_components() {
    let def = orders_with_child_line_items()
        .with_metric("avg_order_size", "order_total / item_count", None)
        .with_metric("scaled", "avg_order_size * 2", None);
    let sql = expand("sales", &def, &req(&[], &["scaled"]))
        .expect("a nested derived metric over multi-grain components must be answerable");
    assert!(
        !sql.contains("avg_order_size *"),
        "the intermediate derived metric must be inlined, not referenced as a column:\n{sql}"
    );
    assert!(
        sql.contains(r#"("__sv_grain_0"."__sv_m0" / "__sv_grain_1"."__sv_m0") * 2 AS "scaled""#),
        "expected the inlined nested expression, got:\n{sql}"
    );
}

/// Two derived metrics over the same components, queried together: each must be
/// assembled independently — the shape that regressed when a single visited-set
/// was shared across the whole rebuild instead of guarding one recursion path.
#[test]
fn two_derived_metrics_over_the_same_components() {
    let def = orders_with_child_line_items()
        .with_metric("avg_order_size", "order_total / item_count", None)
        .with_metric("inverse", "item_count / order_total", None);
    let sql = expand("sales", &def, &req(&[], &["avg_order_size", "inverse"]))
        .expect("two derived multi-grain metrics must both be answerable");
    assert!(
        sql.contains(r#"AS "avg_order_size""#) && sql.contains(r#"AS "inverse""#),
        "both metrics must be emitted, got:\n{sql}"
    );
    assert!(
        !sql.contains("order_total / item_count") && !sql.contains("item_count / order_total"),
        "neither expression may survive with unresolved metric references:\n{sql}"
    );
}

/// A chain of intermediate derived metrics, where a later one references an
/// earlier one: `half = order_total / item_count`, `scaled = half * 2`,
/// `combined = scaled + half`. Every intermediate must be resolved down to
/// component columns on each path it appears on — a rebuild that remembers
/// "already inlined `half`" across sibling branches leaks the raw metric name
/// into the SQL, which then fails to bind.
#[test]
fn derived_metric_chain_resolves_every_reference_to_columns() {
    let def = orders_with_child_line_items()
        .with_metric("half", "order_total / item_count", None)
        .with_metric("scaled", "half * 2", None)
        .with_metric("combined", "scaled + half", None);
    let sql = expand("sales", &def, &req(&[], &["combined"]))
        .expect("a chain of derived metrics over multi-grain components must be answerable");
    for leaked in ["half", "scaled", "order_total", "item_count"] {
        assert!(
            !sql.contains(leaked),
            "metric name '{leaked}' leaked into the SQL unresolved — it is not a column:\n{sql}"
        );
    }
}

/// The grain CTEs must be numbered deterministically: the same definition and
/// request always produce byte-identical SQL. (Component collection walks a
/// name set; iterating it directly made group 0 the line-item grain on some
/// runs and the order grain on others.)
#[test]
fn per_grain_sql_is_deterministic() {
    let def = orders_with_child_line_items().with_metric(
        "avg_order_size",
        "order_total / item_count",
        None,
    );
    let first = expand("sales", &def, &req(&[], &["avg_order_size"])).expect("expands");
    for _ in 0..16 {
        assert_eq!(
            first,
            expand("sales", &def, &req(&[], &["avg_order_size"])).expect("expands"),
            "per-grain SQL must not vary between expansions of the same query"
        );
    }
}

// ---------------------------------------------------------------------------
// Sibling grains (the chasm trap): two child tables of one parent
// ---------------------------------------------------------------------------

/// root `o` with TWO children — `li` (line items) and `s` (shipments). Neither
/// is an ancestor of the other; the path between them runs through the root,
/// and the `o -> s` leg of it fans.
fn orders_with_two_children() -> SemanticViewDefinition {
    base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &["id"])
        .with_table("s", "shipments", &["id"])
        .with_dimension("order_status", "o.status", Some("o"))
        .with_dimension("carrier", "s.carrier", Some("s"))
        .with_metric("item_qty", "SUM(li.qty)", Some("li"))
        .with_metric("ship_weight", "SUM(s.weight)", Some("s"))
        .with_pkfk_join("item_to_order", "li", "o", &["order_id"], &["id"])
        .with_pkfk_join("shipment_to_order", "s", "o", &["order_id"], &["id"])
}

/// GRAIN-03: metrics on two SIBLING child tables — the chasm trap. Joined into
/// one query each multiplies the other (an order's 2 line items x 2 shipments =
/// 4 rows), so each must be aggregated at its own grain.
#[test]
fn sibling_grain_metrics_are_computed_per_grain() {
    let def = orders_with_two_children();
    let sql = expand("sales", &def, &req(&[], &["item_qty", "ship_weight"]))
        .expect("two sibling child grains must be answerable per-grain");
    assert!(
        sql.contains(r#"FROM "line_items" AS "li""#) && sql.contains(r#"FROM "shipments" AS "s""#),
        "each sibling anchors its own CTE, got:\n{sql}"
    );
    assert!(
        sql.contains("CROSS JOIN"),
        "one row per grain, no dimensions to join on, got:\n{sql}"
    );
}

/// The same pair grouped by a dimension on their shared parent — reachable from
/// both grains without fanning either.
#[test]
fn sibling_grain_metrics_with_parent_dimension() {
    let def = orders_with_two_children();
    let sql = expand(
        "sales",
        &def,
        &req(&["order_status"], &["item_qty", "ship_weight"]),
    )
    .expect("sibling grains grouped by a parent dimension must be answerable");
    assert!(
        sql.contains("FULL OUTER JOIN"),
        "grain results join on the shared dimension, got:\n{sql}"
    );
    // Each CTE reaches the parent dimension through its own FK.
    assert!(
        sql.matches(r#"LEFT JOIN "orders" AS "o""#).count() == 2,
        "each grain CTE joins the parent itself, got:\n{sql}"
    );
}

/// A metric on one child table grouped by a dimension on its SIBLING is a fan
/// trap — line-item rows are multiplied by the order's shipments before
/// aggregation — and must be rejected. The path between two siblings runs
/// through their shared parent, which the fence's parent-chain walk could not
/// express: neither sibling is an ancestor of the other, so the walk found no
/// path and the check silently passed.
#[test]
fn metric_grouped_by_sibling_dimension_errors() {
    let def = orders_with_two_children();
    let err = expand("sales", &def, &req(&["carrier"], &["item_qty"]))
        .expect_err("a dimension on a sibling child table fans the metric");
    assert!(
        matches!(err, ExpandError::FanTrap { .. }),
        "expected FanTrap, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// GRAIN-07: the documented residual boundary (TECH-DEBT #36)
// ---------------------------------------------------------------------------

/// GRAIN-12: a multi-grain query carrying an ACTIVE semi-additive metric is
/// computed — the snapshot becomes one grain group's CTE.
///
/// This is the shape TECH-DEBT #36 tracked to the end. Increment 1 re-anchored a
/// SINGLE snapshot CTE; here the snapshot is one group among several, nested
/// inside its grain CTE as `SELECT <outer> FROM (<inner>)` while the sibling
/// group aggregates normally and the two are joined. Probed against Snowflake:
/// it computes each metric at its own grain, snapshot included.
///
/// Supersedes `multi_grain_with_active_semi_additive_metric_still_errors`, which
/// asserted the fan-trap error for exactly this query. The boundary is gone
/// rather than moved this time, so the test is converted rather than re-pointed.
#[test]
fn multi_grain_with_active_semi_additive_metric_is_computed() {
    let def = orders_with_parent_customers()
        .with_dimension("snapshot_day", "c.as_of", Some("c"))
        .with_non_additive_by(
            "total_balance",
            &[("snapshot_day", SortOrder::Asc, NullsOrder::Last)],
        );
    // `snapshot_day` is NOT queried, so the metric is *active* semi-additive;
    // `order_count` sits at the base grain, so the query spans two.
    let sql = expand("sales", &def, &req(&[], &["order_count", "total_balance"]))
        .expect("a snapshot group and a plain group must be computable together");
    assert!(
        sql.contains("__sv_grain_0") && sql.contains("__sv_grain_1"),
        "expected one CTE per grain, got:\n{sql}"
    );
    assert!(
        sql.contains("RANK()"),
        "the semi-additive group keeps its snapshot shape, got:\n{sql}"
    );
    assert!(sql.contains("__sv_rn"), "and its rank column, got:\n{sql}");
    // The snapshot group anchors at customers; the base table must not be
    // joined into it, which is what inflated the balance before.
    assert!(
        sql.contains(r#"FROM "customers" AS "c""#),
        "snapshot group anchored at its own grain, got:\n{sql}"
    );
}

/// GRAIN-12, grouped: the same shape with a dimension both grains can reach.
#[test]
fn multi_grain_semi_additive_with_a_shared_dimension() {
    let def = orders_with_parent_customers()
        .with_dimension("snapshot_day", "c.as_of", Some("c"))
        .with_non_additive_by(
            "total_balance",
            &[("snapshot_day", SortOrder::Asc, NullsOrder::Last)],
        );
    let sql = expand(
        "sales",
        &def,
        &req(&["segment"], &["order_count", "total_balance"]),
    )
    .expect("a dimension above both grains must group the snapshot too");
    assert!(
        sql.contains("RANK()") && sql.contains("__sv_grain_1"),
        "snapshot inside a grain CTE, got:\n{sql}"
    );
    // The dimension must reach the outer coalesce, so the two grains join on it.
    assert!(
        sql.contains("COALESCE") || sql.contains("FULL OUTER JOIN"),
        "grains must be joined on the shared dimension, got:\n{sql}"
    );
}

/// A window metric whose inner aggregate lives at a NON-ROOT grain is answered
/// by anchoring `__sv_agg` at that grain (TECH-DEBT #36, first sub-item).
///
/// `total_balance` is `SUM(c.balance)` on the parent `customers`; `segment` is
/// also on `customers`. Base-anchoring the CTE at `orders` would join each
/// customer once per order and sum the balance that many times — the inflation
/// the v0.11.0 fence turned into `RootGrainFanTrap`. Anchored at `c` the inner
/// aggregate sees one row per customer, and the window function then runs over
/// the CTE exactly as before: only the inner aggregate is grain-sensitive.
///
/// The window metric is declared QUALIFIED (`o.running_balance`), the shape real
/// DDL always produces — `metric_grain` unions that alias with the
/// inner's, so an anchor derived from it would see two grains and decline for
/// every DDL-declared window metric. Using `source_table: None` here would have
/// passed while the feature did nothing through actual DDL.
#[test]
fn multi_grain_window_metric_anchors_the_cte_at_its_own_grain() {
    let def = orders_with_parent_customers()
        .with_metric("running_balance", "SUM(total_balance)", Some("o"))
        .with_window_spec(
            "running_balance",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "total_balance".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["segment".to_string()],
                order_by: vec![],
                frame_clause: None,
            },
        );
    let sql = expand("sales", &def, &req(&["segment"], &["running_balance"]))
        .expect("a window metric at its own grain is answerable");

    // The CTE is anchored at `customers`, NOT at the base table `orders`.
    assert!(
        sql.contains("FROM \"customers\" AS \"c\""),
        "__sv_agg must anchor at the inner metric's own grain: {sql}"
    );
    assert!(
        !sql.contains("\"orders\""),
        "the base table must not appear — joining it is what inflated the sum: {sql}"
    );
    // The window function still runs over the CTE, unchanged.
    assert!(
        sql.contains("__sv_agg"),
        "the window CTE shape is retained: {sql}"
    );
    assert!(
        sql.contains("OVER (PARTITION BY"),
        "the OVER clause still partitions: {sql}"
    );
}

/// A window metric already at the ROOT grain is left exactly as it was — the
/// base-anchored CTE is already correct there, so the anchor decision declines
/// and the emitted SQL is unchanged. Guards against re-anchoring queries that
/// never needed it.
#[test]
fn root_grain_window_metric_stays_base_anchored() {
    let def = orders_with_parent_customers()
        .with_metric("running_orders", "SUM(order_count)", Some("o"))
        .with_window_spec(
            "running_orders",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "order_count".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["order_status".to_string()],
                order_by: vec![],
                frame_clause: None,
            },
        );
    let sql = expand("sales", &def, &req(&["order_status"], &["running_orders"]))
        .expect("a root-grain window metric was always answerable");
    assert!(
        sql.contains(r#"FROM "orders" AS "o""#),
        "must stay anchored at the base table: {sql}"
    );
}

/// A window metric whose inner aggregate is on a CHILD of the base table is
/// anchored at that child (TECH-DEBT #71).
///
/// This was the opposite guard: `FROM orders LEFT JOIN line_items` kept an
/// order with no line items as a NULL-extended row whose `COUNT` is 0, and
/// `FROM line_items LEFT JOIN orders` drops it — which
/// `cr20260718_quoted_metric_window.test` caught as its childless EU order
/// disappearing. Snowflake settled (D1) that the disappearance is right: the
/// inner metric queried on its own has no row for that order, and the window
/// must run over the same groups the inner metric has. The base-anchored CTE
/// also lost every line item whose `order_id` is NULL. Numbers:
/// `tests_home_anchoring::window_over_a_child_metric_is_home_anchored`.
#[test]
fn window_metric_on_a_child_table_is_anchored_at_the_child() {
    let def = orders_with_child_line_items()
        .with_metric("rolling_items", "SUM(item_count)", Some("li"))
        .with_window_spec(
            "rolling_items",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "item_count".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["order_status".to_string()],
                order_by: vec![],
                frame_clause: None,
            },
        );
    let sql = expand("sales", &def, &req(&["order_status"], &["rolling_items"]))
        .expect("a child-grain window metric was always answerable");
    assert!(
        sql.contains("FROM \"line_items\" AS \"li\"\n    LEFT JOIN \"orders\" AS \"o\""),
        "__sv_agg must anchor at the inner metric's own table and join OUT: {sql}"
    );
}

/// The boundary this increment keeps: window metrics whose inner aggregates sit
/// at DIFFERENT grains would need those grains joined before the window runs,
/// which the single-anchor CTE cannot express. Declined, so the fence still
/// reports it rather than emitting a silently wrong shape.
#[test]
fn window_metrics_at_two_different_grains_still_error() {
    let def = orders_with_parent_customers()
        .with_metric("running_balance", "SUM(total_balance)", Some("o"))
        .with_window_spec(
            "running_balance",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "total_balance".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["segment".to_string()],
                order_by: vec![],
                frame_clause: None,
            },
        )
        .with_metric("running_orders", "SUM(order_count)", Some("o"))
        .with_window_spec(
            "running_orders",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "order_count".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["segment".to_string()],
                order_by: vec![],
                frame_clause: None,
            },
        );
    let err = expand(
        "sales",
        &def,
        &req(&["segment"], &["running_balance", "running_orders"]),
    )
    .expect_err("inner aggregates at two grains are not answerable by one anchored CTE");
    assert!(
        matches!(
            err,
            ExpandError::RootGrainFanTrap { .. } | ExpandError::MetricFanTrap { .. }
        ),
        "expected a fan-trap error, got: {err}"
    );
}

/// A metric carrying `USING` role-playing context is not per-grain eligible
/// either: which role a grain CTE should join is exactly what `USING` answers on
/// the base-anchored path, and the grain CTEs do not carry that context.
#[test]
fn multi_grain_with_role_playing_still_errors() {
    // `orders` reaches `customers` through TWO named relationships — the
    // role-playing shape.
    let def = orders_with_parent_customers().with_pkfk_join(
        "o_to_billing_c",
        "o",
        "c",
        &["billing_customer_id"],
        &["id"],
    );
    let err = expand("sales", &def, &req(&[], &["total_balance"]))
        .expect_err("role-playing is not per-grain eligible");
    assert!(
        matches!(err, ExpandError::RootGrainFanTrap { .. }),
        "expected the v0.11.0 fan-trap error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Pre-aggregation `where_clause` on the per-grain path
//
// The predicate goes into EACH grain CTE, so every metric aggregates over only
// the matching rows. On the outer query it would filter the already-combined
// result -- and because the grains are joined FULL OUTER, that would drop whole
// groups rather than recompute them.
// ---------------------------------------------------------------------------

/// The two-grain shape plus a `customers` parent that neither metric anchors,
/// so a predicate on `segment` forces every grain CTE to join it.
fn two_grains_with_unqueried_parent() -> SemanticViewDefinition {
    orders_with_child_line_items()
        .with_table("c", "customers", &["id"])
        .with_dimension("segment", "c.segment", Some("c"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"])
}

/// The bodies of the grain CTEs, excluding the outer query (which also mentions
/// `__sv_grain_N`, so splitting on the CTE name alone would over-match).
fn grain_cte_bodies(sql: &str) -> Vec<&str> {
    sql.split(" AS (\n").skip(1).collect()
}

#[test]
fn where_clause_is_injected_into_every_grain_cte() {
    let def = two_grains_with_unqueried_parent();
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("order_status")],
        metrics: vec![
            MetricName::new("order_total"),
            MetricName::new("item_count"),
        ],
        facts: vec![],
        where_clause: Some("segment = 'ENTERPRISE'".to_string()),
    };
    let sql = expand("test_view", &def, &req).unwrap();

    let bodies = grain_cte_bodies(&sql);
    assert_eq!(bodies.len(), 2, "expected two grain CTEs: {sql}");
    for (i, body) in bodies.iter().enumerate() {
        let where_at = body.find("WHERE (c.segment) = 'ENTERPRISE'");
        let group_at = body.find("GROUP BY");
        assert!(
            where_at.is_some(),
            "grain {i} missing the predicate: {body}"
        );
        assert!(
            group_at.is_some() && where_at < group_at,
            "grain {i}: WHERE must precede GROUP BY so it filters the rows going \
             INTO the aggregation: {body}"
        );
    }

    // The outer query must NOT carry it -- there it would be post-aggregation.
    let outer = sql.rsplit_once(")\n").expect("a closing CTE").1;
    assert!(
        !outer.contains("WHERE"),
        "predicate must not leak onto the outer query: {outer}"
    );
}

#[test]
fn where_clause_joins_its_table_into_every_grain_cte() {
    // `c` is referenced only by the predicate, and the `li` grain reaches it
    // only via li -> o -> c, so this also pins the multi-hop join path.
    let def = two_grains_with_unqueried_parent();
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("order_status")],
        metrics: vec![
            MetricName::new("order_total"),
            MetricName::new("item_count"),
        ],
        facts: vec![],
        where_clause: Some("segment = 'ENTERPRISE'".to_string()),
    };
    let sql = expand("test_view", &def, &req).unwrap();
    for (i, body) in grain_cte_bodies(&sql).iter().enumerate() {
        assert!(
            body.contains("\"customers\" AS \"c\""),
            "grain {i} must join customers to evaluate the filter: {body}"
        );
    }
}

#[test]
fn where_clause_on_a_single_grain_plan_filters_before_aggregation() {
    let def = orders_with_parent_customers();
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("segment")],
        metrics: vec![MetricName::new("total_balance")],
        facts: vec![],
        where_clause: Some("segment <> 'CHURNED'".to_string()),
    };
    let sql = expand("test_view", &def, &req).unwrap();
    let where_at = sql.find("WHERE (c.segment) <> 'CHURNED'");
    assert!(where_at.is_some(), "predicate must be emitted: {sql}");
    if let Some(group_at) = sql.find("GROUP BY") {
        assert!(where_at < Some(group_at), "WHERE before GROUP BY: {sql}");
    }
}

/// A PARENT of the base table whose metric is a bare `COUNT(*)` and which
/// declares `UNIQUE (id)` rather than `PRIMARY KEY (id)` — the SG-8 shape, one
/// grain UP instead of one grain down.
///
/// The `UNIQUE`-not-`PRIMARY KEY` detail is what makes this shape reachable
/// through real DDL, and it is load-bearing. D-06 rejects a table that an FK
/// references unless it declares a PRIMARY KEY **or** a UNIQUE, so a parent with
/// neither cannot be created at all (`65_pk_error.test`). `UNIQUE` satisfies
/// D-06, and it also makes `c` the "one" side that [`window_cte_anchor`]
/// re-anchors toward — while leaving `pk_columns` empty, so the
/// `COUNT(*)` → `COUNT(<pk>)` rewrite is impossible and SG-8 records the metric.
///
/// Declaring `c` with neither constraint would be a fixture DDL cannot produce —
/// the same trap that made the first cut of this feature a no-op through actual
/// DDL, so it is spelled out rather than left implicit.
fn orders_with_unique_only_parent() -> SemanticViewDefinition {
    let mut def = base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("c", "customers", &[])
        .with_dimension("segment", "c.segment", Some("c"))
        .with_dimension("order_status", "o.status", Some("o"))
        .with_metric("customer_count", "COUNT(*)", Some("c"))
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"]);
    let c = def
        .tables
        .iter_mut()
        .find(|t| t.alias == "c")
        .expect("the fixture declares `c`");
    c.unique_constraints = vec![vec!["id".to_string()]];
    def
}

/// A `SUM(<inner>) OVER (PARTITION BY <dim>)` window spec.
fn sum_over(inner: &str, partition_dim: &str) -> WindowSpec {
    WindowSpec {
        window_function: "SUM".to_string(),
        inner_metric: inner.to_string(),
        extra_args: vec![],
        excluding_dims: vec![],
        partition_dims: vec![partition_dim.to_string()],
        order_by: vec![],
        frame_clause: None,
    }
}

/// SG-8 must not reject a query the anchor makes safe (PR #175 review).
///
/// `customer_count` is a bare `COUNT(*)` on the PK-less parent `customers`.
/// Base-anchored, `FROM orders LEFT JOIN customers` would count NULL-extended
/// rows, which is exactly what `CountStarRequiresPrimaryKey` exists to prevent —
/// but the anchored CTE is `FROM customers`, where `COUNT(*)` counts customer
/// rows exactly and needs no PRIMARY KEY, the same reasoning that already
/// exempts the per-grain path.
///
/// This regressed on nothing — it was never answerable — but the guard ran
/// before `window_anchor` was computed, so a subset of the shape this change set
/// out to support still errored. Verified red before the fix, with
/// `CountStarRequiresPrimaryKey`.
#[test]
fn anchored_window_count_star_needs_no_primary_key() {
    let def = orders_with_unique_only_parent()
        .with_metric("running_customers", "SUM(customer_count)", Some("o"))
        .with_window_spec("running_customers", sum_over("customer_count", "segment"));
    let sql = expand("sales", &def, &req(&["segment"], &["running_customers"]))
        .expect("COUNT(*) at the anchored grain needs no PRIMARY KEY");
    assert!(
        sql.contains("FROM \"customers\" AS \"c\""),
        "the CTE must anchor at the counted table: {sql}"
    );
    assert!(
        sql.contains("COUNT(*)"),
        "the count stays a plain COUNT(*) at its own grain: {sql}"
    );
    assert!(
        !sql.contains("\"orders\""),
        "joining the base table is what would have inflated the count: {sql}"
    );
}

/// The invariant that makes the SG-8 bypass above sound, and the reason it is a
/// bypass rather than a deletion.
///
/// A dimension BELOW the anchor's grain pulls a "many"-side join back into the
/// anchored CTE (`FROM customers JOIN orders`), where `COUNT(*)` would count
/// orders rather than customers. The retained metric × dimension fan-trap check
/// rejects that shape before emission, so skipping SG-8 cannot turn a loud error
/// into a silently inflated number — it only changes WHICH error is reported
/// (`CountStarRequiresPrimaryKey` before, `FanTrap` now). Were that check ever
/// dropped, this test emits inflated SQL and fails.
#[test]
fn anchored_window_count_star_below_its_grain_still_errors() {
    let def = orders_with_unique_only_parent()
        .with_metric("running_customers", "SUM(customer_count)", Some("o"))
        .with_window_spec(
            "running_customers",
            sum_over("customer_count", "order_status"),
        );
    let err = expand(
        "sales",
        &def,
        &req(&["order_status"], &["running_customers"]),
    )
    .expect_err("a dimension below the anchor grain re-fans the CTE");
    assert!(
        matches!(
            err,
            ExpandError::FanTrap { .. } | ExpandError::MetricFanTrap { .. }
        ),
        "expected a fan-trap error, got: {err}"
    );
}

/// The same invariant for an inner `SUM` on a PK-ful parent, independent of
/// SG-8: a dimension below the anchor's grain must error rather than emit a CTE
/// whose fanning join re-inflates the very aggregate this change set out to fix.
#[test]
fn anchored_window_sum_below_its_grain_still_errors() {
    let def = orders_with_parent_customers()
        .with_metric("running_balance", "SUM(total_balance)", Some("o"))
        .with_window_spec("running_balance", sum_over("total_balance", "order_status"));
    let err = expand("sales", &def, &req(&["order_status"], &["running_balance"]))
        .expect_err("a dimension below the anchor grain re-fans the inner SUM");
    assert!(
        matches!(
            err,
            ExpandError::FanTrap { .. } | ExpandError::MetricFanTrap { .. }
        ),
        "expected a fan-trap error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// GRAIN-09: role-playing is a per-QUERY question, not a per-definition one
// ---------------------------------------------------------------------------

/// Base `f` (flights) reaches `a` (airports) through TWO named relationships —
/// the role-playing shape — and separately reaches an unrelated parent `c`
/// (carriers) through one.
fn flights_with_role_playing_airports() -> SemanticViewDefinition {
    base_table(minimal_def("f", "d", "d", "m", "count(*)"), "flights", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("a", "airports", &["code"])
        .with_table("c", "carriers", &["id"])
        .with_dimension("dep_city", "a.city", Some("a"))
        .with_dimension("carrier_name", "c.name", Some("c"))
        .with_metric("flight_count", "COUNT(*)", Some("f"))
        .with_metric("fleet_size", "SUM(c.fleet)", Some("c"))
        .with_pkfk_join("dep", "f", "a", &["dep_code"], &["code"])
        .with_pkfk_join("arr", "f", "a", &["arr_code"], &["code"])
        .with_pkfk_join("f_to_c", "f", "c", &["carrier_id"], &["id"])
}

/// GRAIN-09: role-playing that the query never reaches must not cost it
/// per-grain emission.
///
/// Two grains (`f` and `c`) and a dimension on `c`: nothing here touches the
/// role-played `airports`, so no grain CTE joins anything ambiguous and no role
/// context is needed. The eligibility test used to ask a definition-level
/// question — "does any table have two inbound relationships from one source?" —
/// so every query against this definition lost per-grain emission and got the
/// v0.11.0 fan-trap error, however unrelated its grains were.
#[test]
fn role_playing_elsewhere_in_the_definition_does_not_block_per_grain() {
    let def = flights_with_role_playing_airports();
    let sql = expand(
        "flights_sv",
        &def,
        &req(&["carrier_name"], &["flight_count", "fleet_size"]),
    )
    .expect("role-playing the query never reaches must not block per-grain");
    assert!(
        sql.contains("__sv_grain_0") && sql.contains("__sv_grain_1"),
        "expected one CTE per grain, got:\n{sql}"
    );
    assert!(
        sql.contains(r#"FROM "carriers" AS "c""#),
        "the carrier-grain metric must anchor at its own table, got:\n{sql}"
    );
    assert!(
        !sql.contains("airports"),
        "the role-played table is not part of this query and must not be joined:\n{sql}"
    );
}

/// The single-grain half of the same fix, reported independently so the
/// sqllogictest file's halt-at-first-failure cannot leave it unproven: the
/// carrier-grain metric queried ALONE with its own dimension is a one-CTE
/// per-grain query, and it was declined by the definition-level test too.
#[test]
fn role_playing_elsewhere_does_not_block_a_single_grain_query() {
    let def = flights_with_role_playing_airports();
    let sql = expand("flights_sv", &def, &req(&["carrier_name"], &["fleet_size"]))
        .expect("a single-grain parent metric must be answerable here too");
    assert!(
        sql.contains(r#"FROM "carriers" AS "c""#),
        "anchored at the metric's own table, got:\n{sql}"
    );
    assert!(
        !sql.contains("flights") && !sql.contains("airports"),
        "neither the base table nor the role-played table belongs in this CTE:\n{sql}"
    );
}

/// The boundary the narrowed test keeps: when the query DOES reach the
/// role-played table, per-grain still declines.
///
/// `dep_city` is on `airports`, reachable by both `dep` and `arr`, and no
/// co-queried metric carries `USING` to say which. A grain CTE would have to
/// pick an edge, and picking one silently is the declaration-order-dependent
/// mis-binding the fence exists to prevent — so the query keeps its error until
/// `USING` context is threaded into `anchor_joins`.
#[test]
fn dimension_on_a_role_playing_target_still_declines_per_grain() {
    let def = flights_with_role_playing_airports();
    let err = expand(
        "flights_sv",
        &def,
        &req(&["dep_city"], &["flight_count", "fleet_size"]),
    )
    .expect_err("an unscoped dimension on a role-playing target is still declined");
    // Since Phase 6.3 the base-anchored path reports the missing role before the
    // fan trap between the two metrics (it used to report the fan trap).
    assert!(
        matches!(err, ExpandError::AmbiguousPath { .. }),
        "expected the ambiguous-path error, got: {err}"
    );
}

/// A role-playing definition that also has a CHILD of the base table, so a
/// query can have two grains while the role-played parent `a` sits ABOVE both —
/// the shape where the where-clause fan-trap check does not fire.
fn flights_with_role_playing_and_child_legs() -> SemanticViewDefinition {
    base_table(minimal_def("f", "d", "d", "m", "count(*)"), "flights", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("a", "airports", &["code"])
        .with_table("l", "legs", &["id"])
        .with_dimension("dep_city", "a.city", Some("a"))
        .with_dimension("flight_status", "f.status", Some("f"))
        .with_metric("flight_count", "COUNT(*)", Some("f"))
        .with_metric("leg_count", "COUNT(l.id)", Some("l"))
        .with_pkfk_join("dep", "f", "a", &["dep_code"], &["code"])
        .with_pkfk_join("arr", "f", "a", &["arr_code"], &["code"])
        .with_pkfk_join("l_to_f", "l", "f", &["flight_id"], &["id"])
}

/// A `where_clause` predicate is joined into every grain CTE, so its tables are
/// as "touched" by the query as a dimension's — and must be tested for
/// role-playing the same way.
///
/// Two grains (`f`, `l`) with the role-played `a` ABOVE both, so the
/// where-clause fan-trap check does not fire and nothing else stands in the way.
/// The gate's first cut looked only at dimension source tables and metric grain
/// tables, so this query was ruled eligible, and `anchor_joins` then walked
/// `f -> a` through `edge_between`, which returns the FIRST declared edge of the
/// two. Both grain CTEs silently joined `dep` and filtered on departure city.
///
/// The name `dep_city` makes that look intentional; it is not. The dimension is
/// `a.city`, and which role it bound to was decided by declaration order — swap
/// `dep` and `arr` in the DDL and the same query filters on arrival city. That
/// is the declaration-order-dependent mis-binding this gate exists to prevent,
/// and answering it silently is strictly worse than the `MetricFanTrap` this
/// shape raised before per-grain ever looked at it.
#[test]
fn where_clause_reaching_a_role_played_table_declines_per_grain() {
    let def = flights_with_role_playing_and_child_legs();
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("flight_status")],
        metrics: vec![
            MetricName::new("flight_count"),
            MetricName::new("leg_count"),
        ],
        facts: vec![],
        where_clause: Some("dep_city = 'London'".to_string()),
    };
    let err = expand("flights_sv", &def, &req)
        .expect_err("a predicate on a role-played table must not be answered by guesswork");
    assert!(
        matches!(
            err,
            ExpandError::MetricFanTrap { .. }
                | ExpandError::FanTrap { .. }
                | ExpandError::RootGrainFanTrap { .. }
                | ExpandError::WhereClauseFanTrap { .. }
        ),
        "expected the base-anchored fence's error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// GRAIN-10: USING names which role a grain CTE joins
// ---------------------------------------------------------------------------

/// `f` (flights) base; `a` (airports) role-played via dep/arr and ABOVE both
/// metric grains; `l` (legs) a child of `f`.
fn flights_using_fixture() -> SemanticViewDefinition {
    base_table(minimal_def("f", "d", "d", "m", "count(*)"), "flights", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("a", "airports", &["code"])
        .with_table("l", "legs", &["id"])
        .with_dimension("dep_city", "a.city", Some("a"))
        .with_dimension("flight_status", "f.status", Some("f"))
        .with_metric("flight_count", "COUNT(*)", Some("f"))
        .with_metric("leg_count", "COUNT(l.id)", Some("l"))
        .with_pkfk_join("dep", "f", "a", &["dep_code"], &["code"])
        .with_pkfk_join("arr", "f", "a", &["arr_code"], &["code"])
        .with_pkfk_join("l_to_f", "l", "f", &["flight_id"], &["id"])
        .with_using_relationship("flight_count", &["dep"])
}

/// `flights_using_fixture` plus `arrival_count`, a base-grain metric whose
/// `USING` names the OTHER role of `a`.
fn flights_two_role_fixture() -> SemanticViewDefinition {
    flights_using_fixture()
        .with_metric("arrival_count", "COUNT(*)", Some("f"))
        .with_using_relationship("arrival_count", &["arr"])
}

/// GRAIN-10, Phase 6.3: each grain group joins the role ITS metrics' `USING`
/// names and emits the dimension against that scoped alias.
///
/// The base-anchored path does this for one role: it emits `a__dep.city` and
/// joins `"airports" AS "a__dep" ON "f"."dep_code" = "a__dep"."code"`. Metrics
/// naming different roles are different groups, so each CTE has to reproduce
/// that shape for its own role, choosing the NAMED edge rather than whichever
/// `edge_between` happens to return first.
#[test]
fn using_scopes_a_role_played_dimension_in_every_grain_cte() {
    let def = flights_two_role_fixture();
    let sql = expand(
        "flights_sv",
        &def,
        &req(&["dep_city"], &["flight_count", "arrival_count"]),
    )
    .expect("each metric names its role, so each grain CTE can join it unambiguously");
    assert!(
        sql.contains("__sv_grain_0") && sql.contains("__sv_grain_1"),
        "expected one CTE per role, got:\n{sql}"
    );
    assert_eq!(
        sql.matches(r#"AS "a__dep""#).count(),
        1,
        "the departure role is joined once, in its own CTE, got:\n{sql}"
    );
    assert_eq!(
        sql.matches(r#"AS "a__arr""#).count(),
        1,
        "the arrival role is joined once, in its own CTE, got:\n{sql}"
    );
    assert!(
        sql.contains("a__dep.city") && sql.contains("a__arr.city"),
        "each CTE emits the dimension against its own scoped alias, got:\n{sql}"
    );
}

/// Phase 6.3 (Snowflake parity): a co-queried metric's `USING` no longer lends
/// its role to a metric that names none. `leg_count` cannot name one at all —
/// a `USING` relationship must start at the metric's own table — so grouping it
/// by departure city is refused, where it used to borrow `flight_count`'s role
/// and change its numbers with its companion.
#[test]
fn a_metric_without_a_role_does_not_borrow_a_co_queried_one() {
    let def = flights_using_fixture();
    let err = expand(
        "flights_sv",
        &def,
        &req(&["dep_city"], &["flight_count", "leg_count"]),
    )
    .expect_err("leg_count names no path to the role-played table");
    assert!(
        matches!(err, ExpandError::AmbiguousPath { .. }),
        "expected AmbiguousPath, got: {err}"
    );
}

/// NOT a new capability — a guard that the widened routing does not hijack a
/// query the base-anchored path already answered correctly.
///
/// `flight_count` alone sits at the BASE grain, so no per-grain treatment is
/// needed and `plan` returns `None`; the base-anchored path emits `a__dep.city`
/// as it always has. Verified by reverting the eligibility relaxation and
/// watching this still pass, which is exactly why it carries no "was declined
/// before" claim: it never was.
#[test]
fn single_grain_role_played_dimension_stays_on_the_base_anchored_path() {
    let def = flights_using_fixture();
    let sql = expand("flights_sv", &def, &req(&["dep_city"], &["flight_count"]))
        .expect("USING names the role for a one-grain query too");
    assert!(
        sql.contains("a__dep.city"),
        "the dimension binds to the named role, got:\n{sql}"
    );
    assert!(
        !sql.contains("arr_code"),
        "the arrival role was not named and must not be joined:\n{sql}"
    );
}

/// Guard: `USING` rescues a DIMENSION on a role-played table, not a
/// `where_clause` member on one.
///
/// Only a dimension's expression is rewritten to the scoped alias; the
/// predicate's is not. Answering this would bind the filter by declaration
/// order, which is the regression PR #176 shipped and then fixed — this pins
/// that the role-threading increment does not re-open it in a new shape.
#[test]
fn using_does_not_rescue_a_where_clause_on_the_role_played_table() {
    // Two metrics each naming its own role (Phase 6.3), so the predicate is the
    // only thing standing between this query and a per-grain answer: without
    // the fence each grain CTE would join the bare `a` for it by declaration
    // order.
    let def = flights_two_role_fixture();
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("dep_city")],
        metrics: vec![
            MetricName::new("flight_count"),
            MetricName::new("arrival_count"),
        ],
        facts: vec![],
        where_clause: Some("dep_city = 'London'".to_string()),
    };
    let err = expand("flights_sv", &def, &req)
        .expect_err("a predicate on a role-played table is not scoped by USING");
    assert!(
        matches!(
            err,
            ExpandError::AmbiguousPath { .. }
                | ExpandError::MetricFanTrap { .. }
                | ExpandError::FanTrap { .. }
                | ExpandError::RootGrainFanTrap { .. }
                | ExpandError::WhereClauseFanTrap { .. }
        ),
        "expected the base-anchored path's error, got: {err}"
    );
}

/// A metric whose `USING` names a relationship to a table reached only ONE way.
///
/// This was a guard that the per-grain path declined such a query (it errored
/// with the fan-trap fence). TECH-DEBT #71 deliberately widens it: a `USING`
/// relationship whose target no queried dimension or `where_clause` member sits
/// on scopes nothing — here `l_to_f` is the ONLY edge from `l` to `f`, so the
/// grain CTE's join through it is the join `USING` names — and declining kept
/// the child metric off its home table, so its value depended on its companions
/// (MM-3). The CTE must still join `f` through that edge.
///
/// Grouped by a dimension on `c`, a parent of `f`, rather than on the
/// role-played `a`: since Phase 6.3 a metric must name its own role to be
/// grouped by `a`, and `leg_count` cannot (see
/// `a_metric_without_a_role_does_not_borrow_a_co_queried_one`).
#[test]
fn using_naming_a_non_role_played_relationship_is_honoured_per_grain() {
    let def = flights_using_fixture()
        .with_table("c", "carriers", &["id"])
        .with_dimension("carrier_name", "c.name", Some("c"))
        .with_pkfk_join("f_to_c", "f", "c", &["carrier_id"], &["id"])
        .with_using_relationship("leg_count", &["l_to_f"]);
    let sql = expand(
        "flights_sv",
        &def,
        &req(&["carrier_name"], &["flight_count", "leg_count"]),
    )
    .expect("USING on the only relationship to a table scopes nothing ambiguous");
    assert!(
        sql.contains(
            "FROM \"legs\" AS \"l\"\n    LEFT JOIN \"flights\" AS \"f\" ON \"l\".\"flight_id\" = \"f\".\"id\"\n    \
             LEFT JOIN \"carriers\" AS \"c\""
        ),
        "the leg grain must reach the dimension through l_to_f:\n{sql}"
    );
}

/// The GRAIN-10 shape with the `dep` relationship declared QUOTED (`"Dep"`,
/// stored with its quotes) while `USING` names it bare. One identifier to
/// DuckDB, so the per-grain path must thread the same role: `scoped_roles`,
/// the `using_is_threaded` gate and the grain-CTE join emission all matched
/// relationship names by ASCII-lowercase spelling and so found no role.
#[test]
fn using_scopes_a_quoted_relationship_role_in_every_grain_cte() {
    let mut def = flights_two_role_fixture();
    def.joins[0].name = Some("\"Dep\"".to_string());
    let sql = expand(
        "flights_sv",
        &def,
        &req(&["dep_city"], &["flight_count", "arrival_count"]),
    )
    .expect("USING names the quoted role, so the grain CTEs can join it");
    assert_eq!(
        sql.matches(r#"AS "a__dep""#).count(),
        1,
        "the departure CTE joins the scoped role exactly once, got:\n{sql}"
    );
    assert!(sql.contains("a__dep.city"), "{sql}");
    assert_eq!(
        sql.matches("dep_code").count(),
        1,
        "only the departure CTE joins the departure edge:\n{sql}"
    );
}

/// A pre-#249 stored row (PR #249 review) holding a role-playing pair whose
/// names differ only by quoting: `dep` (departure) and `"dep"` (arrival).
/// `USING ("dep")` names the ARRIVAL edge, but both roles share the scoped
/// alias `a__dep`, so `scoped_roles` records the first-declared `dep` for
/// table `a`. The per-grain gate compared that role's spelling with the
/// `USING` spelling and declined, leaving the query to the base-anchored path;
/// matching them as one identifier instead threads the query here, where the
/// grain CTEs join the DEPARTURE edge — the wrong role, silently.
#[test]
fn legacy_quoted_pair_using_does_not_thread_the_other_role_per_grain() {
    let mut def = flights_using_fixture();
    def.joins[1].name = Some("\"dep\"".to_string());
    def.metrics[0].using_relationships = vec!["\"dep\"".to_string()];
    let result = expand(
        "flights_sv",
        &def,
        &req(&["dep_city"], &["flight_count", "leg_count"]),
    );
    if let Ok(sql) = &result {
        assert!(
            !sql.contains("dep_code"),
            "USING (\"dep\") names the arrival edge; the departure edge must not be joined:\n{sql}"
        );
    }
    let err = result.expect_err("the base-anchored path declines this shape, as before #249");
    // Since Phase 6.3 `leg_count`, which names no role, is reported first.
    assert!(
        matches!(
            err,
            ExpandError::AmbiguousPath { .. }
                | ExpandError::MetricFanTrap { .. }
                | ExpandError::FanTrap { .. }
                | ExpandError::RootGrainFanTrap { .. }
        ),
        "expected the base-anchored path's error, got: {err}"
    );
}

/// root `f` (flights) --carrier_id--> `c` (carriers) --dep/arr--> `a` (airports).
///
/// `c` is a PARENT of the base table, so a metric on it needs per-grain
/// treatment; `a` is a parent of `c`, so a dimension on it is ABOVE the metric's
/// grain and the fan-trap fence permits it. That combination is what reaches
/// `render_single_grain` — one grain group — with a role resolved.
fn carriers_via_role_played_airports() -> SemanticViewDefinition {
    base_table(minimal_def("f", "d", "d", "m", "count(*)"), "flights", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("a", "airports", &["code"])
        .with_table("c", "carriers", &["id"])
        .with_dimension("dep_city", "a.city", Some("a"))
        .with_metric("fleet_size", "SUM(c.fleet)", Some("c"))
        .with_pkfk_join("dep", "c", "a", &["dep_code"], &["code"])
        .with_pkfk_join("arr", "c", "a", &["arr_code"], &["code"])
        .with_pkfk_join("f_to_c", "f", "c", &["carrier_id"], &["id"])
        .with_using_relationship("fleet_size", &["dep"])
}

/// A SINGLE per-grain CTE must scope its dimension too.
///
/// One grain group takes `render_single_grain` rather than the multi-grain
/// renderer, but it emits its joins through the same `anchor_joins` — so it
/// receives the scoped `a__dep` JOIN and must rewrite the dimension to match.
/// Emitting the bare `a.city` there references an alias its own FROM never
/// binds, which DuckDB rejects at bind time.
#[test]
fn using_scopes_a_role_played_dimension_in_a_single_grain_cte() {
    let def = carriers_via_role_played_airports();
    let sql = expand("sv", &def, &req(&["dep_city"], &["fleet_size"]))
        .expect("a parent-grain metric with USING is per-grain eligible");
    assert!(
        sql.contains(r#"AS "a__dep" ON"#),
        "the scoped role is joined, got:\n{sql}"
    );
    assert!(
        sql.contains("a__dep.city"),
        "the dimension must bind to the scoped alias the JOIN emitted, got:\n{sql}"
    );
    assert!(
        !sql.contains("a.city AS"),
        "the bare alias is never bound by this FROM, so selecting it is invalid SQL:\n{sql}"
    );
}

// ---------------------------------------------------------------------------
// GRAIN-11: an active semi-additive metric snapshots at its OWN grain
// ---------------------------------------------------------------------------

/// root `o` (orders) --account_id--> `a` (accounts).
///
/// `a` is a PARENT of the base table, so `FROM orders LEFT JOIN accounts`
/// repeats each account row once per order and inflates a metric on `a`. The
/// metric is semi-additive on `report_date`, which the query does NOT ask for —
/// so it is ACTIVE and `__sv_snapshot` is the shape being emitted.
fn accounts_snapshot_fixture() -> SemanticViewDefinition {
    base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("a", "accounts", &["id"])
        .with_dimension("account_type", "a.account_type", Some("a"))
        .with_dimension("report_date", "a.report_date", Some("a"))
        .with_metric("total_balance", "SUM(a.balance)", Some("a"))
        .with_non_additive_by(
            "total_balance",
            &[("report_date", SortOrder::Asc, NullsOrder::Last)],
        )
        .with_pkfk_join("o_to_a", "o", "a", &["account_id"], &["id"])
}

/// GRAIN-11: the snapshot CTE anchors at the metric's own table instead of the
/// base table. Probed against Snowflake (TECH-DEBT #36): it computes the
/// snapshot inside the metric's own-grain aggregation. Was `RootGrainFanTrap`.
#[test]
fn active_semi_additive_snapshot_anchors_at_its_own_grain() {
    let def = accounts_snapshot_fixture();
    let sql = expand("sv", &def, &req(&["account_type"], &["total_balance"]))
        .expect("a parent-grain semi-additive metric must snapshot at its own grain");
    assert!(
        sql.contains(r#"FROM "accounts" AS "a""#),
        "the snapshot must anchor at the metric's own table, got:\n{sql}"
    );
    assert!(
        !sql.contains("orders"),
        "joining the base table is exactly what repeated each account row:\n{sql}"
    );
    assert!(
        sql.contains("RANK()"),
        "still the snapshot shape, just re-anchored:\n{sql}"
    );
}

/// GRAIN-11 — DIRECTION, reversed by TECH-DEBT #71. A metric at a CHILD grain
/// is now re-anchored too. This used to be refused because `FROM base LEFT
/// JOIN child` kept a childless parent as a NULL-extended group and `FROM
/// child` drops it; Snowflake settled (D1) that dropping it is right — a child
/// metric queried alone has no row for a parent with no children — and that
/// the child rows whose foreign key is NULL, which the base-anchored join lost,
/// are counted. Numbers: `tests_home_anchoring::semi_additive_child_metric_is_home_anchored`.
#[test]
fn child_grain_semi_additive_is_re_anchored_at_its_own_table() {
    let def = base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("li", "line_items", &["id"])
        .with_dimension("order_status", "o.status", Some("o"))
        .with_dimension("ship_date", "li.ship_date", Some("li"))
        .with_metric("item_total", "SUM(li.amount)", Some("li"))
        .with_non_additive_by(
            "item_total",
            &[("ship_date", SortOrder::Asc, NullsOrder::Last)],
        )
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"]);
    let sql = expand("sv", &def, &req(&["order_status"], &["item_total"]))
        .expect("a child-grain semi-additive metric is answerable");
    assert!(
        sql.contains("FROM \"line_items\" AS \"li\"\n    LEFT JOIN \"orders\" AS \"o\""),
        "the snapshot must anchor at the metric's own table and join OUT:\n{sql}"
    );
    assert!(
        sql.contains("RANK()"),
        "still the snapshot shape, just re-anchored:\n{sql}"
    );
}

/// root `o` (orders) --account_id--> `a` (accounts) --snapshot_id--> `s`.
///
/// The NA dimension lives on `s`, NOT on the metric's own table. Snowflake
/// accepts this when the reference is qualified (probed, TECH-DEBT #36), so the
/// re-anchored CTE has to join `s` for the `RANK`'s ORDER BY to bind.
fn offtable_na_dim_fixture() -> SemanticViewDefinition {
    base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("a", "accounts", &["id"])
        .with_table("s", "snapshots", &["id"])
        .with_dimension("account_type", "a.account_type", Some("a"))
        .with_dimension("report_date", "s.report_date", Some("s"))
        .with_metric("total_balance", "SUM(a.balance)", Some("a"))
        .with_non_additive_by(
            "total_balance",
            &[("report_date", SortOrder::Asc, NullsOrder::Last)],
        )
        .with_pkfk_join("o_to_a", "o", "a", &["account_id"], &["id"])
        .with_pkfk_join("a_to_s", "a", "s", &["snapshot_id"], &["id"])
}

/// GRAIN-11 guard — the NA dimension's table must be JOINED into the
/// re-anchored CTE.
///
/// An active semi-additive metric's NA dim is by definition not queried, so its
/// table never appears in `resolved_dims` and the ordinary dimension-join walk
/// would not reach it. Emitting `ORDER BY s.report_date` over a FROM that never
/// binds `s` is the unbound-alias class that shipped in #177.
#[test]
fn re_anchored_snapshot_joins_an_offtable_na_dimension() {
    let def = offtable_na_dim_fixture();
    let sql = expand("sv", &def, &req(&["account_type"], &["total_balance"]))
        .expect("an off-table NA dimension is legal (Snowflake accepts the qualified form)");
    assert!(
        sql.contains(r#"FROM "accounts" AS "a""#),
        "anchored at the metric's own grain, got:\n{sql}"
    );
    assert!(
        sql.contains(r#""snapshots" AS "s""#),
        "the NA dimension's table must be joined or its ORDER BY cannot bind:\n{sql}"
    );
}

/// GRAIN-12: TWO snapshot groups in one query.
///
/// `build_snapshot_block` numbers its capture columns by position within the
/// metrics it is given, so both groups independently produce `__sv_semi_0` and
/// `__sv_rn`. That is safe only because each lives inside its own grain CTE —
/// this pins that the per-group namespaces really are separate, which a single
/// shared snapshot CTE could not have provided.
#[test]
fn two_snapshot_groups_get_independent_column_namespaces() {
    let def = base_table(minimal_def("o", "d", "d", "m", "count(*)"), "orders", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("c", "customers", &["id"])
        .with_table("w", "warehouses", &["id"])
        .with_dimension("c_day", "c.as_of", Some("c"))
        .with_dimension("w_day", "w.as_of", Some("w"))
        .with_metric("total_balance", "SUM(c.balance)", Some("c"))
        .with_metric("stock_level", "SUM(w.stock)", Some("w"))
        .with_non_additive_by(
            "total_balance",
            &[("c_day", SortOrder::Asc, NullsOrder::Last)],
        )
        .with_non_additive_by(
            "stock_level",
            &[("w_day", SortOrder::Asc, NullsOrder::Last)],
        )
        .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"])
        .with_pkfk_join("o_to_w", "o", "w", &["warehouse_id"], &["id"]);
    let sql = expand("sales", &def, &req(&[], &["total_balance", "stock_level"]))
        .expect("two snapshot groups at two grains must be computable");
    assert_eq!(
        sql.matches("RANK()").count(),
        2,
        "one rank per snapshot group, got:\n{sql}"
    );
    assert!(
        sql.contains(r#"FROM "customers" AS "c""#) && sql.contains(r#"FROM "warehouses" AS "w""#),
        "each snapshot anchors at its own grain, got:\n{sql}"
    );
    // Each grain CTE orders by its OWN metric's NA dimension.
    assert!(
        sql.contains("c.as_of") && sql.contains("w.as_of"),
        "each snapshot ranks by its own NA dimension, got:\n{sql}"
    );
}

/// GRAIN-12 guard — a role-played dimension makes a snapshot query INELIGIBLE.
///
/// Copilot review catch on #180, and a real silent-wrong-answer hole.
/// `render_snapshot_group` builds its `ResolvedDim`s with `scoped_alias: None`
/// and passes an empty roles map to `anchor_joins`, so a snapshot group cannot
/// honour a role — while a sibling PLAIN group in the same query can. The two
/// CTEs then disagree about which instance the dimension means:
///
/// ```text
/// __sv_grain_0 (plain)    a__dep.city   ON "c"."dep_code"   <- named role
/// __sv_grain_1 (snapshot) a.city        ON "c"."arr_code"   <- declaration order
/// ```
///
/// and the outer FULL OUTER JOIN compares departure city against arrival city.
/// No error, plausible output, wrong numbers. Role-playing therefore stays
/// STRICT whenever an active semi-additive metric is present, which is what
/// `render_snapshot_group`'s doc comment always claimed.
///
/// The fixture declares `arr` FIRST so the bare-alias edge is demonstrably the
/// wrong one: with `dep` first the mis-binding would coincide with the right
/// answer and the test would pass for the wrong reason.
#[test]
fn role_played_dimension_with_a_semi_additive_metric_stays_ineligible() {
    let def = base_table(minimal_def("f", "d", "d", "m", "count(*)"), "flights", "id")
        .clear_dimensions()
        .clear_metrics()
        .with_table("c", "carriers", &["id"])
        .with_table("a", "airports", &["code"])
        .with_dimension("hub_city", "a.city", Some("a"))
        .with_dimension("as_of", "c.as_of", Some("c"))
        .with_metric("flight_count", "COUNT(*)", Some("f"))
        .with_metric("fleet_balance", "SUM(c.fleet)", Some("c"))
        .with_using_relationship("flight_count", &["dep"])
        // Both metrics name the same role (Phase 6.3 refuses a metric that names
        // none), so only the snapshot group's blindness to roles is in play.
        .with_using_relationship("fleet_balance", &["dep"])
        .with_non_additive_by(
            "fleet_balance",
            &[("as_of", SortOrder::Asc, NullsOrder::Last)],
        )
        .with_pkfk_join("f_to_c", "f", "c", &["carrier_id"], &["id"])
        .with_pkfk_join("arr", "c", "a", &["arr_code"], &["code"])
        .with_pkfk_join("dep", "c", "a", &["dep_code"], &["code"]);
    let err = expand(
        "sv",
        &def,
        &req(&["hub_city"], &["flight_count", "fleet_balance"]),
    )
    .expect_err("a snapshot group cannot honour a role, so the query must decline");
    assert!(
        matches!(
            err,
            ExpandError::RootGrainFanTrap { .. }
                | ExpandError::MetricFanTrap { .. }
                | ExpandError::FanTrap { .. }
        ),
        "expected the base-anchored fence's error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// EXP-11: a derived metric with a root-grain component that names no table
// ---------------------------------------------------------------------------

/// A metric with **no** `source_table` whose expression *aggregates* sits at the
/// root grain — the substitution EXP-8 added to the fence for the same shape.
/// It contributes no table to `collect_derived_metric_source_tables`, so a
/// derived metric that mixes one with a real-table component reports only the
/// other component's grain.
///
/// Current DDL cannot declare this metric (`alias.name` qualification is
/// mandatory, and `validate_derived_metrics` rejects an aggregate in an
/// unqualified entry), so the fixture is built directly — the same population
/// of legacy stored rows and constructed definitions EXP-8 was added to
/// protect.
fn orders_with_rootless_aggregate() -> SemanticViewDefinition {
    orders_with_parent_customers()
        .with_metric("rootless_total", "SUM(amount)", None)
        .with_metric("mixed", "total_balance - rootless_total", None)
}

/// EXP-11: `mixed`'s grain set is `{customers}` alone — the root component
/// vanishes — so the planner takes its single-grain fast path and anchors the
/// WHOLE resolved expression, `SUM(amount)` included, at `customers`. The fence
/// then runs in per-grain mode, which skips the EXP-1 root-grain check that
/// would have caught it. `SUM(amount)` binds against `customers`: silently wrong
/// if that table has an `amount` column, a confusing binder error if not.
///
/// v0.11.0 raised `RootGrainFanTrap` for this query, and that is the right
/// answer until the root component can be given a CTE of its own. This asserts
/// an ERROR where a fuller per-grain implementation would compute a number —
/// TECH-DEBT #50 records what "fuller" means and why no DDL-reachable query
/// needs it.
#[test]
fn derived_metric_with_rootless_aggregate_component_errors() {
    let def = orders_with_rootless_aggregate();
    let err = expand("sales", &def, &req(&[], &["mixed"]))
        .expect_err("a root-grain component under a parent-grain anchor must not be emitted");
    assert!(
        matches!(err, ExpandError::RootGrainFanTrap { .. }),
        "expected the root-grain fence error, got: {err}"
    );
}

/// EXP-11, multi-grain variant: with two real grains the planner reaches
/// `decompose`, which treats every source-less dependency as "derived, inlined
/// by the reference walk". For an *aggregate* one that splices the raw
/// `SUM(amount)` into the OUTER select over the grain CTEs, where no such column
/// exists. Decline instead.
#[test]
fn multi_grain_derived_metric_with_rootless_aggregate_component_errors() {
    let def = orders_with_parent_customers()
        .with_table("li", "line_items", &["id"])
        .with_metric("item_count", "COUNT(li.id)", Some("li"))
        .with_metric("rootless_total", "SUM(amount)", None)
        .with_metric("mixed", "total_balance + item_count + rootless_total", None)
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"]);
    let err = expand("sales", &def, &req(&[], &["mixed"]))
        .expect_err("a raw aggregate cannot be spliced into the outer select");
    assert!(
        matches!(err, ExpandError::RootGrainFanTrap { .. }),
        "expected the root-grain fence error, got: {err}"
    );
}

/// EXP-11: the root-grain component must survive into the grain set even when
/// the query is otherwise answerable per-grain — a metric that mixes a
/// rootless aggregate with a component on the root's own table is at the root
/// grain and nowhere else.
#[test]
fn rootless_aggregate_mixed_with_root_table_component_stays_at_root() {
    let def = orders_with_parent_customers()
        .with_metric("rootless_total", "SUM(amount)", None)
        .with_metric("mixed", "order_count + rootless_total", None);
    let sql = expand("sales", &def, &req(&[], &["mixed"]))
        .expect("both components sit at the root grain, so the query is answerable");
    assert!(
        sql.contains(r#"FROM "orders" AS "o""#),
        "must be anchored at the root table, got:\n{sql}"
    );
    assert!(
        sql.contains("(COUNT(*)) + (SUM(amount))"),
        "both components must be computed over the root table's rows, got:\n{sql}"
    );
    assert!(
        !sql.contains("__sv_grain"),
        "one grain needs no per-grain CTEs, got:\n{sql}"
    );
}

// EXP-22 (code-review 2026-08-06): an UNQUALIFIED `where_clause` member and
// re-anchoring.
//
// All three deciders decline a queried DIMENSION with no source table, on the
// stated grounds that an unqualified expression "is resolved against whatever
// the FROM happens to expose", so its binding would move with the anchor. A
// `where_clause` member is spliced into exactly the same CTEs and has exactly
// the same problem — but contributed no source table to `where_tables` and got
// no such check, so its predicate text was carried into a CTE whose FROM is
// the anchor table and bound against THAT table's columns.
//
// Verified against DuckDB: with `status` declared unqualified and both tables
// carrying a `status` column, `where_clause := 'status = ''active'''` filtered
// `customers.status` inside the re-anchored CTE and returned the balance of
// the *active customer* where base-table semantics required the empty set. In
// the multi-grain shape the same predicate text lands in two CTEs anchored at
// different tables — two bindings for one predicate, in one statement.
//
// The fix mirrors the dimension rule exactly: decline, and let the query take
// the base-anchored path where an unqualified expression is well defined (or
// keep that path's fan-trap error, which is what an unanswerable shape is
// supposed to produce).

/// [`orders_with_parent_customers`] plus an unqualified `status` dimension —
/// the shape whose binding moves with the anchor.
fn parent_customers_with_unqualified_status() -> SemanticViewDefinition {
    orders_with_parent_customers().with_dimension("status", "status", None)
}

#[test]
fn an_unqualified_where_clause_member_declines_the_single_grain_re_anchor() {
    let def = parent_customers_with_unqualified_status();
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("segment")],
        metrics: vec![MetricName::new("total_balance")],
        facts: vec![],
        where_clause: Some("status = 'ACTIVE'".to_string()),
    };
    match expand("test_view", &def, &req) {
        // Base-anchored: the predicate binds where the user wrote it against.
        Ok(sql) => assert!(
            !sql.contains(r#"FROM "customers" AS "c""#),
            "an unqualified predicate must not be re-anchored onto customers: {sql}"
        ),
        // Or the base-anchored path's own fan-trap error for this shape.
        // Either is correct; binding `status` to `customers` silently is not.
        Err(e) => assert!(
            matches!(
                e,
                ExpandError::MetricFanTrap { .. } | ExpandError::RootGrainFanTrap { .. }
            ),
            "expected base-anchored SQL or a fan-trap error, got: {e:?}"
        ),
    }
}

#[test]
fn an_unqualified_where_clause_member_declines_the_window_re_anchor() {
    let def = parent_customers_with_unqualified_status()
        .with_metric("running_balance", "SUM(total_balance)", Some("o"))
        .with_window_spec(
            "running_balance",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "total_balance".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["segment".to_string()],
                order_by: vec![],
                frame_clause: None,
            },
        );
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("segment")],
        metrics: vec![MetricName::new("running_balance")],
        facts: vec![],
        where_clause: Some("status = 'ACTIVE'".to_string()),
    };
    match expand("sales", &def, &req) {
        Ok(sql) => assert!(
            !sql.contains(r#"FROM "customers" AS "c""#),
            "__sv_agg must not anchor at customers with an unqualified predicate: {sql}"
        ),
        Err(e) => assert!(
            matches!(
                e,
                ExpandError::MetricFanTrap { .. } | ExpandError::RootGrainFanTrap { .. }
            ),
            "expected base-anchored SQL or a fan-trap error, got: {e:?}"
        ),
    }
}

#[test]
fn an_unqualified_where_clause_member_declines_the_snapshot_re_anchor() {
    let def = accounts_snapshot_fixture().with_dimension("status", "status", None);
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("account_type")],
        metrics: vec![MetricName::new("total_balance")],
        facts: vec![],
        where_clause: Some("status = 'ACTIVE'".to_string()),
    };
    match expand("sv", &def, &req) {
        Ok(sql) => assert!(
            !sql.contains(r#"FROM "accounts" AS "a""#),
            "the snapshot must not anchor at accounts with an unqualified predicate: {sql}"
        ),
        Err(e) => assert!(
            matches!(
                e,
                ExpandError::MetricFanTrap { .. } | ExpandError::RootGrainFanTrap { .. }
            ),
            "expected base-anchored SQL or a fan-trap error, got: {e:?}"
        ),
    }
}

// Controls: a QUALIFIED `where_clause` member must still re-anchor exactly as
// before — the guard keys on the missing source table, not on the presence of
// a predicate. Without these, declining unconditionally would pass.

#[test]
fn control_a_qualified_where_clause_member_still_re_anchors_the_window_cte() {
    let def = orders_with_parent_customers()
        .with_metric("running_balance", "SUM(total_balance)", Some("o"))
        .with_window_spec(
            "running_balance",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "total_balance".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["segment".to_string()],
                order_by: vec![],
                frame_clause: None,
            },
        );
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("segment")],
        metrics: vec![MetricName::new("running_balance")],
        facts: vec![],
        where_clause: Some("segment <> 'CHURNED'".to_string()),
    };
    let sql = expand("sales", &def, &req).expect("a qualified predicate is unaffected");
    assert!(
        sql.contains(r#"FROM "customers" AS "c""#),
        "a qualified member must still anchor at its own grain: {sql}"
    );
}

#[test]
fn control_a_qualified_where_clause_member_still_re_anchors_the_snapshot() {
    let def = accounts_snapshot_fixture();
    let req = QueryRequest {
        dimensions: vec![DimensionName::new("account_type")],
        metrics: vec![MetricName::new("total_balance")],
        facts: vec![],
        where_clause: Some("account_type <> 'CLOSED'".to_string()),
    };
    let sql = expand("sv", &def, &req).expect("a qualified predicate is unaffected");
    assert!(
        sql.contains(r#"FROM "accounts" AS "a""#),
        "a qualified member must still anchor at its own grain: {sql}"
    );
}
