//! Home-table anchoring (TECH-DEBT #71, decision D1).
//!
//! Every metric is aggregated from its OWN home table, joining only outward
//! along many-to-one edges to reach the requested dimensions; the per-metric
//! blocks are merged on the dimension values. Anentropic ran the D1 fixture on
//! Snowflake (2026-10-13) and its grids settle the two edge cases a
//! base-anchored `FROM base LEFT JOIN child` got the other way:
//!
//! - a child row whose foreign key is NULL (an orphan) IS counted, under a NULL
//!   dimension value;
//! - a parent with no children gives a child metric queried alone NO row, and a
//!   NULL (not 0) when a metric of the parent's grain brings the row in.
//!
//! These tests pin NUMBERS: each runs the generated SQL against in-memory
//! DuckDB. Every test is its own `#[test]` so each reports red or green on its
//! own (CLAUDE.md, "confirm the red for each case"). The `d1_q*` tests are the
//! fixture's six SEMANTIC_VIEW queries with Snowflake's grids; the rest cover
//! one emission path each.

use super::*;
use crate::expand::test_helpers::TestFixtureExt;
use crate::model::{
    Cardinality, Join, NullsOrder, SemanticViewDefinition, SortOrder, TableRef, WindowSpec,
};

fn table(alias: &str, pk: &[&str]) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: format!("d1_{alias}"),
        pk_columns: pk.iter().map(ToString::to_string).collect(),
        unique_constraints: vec![],
        comment: None,
        synonyms: vec![],
        tags: Vec::new(),
    }
}

fn many_to_one(name: &str, from: &str, to: &str, fk: &str, pk: &str) -> Join {
    Join {
        table: to.to_string(),
        from_alias: from.to_string(),
        fk_columns: vec![fk.to_string()],
        ref_columns: vec![pk.to_string()],
        name: Some(name.to_string()),
        cardinality: Cardinality::ManyToOne,
    }
}

/// The D1 model: customers `c` <- orders `o` <- line items `li`, with `c` the
/// base table (declared first), exactly as in the Snowflake fixture.
fn d1_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("c", &["c_id"]),
            table("o", &["o_id"]),
            table("li", &["li_id"]),
        ],
        joins: vec![
            many_to_one("o_to_c", "o", "c", "c_id", "c_id"),
            many_to_one("li_to_o", "li", "o", "o_id", "o_id"),
        ],
        ..Default::default()
    }
    .with_dimension("cust_region", "c.region", Some("c"))
    .with_dimension("order_id", "o.o_id", Some("o"))
    .with_dimension("item_qty", "li.qty", Some("li"))
    .with_metric("n_customers", "COUNT(c.c_id)", Some("c"))
    .with_metric("n_orders", "COUNT(o.o_id)", Some("o"))
    .with_metric("total_amount", "SUM(o.amount)", Some("o"))
    .with_metric("n_items", "COUNT(li.li_id)", Some("li"))
    .with_metric("n_items_all", "COUNT(*)", Some("li"))
    .with_metric("total_qty", "SUM(li.qty)", Some("li"))
}

/// The D1 data: a customer with no orders (APAC), an order with no items
/// (12), an order with a NULL customer (13) and an item with a NULL order (103).
fn d1_db() -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE d1_c (c_id INT, region VARCHAR); \
         CREATE TABLE d1_o (o_id INT, c_id INT, amount DECIMAL(10,2)); \
         CREATE TABLE d1_li (li_id INT, o_id INT, qty INT); \
         INSERT INTO d1_c VALUES (1, 'EU'), (2, 'US'), (3, 'APAC'); \
         INSERT INTO d1_o VALUES (10, 1, 100), (11, 1, 50), (12, 2, 70), (13, NULL, 5); \
         INSERT INTO d1_li VALUES (100, 10, 1), (101, 10, 2), (102, 11, 3), (103, NULL, 40);",
    )
    .expect("load the D1 fixture");
    conn
}

fn req(dims: &[&str], mets: &[&str], where_clause: Option<&str>) -> QueryRequest {
    QueryRequest {
        where_clause: where_clause.map(ToString::to_string),
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    }
}

/// Run `request` and render each result row as `a|b|…` (NULL spelled out),
/// columns in request order (dimensions, then metrics), rows sorted.
fn rows(
    def: &SemanticViewDefinition,
    conn: &duckdb::Connection,
    request: &QueryRequest,
) -> Vec<String> {
    let sql = expand("d1", def, request).unwrap_or_else(|e| panic!("expand failed: {e}"));
    let cols: Vec<String> = request
        .dimensions
        .iter()
        .map(|d| d.as_str().to_string())
        .chain(request.metrics.iter().map(|m| m.as_str().to_string()))
        .map(|c| format!("COALESCE(CAST(q.{} AS VARCHAR), 'NULL')", quote_ident(&c)))
        .collect();
    let probe = format!(
        "SELECT concat_ws('|', {}) AS r FROM ({sql}) q ORDER BY r",
        cols.join(", ")
    );
    let mut stmt = conn
        .prepare(&probe)
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"));
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap_or_else(|e| panic!("query failed: {e}\nSQL:\n{sql}"))
        .map(Result::unwrap)
        .collect()
}

fn d1_rows(dims: &[&str], mets: &[&str]) -> Vec<String> {
    rows(&d1_def(), &d1_db(), &req(dims, mets, None))
}

// ---------------------------------------------------------------------------
// The D1 fixture, Q1-Q6, against Snowflake's grids.
// ---------------------------------------------------------------------------

/// Q1: child metrics alone by a grandparent dimension. The orphan item is the
/// NULL-region row; US and APAC have no items, so no row (was: US 0, APAC 0,
/// no NULL row).
#[test]
fn d1_q1_child_metrics_alone_by_grandparent_dimension() {
    assert_eq!(
        d1_rows(&["cust_region"], &["n_items", "n_items_all", "total_qty"]),
        vec!["EU|3|3|6", "NULL|1|1|40"]
    );
}

/// Q2: the same child metrics alongside a customer-grain metric — identical
/// child values; US / APAC appear with NULL (not 0) child metrics.
#[test]
fn d1_q2_child_metrics_alongside_a_parent_grain_metric() {
    assert_eq!(
        d1_rows(
            &["cust_region"],
            &["n_customers", "n_items", "n_items_all", "total_qty"]
        ),
        vec![
            "APAC|1|NULL|NULL|NULL",
            "EU|1|3|3|6",
            "NULL|NULL|1|1|40",
            "US|1|NULL|NULL|NULL",
        ]
    );
}

/// Q3: grand totals count the orphan item: 46, not 6.
#[test]
fn d1_q3_grand_total_counts_the_orphan_child() {
    assert_eq!(d1_rows(&[], &["n_items", "total_qty"]), vec!["4|46"]);
}

/// Q4: 225 includes orphan order 13.
#[test]
fn d1_q4_grand_totals_across_two_grains() {
    assert_eq!(
        d1_rows(&[], &["total_amount", "n_items", "total_qty"]),
        vec!["225.00|4|46"]
    );
}

/// Q5: by the parent dimension. Orders 12 and 13 have no items — no row.
#[test]
fn d1_q5_child_metrics_alone_by_parent_dimension() {
    assert_eq!(
        d1_rows(&["order_id"], &["n_items", "total_qty"]),
        vec!["10|2|3", "11|1|3", "NULL|1|40"]
    );
}

/// Q6: with an order-grain metric alongside — the same child values as Q5.
#[test]
fn d1_q6_child_metrics_alongside_an_order_grain_metric() {
    assert_eq!(
        d1_rows(&["order_id"], &["n_orders", "n_items", "total_qty"]),
        vec![
            "10|1|2|3",
            "11|1|1|3",
            "12|1|NULL|NULL",
            "13|1|NULL|NULL",
            "NULL|NULL|1|40",
        ]
    );
}

// ---------------------------------------------------------------------------
// One emission path each.
// ---------------------------------------------------------------------------

/// A derived metric over two child metrics (one grain): computed from the
/// child table, so the orphan has its own row (was: no NULL row, and NULL for
/// the childless US / APAC through `NULLIF`).
#[test]
fn derived_metric_over_child_metrics_is_home_anchored() {
    let def = d1_def().with_metric("avg_qty", "total_qty / NULLIF(n_items, 0)", None);
    assert_eq!(
        rows(&def, &d1_db(), &req(&["cust_region"], &["avg_qty"], None)),
        vec!["EU|2.0", "NULL|40.0"]
    );
}

/// MM-4: grouping a child metric by a CHILD dimension. Base-anchored, the
/// childless parents made a `NULL|0` group and the orphan item vanished.
#[test]
fn child_dimension_grouping_has_no_manufactured_group() {
    assert_eq!(
        d1_rows(&["item_qty"], &["n_items"]),
        vec!["1|1", "2|1", "3|1", "40|1"]
    );
}

/// A `where_clause` member on a PARENT table filters the joined parent value:
/// the orphan item's region is NULL, so `IS NULL` selects exactly it (was: 0 —
/// the base-anchored FROM never saw the orphan).
#[test]
fn where_clause_on_a_parent_member_sees_the_orphan() {
    assert_eq!(
        rows(
            &d1_def(),
            &d1_db(),
            &req(&[], &["n_items"], Some("cust_region IS NULL"))
        ),
        vec!["1"]
    );
}

/// A `where_clause` member on the child itself.
#[test]
fn where_clause_on_a_child_member_sees_the_orphan() {
    assert_eq!(
        rows(
            &d1_def(),
            &d1_db(),
            &req(&[], &["n_items", "total_qty"], Some("item_qty > 10"))
        ),
        vec!["1|40"]
    );
}

/// Semi-additive: the `__sv_snapshot` CTE anchors at the child (the snapshot
/// anchor used to re-anchor only toward a PARENT). `last_qty` is the qty of the
/// latest (highest, `ASC` = latest) item id per region: EU's is item 102 (3), and the orphan item 103
/// (40) is the NULL-region row. Base-anchored, US and APAC got a NULL row and
/// the orphan none.
#[test]
fn semi_additive_child_metric_is_home_anchored() {
    let def = d1_def()
        .with_dimension("item_id", "li.li_id", Some("li"))
        .with_metric("last_qty", "SUM(li.qty)", Some("li"))
        .with_non_additive_by("last_qty", &[("item_id", SortOrder::Asc, NullsOrder::Last)]);
    assert_eq!(
        rows(&def, &d1_db(), &req(&["cust_region"], &["last_qty"], None)),
        vec!["EU|3", "NULL|40"]
    );
}

/// Window: `__sv_agg` anchors at the inner metric's child table, so the window
/// runs over the home-anchored groups. `PARTITION BY EXCLUDING cust_region`
/// makes it a total across them: 46 (was: EU 6, US / APAC NULL — a total of 6
/// on every row, and no orphan row).
#[test]
fn window_over_a_child_metric_is_home_anchored() {
    let def = d1_def()
        .with_metric(
            "qty_overall",
            "SUM(total_qty) OVER (PARTITION BY EXCLUDING cust_region)",
            Some("li"),
        )
        .with_window_spec(
            "qty_overall",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "total_qty".to_string(),
                excluding_dims: vec!["cust_region".to_string()],
                ..Default::default()
            },
        );
    assert_eq!(
        rows(
            &def,
            &d1_db(),
            &req(&["cust_region"], &["qty_overall"], None)
        ),
        vec!["EU|46", "NULL|46"]
    );
}

/// Reviewbot, #250: a re-anchored `__sv_agg` must join the table of a fact the
/// inner metric inlines. `rev` lives on `li` but reads the parent fact
/// `o.o_amount`; grouped by a `li` dimension, no queried dimension reaches `o`.
/// Base-anchored, `resolve_joins_pkfk` joined `o` for the fact; the anchored
/// FROM joined only the dimensions' and `where_clause` tables, so DuckDB raised
/// a Binder Error on `o.amount`. 1*100 + 2*100 + 3*50 = 450; the orphan item's
/// product is NULL.
#[test]
fn window_reanchored_at_a_child_joins_the_inner_metrics_fact_table() {
    let def = d1_def()
        .with_fact("o_amount", "o.amount", "o")
        .with_metric("rev", "SUM(li.qty * o.o_amount)", Some("li"))
        .with_metric(
            "rev_overall",
            "SUM(rev) OVER (PARTITION BY EXCLUDING item_qty)",
            Some("li"),
        )
        .with_window_spec(
            "rev_overall",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "rev".to_string(),
                excluding_dims: vec!["item_qty".to_string()],
                ..Default::default()
            },
        );
    assert_eq!(
        rows(&def, &d1_db(), &req(&["item_qty"], &["rev_overall"], None)),
        vec!["1|450.00", "2|450.00", "3|450.00", "40|450.00"]
    );
}

/// The window sibling for a fact a queried DIMENSION reaches: `li_amount` is a
/// `li` dimension whose expression reads the parent fact `o.o_amount`, so the
/// re-anchored `__sv_agg` has to join `o` for the group key too.
#[test]
fn window_reanchored_at_a_child_joins_a_dimensions_fact_table() {
    let def = d1_def()
        .with_fact("o_amount", "o.amount", "o")
        .with_dimension("li_amount", "o.o_amount", Some("li"))
        .with_metric(
            "qty_overall",
            "SUM(total_qty) OVER (PARTITION BY EXCLUDING li_amount)",
            Some("li"),
        )
        .with_window_spec(
            "qty_overall",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "total_qty".to_string(),
                excluding_dims: vec!["li_amount".to_string()],
                ..Default::default()
            },
        );
    assert_eq!(
        rows(&def, &d1_db(), &req(&["li_amount"], &["qty_overall"], None)),
        vec!["100.00|46", "50.00|46", "NULL|46"]
    );
}

/// Reviewbot, #250: the snapshot sibling. `last_rev` is semi-additive on `li`
/// and reads the parent fact `o.o_amount`; the re-anchored snapshot SELECT must
/// join `o`. Each `item_qty` group holds one item, so the snapshot is that
/// item's product; the orphan's is NULL.
#[test]
fn snapshot_reanchored_at_a_child_joins_the_metrics_fact_table() {
    let def = d1_def()
        .with_fact("o_amount", "o.amount", "o")
        .with_dimension("item_id", "li.li_id", Some("li"))
        .with_metric("last_rev", "SUM(li.qty * o.o_amount)", Some("li"))
        .with_non_additive_by("last_rev", &[("item_id", SortOrder::Asc, NullsOrder::Last)]);
    assert_eq!(
        rows(&def, &d1_db(), &req(&["item_qty"], &["last_rev"], None)),
        vec!["1|100.00", "2|200.00", "3|150.00", "40|NULL"]
    );
}

/// `COUNT(*)` on a child table with no PRIMARY KEY. Base-anchored, the phantom
/// row made this an error (`CountStarRequiresPrimaryKey`); home-anchored there
/// is no phantom, and every row is counted.
#[test]
fn count_star_on_a_pk_less_child_needs_no_primary_key() {
    let mut def = d1_def();
    def.tables[2].pk_columns.clear();
    assert_eq!(
        rows(
            &def,
            &d1_db(),
            &req(&["cust_region"], &["n_items_all"], None)
        ),
        vec!["EU|3", "NULL|1"]
    );
}

// ---------------------------------------------------------------------------
// Role-playing: `USING` on the per-grain path.
// ---------------------------------------------------------------------------

/// Base `o` reaching the role-played `ap` twice (`dep` / `arr`), with a child
/// `li`. `alt` is a fact on `ap`; `dep_alt` / `arr_alt` select its role.
fn role_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("o", &["id"]),
            table("li", &["id"]),
            table("ap", &["code"]),
        ],
        joins: vec![
            many_to_one("li_o", "li", "o", "order_id", "id"),
            many_to_one("dep", "o", "ap", "dep_code", "code"),
            many_to_one("arr", "o", "ap", "arr_code", "code"),
        ],
        ..Default::default()
    }
    .with_dimension("region", "o.region", Some("o"))
    .with_dimension("city", "ap.city", Some("ap"))
    .with_fact("alt", "ap.alt", "ap")
    .with_metric("n_li", "COUNT(*)", Some("li"))
    .with_metric("dep_alt", "SUM(alt)", Some("o"))
    .with_using_relationship("dep_alt", &["dep"])
    .with_metric("arr_alt", "SUM(alt)", Some("o"))
    .with_using_relationship("arr_alt", &["arr"])
    .with_metric("dep_n_li", "COUNT(*)", Some("li"))
    .with_using_relationship("dep_n_li", &["dep"])
}

/// Two orders (dep 1 -> arr 2, dep 2 -> arr 2), one item each plus an orphan.
fn role_db() -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE d1_o (id INT, region INT, dep_code INT, arr_code INT); \
         CREATE TABLE d1_li (id INT, order_id INT); \
         CREATE TABLE d1_ap (code INT, city VARCHAR, alt INT); \
         INSERT INTO d1_ap VALUES (1, 'A', 100), (2, 'B', 200); \
         INSERT INTO d1_o VALUES (0, 7, 1, 2), (1, 7, 2, 2); \
         INSERT INTO d1_li VALUES (0, 0), (1, 1), (2, NULL);",
    )
    .expect("load the role-playing fixture");
    conn
}

/// A child metric co-queried with two base metrics that reach the role-played
/// fact through DIFFERENT roles. The base grain's CTE must join `ap` twice,
/// once per role, under each scoped alias; the child grain's CTE joins
/// nothing. Base-anchored (the old route for this shape), `n_li` lost the
/// orphan item: 2 instead of 3.
#[test]
fn per_grain_joins_each_role_a_fact_metric_names() {
    assert_eq!(
        rows(
            &role_def(),
            &role_db(),
            &req(&[], &["n_li", "dep_alt", "arr_alt"], None)
        ),
        vec!["3|300|400"]
    );
}

/// A child metric whose `USING` names a role for a table the query never
/// reaches: the relationship scopes nothing, so it does not keep the metric off
/// its home table (was: base-anchored, the orphan item dropped — 2).
#[test]
fn using_that_scopes_nothing_does_not_block_home_anchoring() {
    assert_eq!(
        rows(
            &role_def(),
            &role_db(),
            &req(&["region"], &["dep_n_li"], None)
        ),
        vec!["7|2", "NULL|1"]
    );
}

/// The same child metric grouped by the role-played dimension its `USING`
/// names: the grain CTE joins `ap` through `o` under the scoped alias `dep`
/// selects. Item 0's order departs from A, item 1's from B, the orphan from
/// nowhere.
#[test]
fn child_metric_grouped_by_its_role_played_dimension() {
    assert_eq!(
        rows(
            &role_def(),
            &role_db(),
            &req(&["city"], &["dep_n_li"], None)
        ),
        vec!["A|1", "B|1", "NULL|1"]
    );
}

/// Reviewbot, #250: a `USING` on a TRANSITIVE dependency of a window's inner
/// metric. `w_dep` windows the derived `li_dep_alt2`, whose component
/// `li_dep_alt` reads the role-played fact `alt` under `USING (dep)`. Only the
/// window metric and its direct inner metric were checked for `USING`, and
/// neither carries one, so `__sv_agg` was re-anchored at `li` with no scoped
/// join while the inlined fact already named `ap__dep`: a Binder Error. The
/// window emitter cannot thread a role, so the query must keep the
/// base-anchored route: items 0 and 1 depart from A (100) and B (200), so
/// 2 * 300 = 600 for region 7.
#[test]
fn window_over_a_derived_metric_with_a_using_dependency_is_not_reanchored() {
    let def = role_def()
        .with_metric("li_dep_alt", "SUM(alt)", Some("li"))
        .with_using_relationship("li_dep_alt", &["dep"])
        .with_metric("li_dep_alt2", "li_dep_alt * 2", None)
        .with_metric("w_dep", "SUM(li_dep_alt2) OVER ()", Some("li"))
        .with_window_spec(
            "w_dep",
            WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "li_dep_alt2".to_string(),
                ..Default::default()
            },
        );
    assert_eq!(
        rows(&def, &role_db(), &req(&["region"], &["w_dep"], None)),
        vec!["7|600"]
    );
}

/// Reviewbot, #250: a semi-additive child metric whose `USING` selects the
/// role for a fact it inlines, co-queried with a base-grain metric, takes a
/// SNAPSHOT group in the per-grain plan. That group's CTE must join the scoped
/// instance (`ap__dep`) the inlined fact was requalified to, as the plain
/// groups do; it joined nothing, and the query failed to bind `ap__dep`.
/// `NON ADDITIVE BY (li_id DESC)` keeps the row that sorts last, item 0, whose
/// order departs from A (100); the other role, `arr`, would give B (200), so the
/// value also pins which instance was joined.
#[test]
fn per_grain_snapshot_group_joins_its_scoped_fact() {
    let def = role_def()
        .with_dimension("li_id", "li.id", Some("li"))
        .with_metric("n_o", "COUNT(*)", Some("o"))
        .with_metric("dep_alt_snap", "MAX(alt)", Some("li"))
        .with_using_relationship("dep_alt_snap", &["dep"])
        .with_non_additive_by(
            "dep_alt_snap",
            &[("li_id", SortOrder::Desc, NullsOrder::Last)],
        );
    assert_eq!(
        rows(&def, &role_db(), &req(&[], &["n_o", "dep_alt_snap"], None)),
        vec!["2|100"]
    );
}

/// Reviewbot, #250: two metrics in one grain group inline the same fact on
/// `o`, one under `USING (li_o)` (requalified to `o__li_o`) and one without
/// (bare `o`). The scoped join must not stand in for the bare one: dropping
/// `o` from the group's joins left `m_bare` unbound. Items 0 and 1 both
/// belong to region-7 orders; the orphan contributes NULL.
#[test]
fn grain_group_keeps_the_bare_join_beside_a_scoped_one() {
    let def = role_def()
        .with_fact("o_reg", "o.region", "o")
        .with_metric("m_scoped", "SUM(o.o_reg)", Some("li"))
        .with_using_relationship("m_scoped", &["li_o"])
        .with_metric("m_bare", "SUM(o.o_reg)", Some("li"));
    assert_eq!(
        rows(&def, &role_db(), &req(&[], &["m_scoped", "m_bare"], None)),
        vec!["14|14"]
    );
}

// ---------------------------------------------------------------------------
// Relationship names on the per-grain path resolve like everywhere else
// (`graph::find_relationship` / `RelNameMatch`, #249).
// ---------------------------------------------------------------------------

/// `per_grain_joins_each_role_a_fact_metric_names` with the `USING` names
/// spelled differently from their declarations: `"Dep"` (quoted, other case)
/// and `ARR` both name the declared relationship under DuckDB's identifier
/// rules. The scoped fact join each grain CTE needs must be found from that
/// spelling, as the base-anchored resolver finds it; otherwise the CTE names
/// the scoped alias `rewrite_using_scoped_refs` wrote without joining it.
#[test]
fn per_grain_scoped_fact_join_resolves_a_using_spelled_differently() {
    let mut def = role_def();
    for m in &mut def.metrics {
        match m.name.as_str() {
            "dep_alt" => m.using_relationships = vec!["\"Dep\"".to_string()],
            "arr_alt" => m.using_relationships = vec!["ARR".to_string()],
            _ => {}
        }
    }
    assert_eq!(
        rows(
            &def,
            &role_db(),
            &req(&[], &["n_li", "dep_alt", "arr_alt"], None)
        ),
        vec!["3|300|400"]
    );
}

/// A semi-additive child metric whose `USING` names the one-part quoted
/// relationship `"x.y"` (to `ap`, which the query never reaches), in a view
/// whose `o -> c` relationship is the two-part `x.y` — a different name
/// (#249). The query reaches `c`, not `ap`, so the `USING` scopes nothing and
/// the snapshot CTE re-anchors at the child, as in
/// `semi_additive_child_metric_is_home_anchored`. Matching relationship names
/// with the lossy `ident_matches` took `x.y` for `"x.y"` and kept the metric
/// base-anchored (EU 3, US / APAC NULL, the orphan item dropped).
#[test]
fn snapshot_reanchor_resolves_using_by_relationship_identity() {
    let mut def = d1_def();
    def.joins[0].name = Some("x.y".to_string());
    def.tables.push(table("ap", &["code"]));
    def.joins
        .push(many_to_one("\"x.y\"", "o", "ap", "ap_code", "code"));
    let def = def
        .with_dimension("item_id", "li.li_id", Some("li"))
        .with_metric("last_qty", "SUM(li.qty)", Some("li"))
        .with_non_additive_by("last_qty", &[("item_id", SortOrder::Asc, NullsOrder::Last)])
        .with_using_relationship("last_qty", &["\"x.y\""]);
    let conn = d1_db();
    conn.execute_batch("ALTER TABLE d1_o ADD COLUMN ap_code INT; CREATE TABLE d1_ap (code INT);")
        .expect("extend the D1 fixture");
    assert_eq!(
        rows(&def, &conn, &req(&["cust_region"], &["last_qty"], None)),
        vec!["EU|3", "NULL|40"]
    );
}
