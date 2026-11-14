//! Lowering of cross-table dimension references (Phase 8.2).
//!
//! `customer.d_customer_region AS region.d_region_name` (see
//! `graph::dim_refs`) reads the region's dimension along the path from
//! `customer`. A dimension referencing a related table's *fact* already works
//! end to end: the fact machinery joins the path, inlines the expression, and
//! guards the fan-out and role-playing checks. So each referenced dimension is
//! given a private fact twin on its own table, with the same expression, and
//! each reference is rewritten to name the twin. The rest of the expansion sees
//! a dimension over a related table's fact.
//!
//! A twin's expression is itself rewritten, so a reference to a reference
//! (fixture 11 X3) becomes a fact chain, which the fact toposort orders.
//!
//! Only the expansion's working copy changes: DESCRIBE, `GET_DDL` and the YAML
//! export show the reference as written, as Snowflake's do (X6).

use std::borrow::Cow;
use std::collections::HashMap;

use crate::expr_tokens::inline_references;
use crate::ident::normalize_ident_part;
use crate::model::{AccessModifier, Fact, SemanticViewDefinition};

use super::resolution::quote_ident;

/// The private fact twin's name for dimension `index`: `__sv_dimref_<index>`,
/// with `_` appended while it would match a member the view declares.
fn twin_name(def: &SemanticViewDefinition, index: usize) -> String {
    let mut name = format!("__sv_dimref_{index}");
    let taken = |n: &str| {
        def.facts
            .iter()
            .map(|f| &f.name)
            .chain(def.dimensions.iter().map(|d| &d.name))
            .chain(def.metrics.iter().map(|m| &m.name))
            .any(|m| crate::ident::ident_matches(m, n))
    };
    while taken(&name) {
        name.push('_');
    }
    name
}

/// `def` with every cross-table dimension reference lowered to a private fact
/// twin (see the module docs); borrowed unchanged when there is none.
#[must_use]
pub(super) fn lower_dimension_references(
    def: &SemanticViewDefinition,
) -> Cow<'_, SemanticViewDefinition> {
    let refs = crate::graph::dimension_references(def);
    if refs.is_empty() {
        return Cow::Borrowed(def);
    }
    let mut targets: Vec<usize> = refs.iter().flat_map(|r| r.targets.clone()).collect();
    targets.sort_unstable();
    targets.dedup();
    // A referenced dimension's key, qualified with its table, to its twin.
    let replacements: HashMap<String, String> = targets
        .iter()
        .filter_map(|&j| {
            let d = &def.dimensions[j];
            d.source_table.as_deref().map(|t| {
                (
                    normalize_ident_part(&format!("{t}.{}", d.name)),
                    format!("{}.{}", quote_ident(t), twin_name(def, j)),
                )
            })
        })
        .collect();
    let map: HashMap<String, &str> = replacements
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str()))
        .collect();
    let rewrite = |i: usize| -> String {
        // Only a dimension found to reference another table's dimension is
        // rewritten, so a chain naming its own table keeps its meaning.
        if refs.iter().any(|r| r.dim == i) {
            inline_references(&def.dimensions[i].expr, &map)
        } else {
            def.dimensions[i].expr.clone()
        }
    };
    let mut out = def.clone();
    for &j in &targets {
        let d = &def.dimensions[j];
        out.facts.push(Fact {
            name: twin_name(def, j),
            expr: rewrite(j),
            source_table: d.source_table.clone(),
            output_type: d.output_type.clone(),
            access: AccessModifier::Private,
            ..Default::default()
        });
    }
    for r in &refs {
        out.dimensions[r.dim].expr = rewrite(r.dim);
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cardinality, Dimension, Join, TableRef};

    fn def() -> SemanticViewDefinition {
        let t = |a: &str| TableRef {
            alias: a.to_string(),
            table: a.to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        };
        let d = |t: &str, n: &str, e: &str| Dimension {
            name: n.to_string(),
            expr: e.to_string(),
            source_table: Some(t.to_string()),
            ..Default::default()
        };
        SemanticViewDefinition {
            tables: vec![t("region"), t("customer"), t("orders")],
            joins: vec![
                Join {
                    name: Some("cr".into()),
                    from_alias: "customer".into(),
                    table: "region".into(),
                    fk_columns: vec!["r_id".into()],
                    cardinality: Cardinality::ManyToOne,
                    ..Default::default()
                },
                Join {
                    name: Some("oc".into()),
                    from_alias: "orders".into(),
                    table: "customer".into(),
                    fk_columns: vec!["c_id".into()],
                    cardinality: Cardinality::ManyToOne,
                    ..Default::default()
                },
            ],
            dimensions: vec![
                d("region", "r_name", "region.name"),
                d("customer", "c_region", "region.r_name"),
                d("orders", "o_region", "LOWER(customer.c_region)"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn references_become_private_fact_twins_and_chain() {
        let def = def();
        let out = lower_dimension_references(&def);
        assert_eq!(out.dimensions[1].expr, "\"region\".__sv_dimref_0");
        assert_eq!(out.dimensions[2].expr, "LOWER(\"customer\".__sv_dimref_1)");
        let twin = |n: &str| out.facts.iter().find(|f| f.name == n).expect(n).clone();
        assert_eq!(twin("__sv_dimref_0").expr, "region.name");
        assert_eq!(twin("__sv_dimref_1").expr, "\"region\".__sv_dimref_0");
        assert_eq!(
            twin("__sv_dimref_1").source_table.as_deref(),
            Some("customer")
        );
        assert_eq!(twin("__sv_dimref_0").access, AccessModifier::Private);
    }

    #[test]
    fn a_view_without_references_is_borrowed() {
        let mut def = def();
        def.dimensions.truncate(1);
        assert!(matches!(lower_dimension_references(&def), Cow::Borrowed(_)));
    }
}
