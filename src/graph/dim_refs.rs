//! Cross-table dimension references (Phase 8.2, Snowflake fixture 11 X0-X10).
//!
//! In Snowflake's DDL a dimension may be written as another table's dimension:
//! `customer.d_customer_region AS region.d_region_name` exposes the region's
//! name on `customer`, resolved along the relationship path from `customer` to
//! `region`. A reference may sit inside an expression (`LOWER(region.r)`, X7)
//! and may name a dimension that is itself a reference (X3).
//!
//! Snowflake refuses two forms at CREATE, and so does this module:
//!
//! - a reference to a dimension of a table the referencing table does not
//!   reach along foreign keys, such as a child table's (010214, X8): its value
//!   is not one per row of the referencing table;
//! - a reference to a table reached two ways, such as a role-played one
//!   (010227, X9): which role is meant is not said.
//!
//! A reference here is a qualified `<table>.<dimension>` chain whose table is
//! another declared table and whose name is a dimension of that table. Only a
//! dimension's expression is read this way. A qualified chain naming a member
//! of the dimension's own table, or a column, keeps its old meaning.
//!
//! The expansion lowers each reference to a private fact on the referenced
//! table (`expand::dim_refs`), so the fact machinery joins the path, inlines
//! the expression and guards the fan-out and role checks.

use crate::errors::ParseError;
use crate::expr_tokens::scan_references;
use crate::ident::normalize_ident_part;
use crate::model::SemanticViewDefinition;

/// One dimension's references to other tables' dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DimRefs {
    /// The referencing member, an index into `def.dimensions`, or into
    /// `def.facts` from [`window_fact_references`].
    pub(crate) dim: usize,
    /// The referenced dimensions, indices into `def.dimensions`, in first
    /// appearance order, each once.
    pub(crate) targets: Vec<usize>,
}

/// Every dimension that references another table's dimension, with what it
/// references. Empty for a view with none (or with no tables).
pub(crate) fn dimension_references(def: &SemanticViewDefinition) -> Vec<DimRefs> {
    let exprs = def
        .dimensions
        .iter()
        .map(|d| (d.source_table.as_deref(), d.expr.as_str()));
    references_from(def, exprs)
}

/// Phase 8.3: every fact holding a window function that references another
/// table's dimension (`SUM(revenue) OVER (PARTITION BY st.region)`, Snowflake
/// fixture 11 W9), with what it references; `dim` is the fact's index into
/// `def.facts`. Such a fact is computed over its table before the query
/// (`expand::window_members`), so a parent's dimension is read as a
/// dimension's reference is.
pub(crate) fn window_fact_references(def: &SemanticViewDefinition) -> Vec<DimRefs> {
    let exprs = def.facts.iter().map(|f| {
        let expr = if crate::expr_tokens::has_window_call(&f.expr) {
            f.expr.as_str()
        } else {
            ""
        };
        (f.source_table.as_deref(), expr)
    });
    references_from(def, exprs)
}

/// The references to other tables' dimensions in each of `exprs`, a member's
/// table and expression, indexed in the order given.
fn references_from<'a>(
    def: &SemanticViewDefinition,
    exprs: impl Iterator<Item = (Option<&'a str>, &'a str)>,
) -> Vec<DimRefs> {
    let Some(base) = def.tables.first().map(|t| normalize_ident_part(&t.alias)) else {
        return Vec::new();
    };
    let keys: Vec<Option<String>> = def
        .dimensions
        .iter()
        .map(|d| {
            d.source_table
                .as_deref()
                .map(|t| normalize_ident_part(&format!("{t}.{}", d.name)))
        })
        .collect();
    let mut out = Vec::new();
    for (i, (table, expr)) in exprs.enumerate() {
        let own = table.map_or_else(|| base.clone(), normalize_ident_part);
        let mut targets = Vec::new();
        for chain in scan_references(expr) {
            if chain.is_bare() || chain.first_part_key() == own {
                continue;
            }
            let key = chain.key();
            let Some(j) = keys.iter().position(|k| k.as_deref() == Some(key.as_str())) else {
                continue;
            };
            if !targets.contains(&j) {
                targets.push(j);
            }
        }
        if !targets.is_empty() {
            out.push(DimRefs { dim: i, targets });
        }
    }
    out
}

/// Depth-first search for a cycle of references from `at`; `state` is 0
/// (unvisited), 1 (on the path) or 2 (done). Returns a dimension on the cycle.
fn visit(at: usize, refs: &[DimRefs], state: &mut [u8]) -> Option<usize> {
    state[at] = 1;
    let targets = refs
        .iter()
        .find(|r| r.dim == at)
        .map_or(&[][..], |r| r.targets.as_slice());
    for &to in targets {
        match state[to] {
            0 => {
                if let Some(hit) = visit(to, refs, state) {
                    return Some(hit);
                }
            }
            1 => return Some(to),
            _ => {}
        }
    }
    state[at] = 2;
    None
}

/// Refuse a dimension reference Snowflake refuses at CREATE (see the module
/// docs), and a cycle of references.
pub fn validate_dimension_references(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    let refs = dimension_references(def);
    let fact_refs = window_fact_references(def);
    if refs.is_empty() && fact_refs.is_empty() {
        return Ok(());
    }
    let base = def
        .tables
        .first()
        .map(|t| t.alias.clone())
        .unwrap_or_default();
    let referrers = refs
        .iter()
        .map(|r| {
            let d = &def.dimensions[r.dim];
            ("dimension", d.source_table.as_ref(), &d.name, r)
        })
        .chain(fact_refs.iter().map(|r| {
            let f = &def.facts[r.dim];
            ("fact", f.source_table.as_ref(), &f.name, r)
        }));
    for (kind, own, name, r) in referrers {
        let own = own.cloned().unwrap_or_else(|| base.clone());
        for &j in &r.targets {
            let target = &def.dimensions[j];
            let table = target.source_table.as_deref().unwrap_or_default();
            match super::window_dims::chain_count(def, &own, table, &[]) {
                1 => {}
                0 => {
                    return Err(ParseError::positionless(format!(
                        "{kind} '{own}.{name}' references dimension '{table}.{target}', but \
                         table '{own}' does not reach table '{table}' along its relationships, so \
                         '{target}' has no single value per row of '{own}'. As in Snowflake, a \
                         {kind} can reference a dimension of a table it references, directly \
                         or through other tables, not of a table that references it.",
                        target = target.name,
                    )));
                }
                _ => {
                    return Err(ParseError::positionless(format!(
                        "{kind} '{own}.{name}' references dimension '{table}.{target}', but \
                         table '{own}' reaches table '{table}' by more than one path (for \
                         example, as two roles), so which one is meant is not said. As in \
                         Snowflake, a dimension reference must reach its table by a single \
                         path; define a {kind} on '{own}' for each role instead.",
                        target = target.name,
                    )));
                }
            }
        }
    }
    // A cycle needs a one-to-one relationship walked both ways; refuse it
    // rather than let the expansion meet it.
    // 0 = unvisited, 1 = on the path, 2 = done.
    let mut state = vec![0_u8; def.dimensions.len()];
    for r in &refs {
        if state[r.dim] == 0 {
            if let Some(hit) = visit(r.dim, &refs, &mut state) {
                return Err(ParseError::positionless(format!(
                    "dimension '{}' references itself through other dimensions' references; \
                     a dimension reference cannot form a cycle.",
                    def.dimensions[hit].name
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cardinality, Dimension, Join, TableRef};

    fn table(alias: &str) -> TableRef {
        TableRef {
            alias: alias.to_string(),
            table: alias.to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }
    }

    fn dim(table: &str, name: &str, expr: &str) -> Dimension {
        Dimension {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some(table.to_string()),
            ..Default::default()
        }
    }

    fn join(name: &str, from: &str, to: &str, card: Cardinality) -> Join {
        Join {
            name: Some(name.to_string()),
            from_alias: from.to_string(),
            table: to.to_string(),
            fk_columns: vec![format!("{to}_id")],
            cardinality: card,
            ..Default::default()
        }
    }

    /// region <- nation <- customer, the docs' chain.
    fn chain(dims: Vec<Dimension>) -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![table("region"), table("nation"), table("customer")],
            joins: vec![
                join("nr", "nation", "region", Cardinality::ManyToOne),
                join("cn", "customer", "nation", Cardinality::ManyToOne),
            ],
            dimensions: dims,
            ..Default::default()
        }
    }

    #[test]
    fn a_reference_two_hops_up_is_found_and_accepted() {
        let def = chain(vec![
            dim("region", "r_name", "name"),
            dim("customer", "c_region", "LOWER(region.r_name)"),
        ]);
        assert_eq!(
            dimension_references(&def),
            vec![DimRefs {
                dim: 1,
                targets: vec![0]
            }]
        );
        validate_dimension_references(&def).expect("accepted");
    }

    #[test]
    fn a_chain_naming_an_own_table_member_or_a_column_is_not_a_reference() {
        let def = chain(vec![
            dim("customer", "c_name", "name"),
            dim("customer", "c_upper", "UPPER(customer.c_name)"),
            dim("customer", "c_col", "region.not_a_dimension"),
        ]);
        assert!(dimension_references(&def).is_empty());
    }

    #[test]
    fn a_reference_to_a_child_tables_dimension_is_refused() {
        let def = chain(vec![
            dim("customer", "c_name", "name"),
            dim("region", "any_customer", "customer.c_name"),
        ]);
        let e = validate_dimension_references(&def).unwrap_err().message;
        assert!(e.contains("does not reach table 'customer'"), "{e}");
    }

    #[test]
    fn a_reference_to_a_table_reached_two_ways_is_refused() {
        let def = SemanticViewDefinition {
            tables: vec![table("nation"), table("f")],
            joins: vec![
                join("dep", "f", "nation", Cardinality::ManyToOne),
                join("arr", "f", "nation", Cardinality::ManyToOne),
            ],
            dimensions: vec![dim("nation", "n", "name"), dim("f", "w", "nation.n")],
            ..Default::default()
        };
        let e = validate_dimension_references(&def).unwrap_err().message;
        assert!(e.contains("more than one path"), "{e}");
    }

    /// Phase 8.3: a window fact's references are found and checked as a
    /// dimension's; a plain fact's chain is not a reference.
    #[test]
    fn a_window_facts_reference_is_found_and_checked() {
        let fact = |table: &str, name: &str, expr: &str| crate::model::Fact {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some(table.to_string()),
            ..Default::default()
        };
        let mut def = chain(vec![dim("region", "r_name", "name")]);
        def.facts = vec![
            fact("customer", "plain", "region.r_name"),
            fact(
                "customer",
                "n_in_region",
                "COUNT(*) OVER (PARTITION BY region.r_name)",
            ),
        ];
        assert_eq!(
            window_fact_references(&def),
            vec![DimRefs {
                dim: 1,
                targets: vec![0]
            }]
        );
        validate_dimension_references(&def).expect("accepted");
        def.dimensions.push(dim("customer", "c_name", "name"));
        def.facts = vec![fact(
            "region",
            "rk",
            "RANK() OVER (ORDER BY customer.c_name)",
        )];
        let e = validate_dimension_references(&def).unwrap_err().message;
        assert!(e.starts_with("fact 'region.rk'"), "{e}");
        assert!(e.contains("does not reach table 'customer'"), "{e}");
    }

    #[test]
    fn a_cycle_through_a_one_to_one_relationship_is_refused() {
        let def = SemanticViewDefinition {
            tables: vec![table("a"), table("b")],
            joins: vec![join("ab", "a", "b", Cardinality::OneToOne)],
            dimensions: vec![dim("a", "x", "b.y"), dim("b", "y", "a.x")],
            ..Default::default()
        };
        let e = validate_dimension_references(&def).unwrap_err().message;
        assert!(e.contains("cycle"), "{e}");
    }
}
