//! Derived-metric (metric-of-metrics) inlining.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase30_derived_metric_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::facts::{inline_derived_metrics, toposort_facts};
use crate::expand::test_helpers::{minimal_def, TestFixtureExt};
use crate::model::{
    AccessModifier, Cardinality, Dimension, Fact, Join, Metric, SemanticViewDefinition, TableRef,
};

#[test]
fn inline_derived_one_base_one_derived() {
    let metrics = vec![
        Metric {
            name: "revenue".to_string(),
            expr: "SUM(amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        },
        Metric {
            name: "cost".to_string(),
            expr: "SUM(unit_cost)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        },
        Metric {
            name: "profit".to_string(),
            expr: "revenue - cost".to_string(),
            ..Default::default()
        },
    ];
    let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
        .unwrap()
        .exprs;
    assert_eq!(
        resolved.get("profit").unwrap(),
        "(SUM(amount)) - (SUM(unit_cost))"
    );
}

#[test]
fn inline_derived_stacked() {
    let metrics = vec![
        Metric {
            name: "revenue".to_string(),
            expr: "SUM(amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        },
        Metric {
            name: "cost".to_string(),
            expr: "SUM(unit_cost)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        },
        Metric {
            name: "profit".to_string(),
            expr: "revenue - cost".to_string(),
            ..Default::default()
        },
        Metric {
            name: "margin".to_string(),
            expr: "profit / revenue * 100".to_string(),
            ..Default::default()
        },
    ];
    let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
        .unwrap()
        .exprs;
    assert_eq!(
        resolved.get("profit").unwrap(),
        "(SUM(amount)) - (SUM(unit_cost))"
    );
    assert_eq!(
        resolved.get("margin").unwrap(),
        "((SUM(amount)) - (SUM(unit_cost))) / (SUM(amount)) * 100"
    );
}

#[test]
fn inline_derived_with_facts() {
    let metrics = vec![
        Metric {
            name: "revenue".to_string(),
            expr: "SUM(net_price)".to_string(),
            source_table: Some("li".to_string()),
            ..Default::default()
        },
        Metric {
            name: "double_rev".to_string(),
            expr: "revenue * 2".to_string(),
            ..Default::default()
        },
    ];
    let facts = vec![Fact {
        name: "net_price".to_string(),
        expr: "extended_price * (1 - discount)".to_string(),
        source_table: Some("li".to_string()),
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: false,
        access: AccessModifier::Public,
    }];
    let topo_order = toposort_facts(&facts).unwrap();
    let resolved = inline_derived_metrics(&metrics, &facts, &topo_order, &[])
        .unwrap()
        .exprs;
    assert_eq!(
        resolved.get("revenue").unwrap(),
        "SUM((extended_price * (1 - discount)))"
    );
    assert_eq!(
        resolved.get("double_rev").unwrap(),
        "(SUM((extended_price * (1 - discount)))) * 2"
    );
}

#[test]
fn inline_derived_parenthesization_prevents_precedence_error() {
    let metrics = vec![
        Metric {
            name: "a".to_string(),
            expr: "SUM(x)".to_string(),
            source_table: Some("t".to_string()),
            ..Default::default()
        },
        Metric {
            name: "b".to_string(),
            expr: "SUM(y)".to_string(),
            source_table: Some("t".to_string()),
            ..Default::default()
        },
        Metric {
            name: "profit".to_string(),
            expr: "a - b".to_string(),
            ..Default::default()
        },
        Metric {
            name: "margin".to_string(),
            expr: "profit / a".to_string(),
            ..Default::default()
        },
    ];
    let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
        .unwrap()
        .exprs;
    assert_eq!(
        resolved.get("margin").unwrap(),
        "((SUM(x)) - (SUM(y))) / (SUM(x))"
    );
}

#[test]
fn inline_derived_word_boundary_safety() {
    let metrics = vec![
        Metric {
            name: "revenue".to_string(),
            expr: "SUM(amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        },
        Metric {
            name: "revenue_total".to_string(),
            expr: "SUM(total)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        },
        Metric {
            name: "derived".to_string(),
            expr: "revenue + revenue_total".to_string(),
            ..Default::default()
        },
    ];
    let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
        .unwrap()
        .exprs;
    assert_eq!(
        resolved.get("derived").unwrap(),
        "(SUM(amount)) + (SUM(total))"
    );
}

#[test]
fn expand_derived_metric_correct_sql() {
    let def = minimal_def("orders", "region", "region", "revenue", "SUM(amount)")
        .with_metric("cost", "SUM(unit_cost)", Some("o"))
        .with_metric("profit", "revenue - cost", None);
    // Fix revenue source_table to match original
    let mut def = def;
    def.metrics[0].source_table = Some("o".to_string());
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("profit")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("(SUM(amount)) - (SUM(unit_cost)) AS \"profit\""),
        "Derived metric must expand to inlined expression: {sql}"
    );
    assert!(
        sql.contains("GROUP BY\n    1"),
        "GROUP BY should reference only the dimension: {sql}"
    );
}

#[test]
fn expand_derived_only_no_base_metrics_requested() {
    let def = SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "li".to_string(),
                table: "line_items".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![Dimension {
            name: "region".to_string(),
            expr: "o.region".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        metrics: vec![
            Metric {
                name: "revenue".to_string(),
                expr: "SUM(li.amount)".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
            Metric {
                name: "cost".to_string(),
                expr: "SUM(li.unit_cost)".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
            Metric {
                name: "profit".to_string(),
                expr: "revenue - cost".to_string(),
                ..Default::default()
            },
        ],
        joins: vec![Join {
            table: "o".to_string(),
            from_alias: "li".to_string(),
            fk_columns: vec!["order_id".to_string()],
            ..Default::default()
        }],
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
    };
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("profit")],
    };
    let sql = expand("test", &def, &req).unwrap();
    // TECH-DEBT #71: both components live on `li`, so the derived metric is
    // computed from `li` and joins OUT to `o` for the dimension.
    assert!(
        sql.contains("FROM \"line_items\" AS \"li\"\nLEFT JOIN \"orders\" AS \"o\""),
        "a derived metric over li-based metrics must be anchored at li: {sql}"
    );
    assert!(
        sql.contains(
            "(SUM(CASE WHEN \"li\".\"id\" IS NOT NULL THEN li.amount END)) \
             - (SUM(CASE WHEN \"li\".\"id\" IS NOT NULL THEN li.unit_cost END)) AS \"profit\""
        ),
        "Derived metric expression must be inlined: {sql}"
    );
}

#[test]
fn resolve_joins_includes_transitive_deps_from_derived() {
    let def = SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "li".to_string(),
                table: "line_items".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![Dimension {
            name: "region".to_string(),
            expr: "o.region".to_string(),
            source_table: None,
            ..Default::default()
        }],
        metrics: vec![
            Metric {
                name: "revenue".to_string(),
                expr: "SUM(li.amount)".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
            Metric {
                name: "order_count".to_string(),
                expr: "COUNT(DISTINCT o.id)".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            },
            Metric {
                name: "avg_order_value".to_string(),
                expr: "revenue / order_count".to_string(),
                ..Default::default()
            },
        ],
        // OneToOne (declared): `avg_order_value` fuses base metrics on two
        // tables (`revenue` on li, `order_count` on o). Across a ManyToOne edge
        // that is now a fan trap (EXP-2, exercised by the fan_trap tests); this
        // test's purpose is the ORTHOGONAL mechanic that a derived metric pulls
        // its transitive dependency's join (`li`) into resolution, so the edge
        // is declared OneToOne to keep the query legal and reach that assertion.
        //
        // That mechanic lives in the BASE-ANCHORED resolver, which since
        // TECH-DEBT #71 sees a non-root metric only on a fallback shape: here,
        // the unqualified `region` dimension below (a qualified one would put
        // each component at its own grain and never consult the resolver).
        joins: vec![Join {
            table: "o".to_string(),
            from_alias: "li".to_string(),
            fk_columns: vec!["order_id".to_string()],
            cardinality: Cardinality::OneToOne,
            ..Default::default()
        }],
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
    };
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("avg_order_value")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("LEFT JOIN \"line_items\" AS \"li\""),
        "JOIN to li must be included for derived metric avg_order_value: {sql}"
    );
}

#[test]
fn expand_derived_metric_with_facts_chain() {
    let def = SemanticViewDefinition {
        tables: vec![],
        dimensions: vec![],
        metrics: vec![
            Metric {
                name: "revenue".to_string(),
                expr: "SUM(net_price)".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
            Metric {
                name: "cost".to_string(),
                expr: "SUM(unit_cost)".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
            Metric {
                name: "profit".to_string(),
                expr: "revenue - cost".to_string(),
                ..Default::default()
            },
        ],
        joins: vec![],
        facts: vec![Fact {
            name: "net_price".to_string(),
            expr: "extended_price * (1 - discount)".to_string(),
            source_table: Some("li".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
        }],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
    };
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("profit")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("(SUM((extended_price * (1 - discount)))) - (SUM(unit_cost)) AS \"profit\""),
        "Fact->base->derived chain must resolve correctly: {sql}"
    );
}

// EXP-24 (code-review 2026-08-06): the derived-metric replacement map is keyed
// by BARE canonical names only, while the fact path's `insert_fact_keys` and
// per-grain's `decompose` insert bare AND own-qualified keys. Every detection
// site uses `references_ref`, which DOES match the qualified spelling, and
// `graph/member_refs.rs` documents `t1.metric_a + t2.metric_b` as one of "the
// legal cross-table forms, which must keep working".
//
// So a qualified reference contributed the base metric's table to grain/join/
// USING resolution, but `inline_derived_metrics` left the text verbatim.
// Verified against the expander before the fix: `double_rev AS li.item_rev * 2`
// emitted `SELECT li.item_rev * 2 AS "double_rev"` — a raw, unaggregated,
// unresolvable column. The multi-grain path handles the SAME spelling correctly
// via `decompose`, so the behaviour differed by emission path.

/// `orders` base + `li` child, with a base metric on the child and a derived
/// metric referencing it. Parameterised by the reference spelling.
fn qualified_derived_def(reference: &str) -> SemanticViewDefinition {
    crate::expand::test_helpers::orders_view()
        .clear_dimensions()
        .clear_metrics()
        .with_dimension("status", "status", None)
        .with_table("li", "line_items", &["id"])
        .with_metric("item_rev", "SUM(li.price)", Some("li"))
        .with_metric("double_rev", &format!("{reference} * 2"), None)
        .with_pkfk_join("li_orders", "li", "orders", &["order_id"], &["id"])
}

fn double_rev_req() -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("double_rev")],
    }
}

#[test]
fn own_qualified_derived_metric_reference_is_inlined() {
    let sql = expand(
        "orders",
        &qualified_derived_def("li.item_rev"),
        &double_rev_req(),
    )
    .unwrap();
    assert!(
        sql.contains(
            "(SUM(CASE WHEN \"li\".\"id\" IS NOT NULL THEN li.price END)) * 2 AS \"double_rev\""
        ),
        "a qualified reference must inline the base metric's aggregate: {sql}"
    );
    assert!(
        !sql.contains("li.item_rev"),
        "the reference must not survive as a raw column: {sql}"
    );
}

#[test]
fn own_qualified_derived_reference_matches_the_bare_spelling_exactly() {
    // The two spellings name the same metric, so they must emit the same SQL.
    // This is the property the bare-only keying broke.
    let qualified = expand(
        "orders",
        &qualified_derived_def("li.item_rev"),
        &double_rev_req(),
    )
    .unwrap();
    let bare = expand(
        "orders",
        &qualified_derived_def("item_rev"),
        &double_rev_req(),
    )
    .unwrap();
    assert_eq!(
        qualified, bare,
        "qualifying a reference with its own table must not change the SQL"
    );
}

#[test]
fn quoted_and_case_varied_qualified_reference_is_inlined() {
    // The keys are canonical, so quoting and case must be immaterial on both
    // halves — the same rule PARSE-8 applied to the validators.
    let sql = expand(
        "orders",
        &qualified_derived_def("\"LI\".\"Item_Rev\""),
        &double_rev_req(),
    )
    .unwrap();
    assert!(
        sql.contains(
            "(SUM(CASE WHEN \"li\".\"id\" IS NOT NULL THEN li.price END)) * 2 AS \"double_rev\""
        ),
        "quoting/case must not defeat the replacement: {sql}"
    );
}

/// Control: a qualifier naming a DIFFERENT table must not resolve to this
/// metric — the keying is qualified-aware, not qualifier-blind.
#[test]
fn wrong_table_qualified_reference_is_not_inlined() {
    let sql = expand(
        "orders",
        &qualified_derived_def("orders.item_rev"),
        &double_rev_req(),
    )
    .unwrap();
    assert!(
        !sql.contains("(SUM(li.price)) * 2"),
        "a reference qualified with the WRONG table must not resolve: {sql}"
    );
}
