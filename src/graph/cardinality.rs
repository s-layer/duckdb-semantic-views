//! Cardinality inference for declared relationships.
//!
//! Given the tables and relationships parsed from a semantic-view definition,
//! resolves each relationship's `ref_columns` (the target-side FK columns) and
//! infers its [`Cardinality`] from PK/UNIQUE constraints on the FK side. This is
//! semantic-graph logic — it lives here rather than in `parse` so the parser and
//! `ddl` layers depend inward on the graph rather than the reverse (AR-1).

use std::collections::HashSet;

use crate::errors::ParseError;
use crate::model::{Cardinality, Join, TableRef};

/// Infer cardinality for each relationship based on PK/UNIQUE constraints.
/// Also resolves `ref_columns` (the columns on the target side of the FK reference).
///
/// Two checks per relationship:
/// 1. Resolve `ref_columns`: if empty, default to the target's PK. If the target
///    has no declared PK, this function does not error — it leaves `ref_columns`
///    empty and skips the relationship; the missing-PK case is raised later as a
///    hard error during enrichment (`crate::ddl::define::enrich_definition_for_create`).
/// 2. Infer cardinality: if FK columns match PK/UNIQUE on the `from_alias` table,
///    the relationship is `OneToOne`; otherwise `ManyToOne`.
pub(crate) fn infer_cardinality(
    tables: &[TableRef],
    relationships: &mut [Join],
) -> Result<(), ParseError> {
    for join in relationships.iter_mut() {
        if join.fk_columns.is_empty() {
            continue;
        }

        let to_alias_lower = join.table.to_ascii_lowercase();
        let from_alias_lower = join.from_alias.to_ascii_lowercase();

        // Find target table (REFERENCES target)
        let target = tables
            .iter()
            .find(|t| t.alias.to_ascii_lowercase() == to_alias_lower);

        // Find source table (from_alias side)
        let source = tables
            .iter()
            .find(|t| t.alias.to_ascii_lowercase() == from_alias_lower);

        // Step 1: Resolve ref_columns
        if join.ref_columns.is_empty() {
            // REFERENCES target (no column list) -> use target's PK
            match target {
                Some(t) if !t.pk_columns.is_empty() => {
                    join.ref_columns.clone_from(&t.pk_columns);
                }
                Some(_) => {
                    // Target has no PK declared in DDL -- silently skip
                    // here. Phase 65 (D-05/D-06): the v0.9.0 fallback to
                    // `resolve_pk_from_catalog` against duckdb_constraints()
                    // is gone. The empty `ref_columns` is caught in
                    // `crate::ddl::define::enrich_definition_for_create`
                    // step 2 with the D-06 hard error pointing the user
                    // at the missing PRIMARY KEY / UNIQUE declaration in
                    // the TABLES clause.
                    continue;
                }
                None => {
                    // Target not found -- will be caught by graph validation later
                }
            }
        }
        // When ref_columns was set explicitly (REFERENCES target(cols)),
        // validation against PK/UNIQUE on target happens in graph
        // relationship validation (CARD-03).

        // Step 2: FK column count must match ref column count
        if !join.ref_columns.is_empty() && join.fk_columns.len() != join.ref_columns.len() {
            let rel_name = join.name.as_deref().unwrap_or("?");
            return Err(ParseError {
                message: format!(
                    "FK column count ({}) does not match referenced column count ({}) \
                     in relationship '{rel_name}'.",
                    join.fk_columns.len(),
                    join.ref_columns.len(),
                ),
                position: None,
            });
        }

        // Step 3: Infer cardinality from FK-side constraints (CARD-04)
        if let Some(source) = source {
            let fk_set: HashSet<String> = join
                .fk_columns
                .iter()
                .map(|c| c.to_ascii_lowercase())
                .collect();

            // Check against source PK
            let pk_set: HashSet<String> = source
                .pk_columns
                .iter()
                .map(|c| c.to_ascii_lowercase())
                .collect();

            if !pk_set.is_empty() && fk_set == pk_set {
                join.cardinality = Cardinality::OneToOne;
            } else {
                // Check against source UNIQUE constraints
                let matches_unique = source.unique_constraints.iter().any(|uc| {
                    let uc_set: HashSet<String> =
                        uc.iter().map(|c| c.to_ascii_lowercase()).collect();
                    fk_set == uc_set
                });
                join.cardinality = if matches_unique {
                    Cardinality::OneToOne
                } else {
                    Cardinality::ManyToOne
                };
            }
            // Phase 7.1: an ASOF relationship is many-to-one whatever keys the
            // referencing side declares. Each referencing row matches at most
            // one referenced row, but many referencing rows can match the same
            // one, so the referenced table is never on a "one" side it could
            // be grouped from (Snowflake refuses that query, 010956).
            // Phase 7.2: so is a range relationship, whose referenced rows are
            // periods that many referencing rows can fall in.
            if join.asof.is_some() || join.range_end.is_some() {
                join.cardinality = Cardinality::ManyToOne;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 7.1: an ASOF relationship stays many-to-one even when its
    /// foreign-key columns are the referencing table's key, since many
    /// referencing rows can take the same referenced row.
    #[test]
    fn asof_relationship_is_many_to_one_even_on_a_unique_fk() {
        let tables = vec![
            TableRef {
                alias: "o".into(),
                table: "orders".into(),
                pk_columns: vec!["cust".into(), "d".into()],
                ..Default::default()
            },
            TableRef {
                alias: "a".into(),
                table: "addresses".into(),
                pk_columns: vec!["cust".into(), "s".into()],
                ..Default::default()
            },
        ];
        let mut joins = vec![Join {
            from_alias: "o".into(),
            table: "a".into(),
            fk_columns: vec!["cust".into(), "d".into()],
            ref_columns: vec!["cust".into(), "s".into()],
            asof: Some(1),
            ..Default::default()
        }];
        infer_cardinality(&tables, &mut joins).unwrap();
        assert_eq!(joins[0].cardinality, Cardinality::ManyToOne);
        joins[0].asof = None;
        infer_cardinality(&tables, &mut joins).unwrap();
        assert_eq!(
            joins[0].cardinality,
            Cardinality::OneToOne,
            "control: no ASOF"
        );
    }
}
