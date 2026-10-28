//! Fact expression DAG inlining into metric aggregates.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase29_fact_inlining_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::facts::{inline_facts, toposort_facts};
use crate::expand::test_helpers::{minimal_def, TestFixtureExt};
use crate::model::{AccessModifier, Fact, SemanticViewDefinition};

#[test]
fn toposort_facts_empty() {
    let order = toposort_facts(&[]).unwrap();
    assert!(order.is_empty());
}

#[test]
fn toposort_facts_independent() {
    let facts = vec![
        Fact {
            name: "a".to_string(),
            expr: "x + 1".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
        Fact {
            name: "b".to_string(),
            expr: "y + 2".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
    ];
    let order = toposort_facts(&facts).unwrap();
    assert_eq!(order.len(), 2);
    assert!(order.contains(&0));
    assert!(order.contains(&1));
}

#[test]
fn toposort_facts_chain() {
    let facts = vec![
        Fact {
            name: "a".to_string(),
            expr: "price * qty".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
        Fact {
            name: "b".to_string(),
            expr: "a * (1 - discount)".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
    ];
    let order = toposort_facts(&facts).unwrap();
    assert_eq!(order.len(), 2);
    let a_pos = order.iter().position(|&x| x == 0).unwrap();
    let b_pos = order.iter().position(|&x| x == 1).unwrap();
    assert!(a_pos < b_pos, "a (leaf) must come before b (depends on a)");
}

#[test]
fn toposort_facts_three_level_chain() {
    let facts = vec![
        Fact {
            name: "a".to_string(),
            expr: "price".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
        Fact {
            name: "b".to_string(),
            expr: "a * qty".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
        Fact {
            name: "c".to_string(),
            expr: "b * tax".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
    ];
    let order = toposort_facts(&facts).unwrap();
    assert_eq!(order.len(), 3);
    let a_pos = order.iter().position(|&x| x == 0).unwrap();
    let b_pos = order.iter().position(|&x| x == 1).unwrap();
    let c_pos = order.iter().position(|&x| x == 2).unwrap();
    assert!(a_pos < b_pos);
    assert!(b_pos < c_pos);
}

#[test]
fn inline_facts_no_facts() {
    let result = inline_facts("SUM(price)", &[], &[]);
    assert_eq!(result, "SUM(price)");
}

#[test]
fn inline_facts_single_fact() {
    let facts = vec![Fact {
        name: "net_price".to_string(),
        expr: "price * (1 - discount)".to_string(),
        source_table: None,
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: false,
        access: AccessModifier::Public,
        tags: Vec::new(),
        sample_values: Vec::new(),
    }];
    let order = toposort_facts(&facts).unwrap();
    let result = inline_facts("SUM(net_price)", &facts, &order);
    assert_eq!(result, "SUM((price * (1 - discount)))");
}

#[test]
fn inline_facts_multi_level() {
    let facts = vec![
        Fact {
            name: "a".to_string(),
            expr: "price * qty".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
        Fact {
            name: "b".to_string(),
            expr: "a * (1 - discount)".to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        },
    ];
    let order = toposort_facts(&facts).unwrap();
    let result = inline_facts("SUM(b)", &facts, &order);
    assert_eq!(result, "SUM(((price * qty) * (1 - discount)))");
}

#[test]
fn inline_facts_preserves_parenthesization() {
    let facts = vec![Fact {
        name: "total".to_string(),
        expr: "a + b".to_string(),
        source_table: None,
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: false,
        access: AccessModifier::Public,
        tags: Vec::new(),
        sample_values: Vec::new(),
    }];
    let order = toposort_facts(&facts).unwrap();
    let result = inline_facts("x * total", &facts, &order);
    assert_eq!(result, "x * (a + b)");
}

#[test]
fn inline_facts_word_boundary_prevents_collision() {
    let facts = vec![Fact {
        name: "net_price".to_string(),
        expr: "p * q".to_string(),
        source_table: None,
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: false,
        access: AccessModifier::Public,
        tags: Vec::new(),
        sample_values: Vec::new(),
    }];
    let order = toposort_facts(&facts).unwrap();
    let result = inline_facts("SUM(net_price_total)", &facts, &order);
    assert_eq!(
        result, "SUM(net_price_total)",
        "Word boundary must prevent matching"
    );
}

#[test]
fn inline_facts_with_qualified_name_in_metric() {
    let facts = vec![Fact {
        name: "net_price".to_string(),
        expr: "li.price * (1 - li.discount)".to_string(),
        source_table: Some("li".to_string()),
        output_type: None,
        comment: None,
        synonyms: vec![],
        is_filter: false,
        access: AccessModifier::Public,
        tags: Vec::new(),
        sample_values: Vec::new(),
    }];
    let order = toposort_facts(&facts).unwrap();
    let result = inline_facts("SUM(li.net_price)", &facts, &order);
    assert_eq!(result, "SUM((li.price * (1 - li.discount)))");
}

#[test]
fn expand_with_facts_inlines_into_metric() {
    let def = minimal_def(
        "line_items",
        "region",
        "region",
        "total_net",
        "SUM(net_price)",
    )
    .with_fact("net_price", "price * (1 - discount)", "line_items");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("total_net")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("SUM((price * (1 - discount)))"),
        "Fact inlining must resolve net_price in metric expr: {sql}"
    );
}

#[test]
fn expand_without_facts_unchanged() {
    let def = SemanticViewDefinition::default()
        .with_table("orders", "orders", &[])
        .with_metric("total", "SUM(amount)", None);
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("total")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("SUM(amount) AS"),
        "Without facts, metric expr unchanged: {sql}"
    );
}

#[test]
fn expand_multi_level_facts() {
    let def = SemanticViewDefinition::default()
        .with_table("line_items", "line_items", &[])
        .with_metric("total_tax", "SUM(tax_amount)", None)
        .with_fact("net_price", "extended_price * (1 - discount)", "line_items")
        .with_fact("tax_amount", "net_price * tax_rate", "line_items");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("total_tax")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("SUM(((extended_price * (1 - discount)) * tax_rate))"),
        "Multi-level fact chain must resolve correctly: {sql}"
    );
}
