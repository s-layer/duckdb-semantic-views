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
    // Phase 8.3: a fact holding a window function may reference one too.
    let fact_refs = crate::graph::window_fact_references(def);
    if refs.is_empty() && fact_refs.is_empty() {
        return Cow::Borrowed(def);
    }
    let mut targets: Vec<usize> = refs
        .iter()
        .chain(&fact_refs)
        .flat_map(|r| r.targets.clone())
        .collect();
    targets.sort_unstable();
    targets.dedup();
    // A referenced dimension's key, qualified with its table, to its twin.
    let twin_ref = |j: usize| -> Option<(String, String)> {
        let d = &def.dimensions[j];
        d.source_table.as_deref().map(|t| {
            (
                normalize_ident_part(&format!("{t}.{}", d.name)),
                format!("{}.{}", quote_ident(t), twin_name(def, j)),
            )
        })
    };
    let rewrite = |i: usize| -> String {
        // Only the chains found to reference another table's dimension are
        // rewritten, so a chain naming the dimension's own table keeps its
        // meaning, even when some other dimension references that member.
        let Some(r) = refs.iter().find(|r| r.dim == i) else {
            return def.dimensions[i].expr.clone();
        };
        let replacements: Vec<(String, String)> =
            r.targets.iter().filter_map(|&j| twin_ref(j)).collect();
        let map: HashMap<String, &str> = replacements
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str()))
            .collect();
        inline_references(&def.dimensions[i].expr, &map)
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
    for r in &fact_refs {
        let map: HashMap<String, String> = r.targets.iter().filter_map(|&j| twin_ref(j)).collect();
        let map: HashMap<String, &str> = map.iter().map(|(k, v)| (k.clone(), v.as_str())).collect();
        out.facts[r.dim].expr = inline_references(&def.facts[r.dim].expr, &map);
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

    /// Review of #362: `seg` reads its own column and references `region.n`;
    /// `c_desc` reads `seg`'s column beside the reference; `o_seg` references
    /// `seg`.
    fn own_table_def() -> SemanticViewDefinition {
        let mut def = def();
        def.dimensions = vec![
            Dimension {
                name: "n".into(),
                expr: "region.name".into(),
                source_table: Some("region".into()),
                ..Default::default()
            },
            Dimension {
                name: "seg".into(),
                expr: "COALESCE(customer.seg, region.n)".into(),
                source_table: Some("customer".into()),
                ..Default::default()
            },
            Dimension {
                name: "c_desc".into(),
                expr: "customer.seg || '/' || region.n".into(),
                source_table: Some("customer".into()),
                ..Default::default()
            },
            Dimension {
                name: "o_seg".into(),
                expr: "customer.seg".into(),
                source_table: Some("orders".into()),
                ..Default::default()
            },
        ];
        def
    }

    /// A chain naming the dimension's own table keeps its meaning, even when
    /// another dimension references that member: no twin reads itself.
    #[test]
    fn an_own_table_chain_is_not_rewritten_to_a_twin() {
        let def = own_table_def();
        let out = lower_dimension_references(&def);
        let twin = |n: &str| out.facts.iter().find(|f| f.name == n).expect(n).clone();
        // `seg`'s own `customer.seg` is its column, not its own twin.
        assert_eq!(
            twin("__sv_dimref_1").expr,
            "COALESCE(customer.seg, \"region\".__sv_dimref_0)"
        );
        assert_eq!(
            out.dimensions[1].expr,
            "COALESCE(customer.seg, \"region\".__sv_dimref_0)"
        );
        assert_eq!(out.dimensions[3].expr, "\"customer\".__sv_dimref_1");
    }

    /// A dimension's own-table chain does not change meaning because some
    /// other dimension references that member.
    #[test]
    fn an_own_table_chain_does_not_drift_with_other_references() {
        let def = own_table_def();
        let out = lower_dimension_references(&def);
        // `c_desc` reads the raw column whether or not `o_seg` exists.
        assert_eq!(
            out.dimensions[2].expr,
            "customer.seg || '/' || \"region\".__sv_dimref_0"
        );
    }

    #[test]
    fn a_view_without_references_is_borrowed() {
        let mut def = def();
        def.dimensions.truncate(1);
        assert!(matches!(lower_dimension_references(&def), Cow::Borrowed(_)));
    }
}
