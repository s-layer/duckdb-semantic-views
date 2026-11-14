//! Direct many-to-many relationships (Phase 8.4).
//!
//! Snowflake reads a relationship whose referenced columns are not the
//! referenced table's PRIMARY KEY or a UNIQUE key as many-to-many:
//! `orders (promo_code) REFERENCES promotions (promo_code)`, where promotions
//! holds several rows per code, matches each order with every promotions row of
//! its code. A metric grouped by a dimension across it counts each of its
//! table's rows once per value it matches; a query with no metrics lists the
//! combinations that occur together, every join inner.
//!
//! That is what a bridge gives, so the expansion's working copy reads the
//! relationship through one: a hidden table of the distinct non-NULL key values
//! the referenced table holds, which both tables reference many-to-one.
//! `orders -> promotions` becomes `orders -> K <- promotions`, and every later
//! step sees a fan-out from `K` to promotions, which the once-per-group
//! machinery already counts as Snowflake does. A NULL key matches no row, as in
//! Snowflake (`=` never holds for NULL), and an order whose code no promotion
//! has reaches a `K` row of none, so it keeps its row with NULLs past it.
//!
//! CREATE checks the view on this copy too
//! (`ddl::define::enrich_definition_for_create`), so a check that walks the
//! relationships sees no step from orders to promotions: a dimension
//! reference, a window member or a window metric across the relationship is
//! refused, as Snowflake refuses it (010227). Only the working copy changes:
//! DESCRIBE, `GET_DDL` and the YAML export show the relationship as written.

use std::borrow::Cow;

use crate::ident::{ident_matches, normalize_ident_part};
use crate::model::{Cardinality, Join, Metric, SemanticViewDefinition, TableRef};

use super::types::ExpandError;

use super::resolution::{quote_stored_ident, table_source};

/// The prefix of the alias a many-to-many relationship's key table takes.
const KEY_TABLE_PREFIX: &str = "__sv_m2m_";

/// The suffix of the name of the relationship from a many-to-many
/// relationship's referenced table to its key table.
const BACK_SUFFIX: &str = " (many-to-many)";

/// The column a stored key column names: `"Promo Code"` is `Promo Code`.
fn column_name(stored: &str) -> String {
    match crate::ident::parse_qualified_identifier(stored) {
        Ok(mut parts) if parts.len() == 1 => parts.pop().unwrap_or_default(),
        _ => stored.to_string(),
    }
}

/// Whether `alias` names a key table [`lower_many_to_many`] added: one the
/// referenced table references by the generated relationship it names
/// `<relationship> (many-to-many)`. A declared relationship with a generated
/// name is always named `SYS_RELATIONSHIP_<id>` (`Join::generated_name`), so
/// a table the view declares, whatever its alias, is never one.
pub(crate) fn is_key_table(def: &SemanticViewDefinition, alias: &str) -> bool {
    normalize_ident_part(alias).starts_with(KEY_TABLE_PREFIX)
        && def
            .tables
            .iter()
            .any(|t| ident_matches(&t.alias, alias) && t.definition.is_some())
        && def.joins.iter().any(|j| {
            j.generated_name
                && ident_matches(&j.table, alias)
                && j.name.as_deref().is_some_and(|n| n.ends_with(BACK_SUFFIX))
        })
}

/// Whether `def` holds a key table [`lower_many_to_many`] added.
pub(crate) fn has_key_table(def: &SemanticViewDefinition) -> bool {
    def.tables.iter().any(|t| is_key_table(def, &t.alias))
}

/// Whether `def` declares a direct many-to-many relationship.
#[must_use]
pub fn has_many_to_many(def: &SemanticViewDefinition) -> bool {
    def.joins
        .iter()
        .any(|j| j.cardinality == Cardinality::ManyToMany)
}

/// `def` with each many-to-many relationship read through a key table: the
/// relationship references the key table instead, by the same name, and the
/// referenced table references it too, many-to-one.
#[must_use]
pub fn lower_many_to_many(def: &SemanticViewDefinition) -> Cow<'_, SemanticViewDefinition> {
    if !has_many_to_many(def) {
        return Cow::Borrowed(def);
    }
    let mut out = def.clone();
    let taken = |name: &str, out: &SemanticViewDefinition| {
        out.tables.iter().any(|t| ident_matches(&t.alias, name))
            || out
                .joins
                .iter()
                .any(|j| j.name.as_deref().is_some_and(|n| ident_matches(n, name)))
    };
    for i in 0..def.joins.len() {
        if def.joins[i].cardinality != Cardinality::ManyToMany {
            continue;
        }
        let join = &def.joins[i];
        let target = def
            .tables
            .iter()
            .find(|t| ident_matches(&t.alias, &join.table))
            .filter(|_| !join.ref_columns.is_empty());
        let Some(target) = target else {
            // An unknown table is the graph check's to report; CREATE infers
            // many-to-many only for named referenced columns.
            out.joins[i].cardinality = Cardinality::ManyToOne;
            continue;
        };
        let mut alias = format!("{KEY_TABLE_PREFIX}{i}");
        while taken(&alias, &out) {
            alias.push('_');
        }
        // Named after the relationship, so a message that names the hop from
        // the key table to the referenced table names the relationship.
        let mut ref_name = format!(
            "{}{BACK_SUFFIX}",
            join.name.as_deref().unwrap_or(&join.table)
        );
        while taken(&ref_name, &out) {
            let at = ref_name.len() - BACK_SUFFIX.len();
            ref_name.insert(at, '_');
        }
        let target_alias = quote_stored_ident(&target.alias);
        // Key columns are stored as written, quotes included.
        let columns: Vec<String> = join
            .ref_columns
            .iter()
            .map(|c| format!("{target_alias}.{}", quote_stored_ident(c)))
            .collect();
        let not_null: Vec<String> = columns.iter().map(|c| format!("{c} IS NOT NULL")).collect();
        let query = format!(
            "SELECT DISTINCT {} FROM {} AS {target_alias} WHERE {}",
            columns.join(", "),
            table_source(target, def),
            not_null.join(" AND "),
        );
        out.tables.push(TableRef {
            alias: alias.clone(),
            table: alias.clone(),
            definition: Some(query),
            pk_columns: join.ref_columns.clone(),
            ..TableRef::default()
        });
        if !out.table_columns.is_empty() {
            // `table_columns` holds column names, not stored identifiers.
            let names = join.ref_columns.iter().map(|c| column_name(c)).collect();
            out.table_columns.insert(alias.clone(), names);
        }
        out.joins[i].table.clone_from(&alias);
        out.joins[i].cardinality = Cardinality::ManyToOne;
        out.joins.push(Join {
            table: alias,
            from_alias: target.alias.clone(),
            fk_columns: join.ref_columns.clone(),
            ref_columns: join.ref_columns.clone(),
            name: Some(ref_name),
            cardinality: Cardinality::ManyToOne,
            generated_name: true,
            ..Join::default()
        });
    }
    Cow::Owned(out)
}

/// The name of the many-to-many relationship `key` is the key table of: the
/// first relationship that references it, which [`lower_many_to_many`] left in
/// the relationship's place.
pub(crate) fn relationship_of(def: &SemanticViewDefinition, key: &str) -> String {
    def.joins
        .iter()
        .find(|j| ident_matches(&j.table, key))
        .and_then(|j| j.name.clone())
        .unwrap_or_else(|| key.to_string())
}

/// The many-to-many relationship a path (`[from, …, to]`) crosses, if any: a
/// key table inside it, which the path enters along one side's foreign key and
/// leaves fanning out to the other side.
pub(crate) fn crossed(def: &SemanticViewDefinition, path: &[String]) -> Option<String> {
    let inner = path.get(1..path.len().saturating_sub(1))?;
    inner
        .iter()
        .find(|t| is_key_table(def, t))
        .map(|k| relationship_of(def, k))
}

/// Refuses a metric grouped or filtered across a many-to-many relationship
/// when its table has no PRIMARY KEY: each of its rows is counted once per
/// value it matches, by that key, as in Snowflake, which refuses it the same
/// way (010933, fixture 11's M7 and M14).
pub(super) fn check_metric_keys(
    view_name: &str,
    def: &SemanticViewDefinition,
    metrics: &[&Metric],
    members: &[super::multi_path::Member],
) -> Result<(), ExpandError> {
    if !has_key_table(def) {
        return Ok(());
    }
    for met in metrics {
        let tables = match &met.source_table {
            Some(t) => vec![t.clone()],
            None => super::facts::collect_derived_metric_source_tables(met, &def.metrics),
        };
        for home in tables {
            let keyless = def
                .tables
                .iter()
                .any(|t| ident_matches(&t.alias, &home) && t.pk_columns.is_empty());
            if !keyless {
                continue;
            }
            let home = home.to_ascii_lowercase();
            for member in members {
                let (to, via) = member.route();
                let legs: Vec<(String, String)> = match via {
                    Some(v) => vec![(home.clone(), v.clone()), (v, to)],
                    None => vec![(home.clone(), to)],
                };
                let relationship = legs.iter().find_map(|(a, b)| {
                    super::multi_path::undirected_path(def, a, b, None)
                        .and_then(|p| crossed(def, &p))
                });
                if let Some(relationship) = relationship {
                    return Err(ExpandError::ManyToManyWithoutKey {
                        view_name: view_name.to_string(),
                        subject: format!("metric '{}'", met.name),
                        table: home,
                        member: member.label.clone(),
                        relationship,
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> SemanticViewDefinition {
        let t = |a: &str, pk: &[&str]| TableRef {
            alias: a.to_string(),
            table: format!("m_{a}"),
            pk_columns: pk.iter().map(|c| (*c).to_string()).collect(),
            ..Default::default()
        };
        SemanticViewDefinition {
            tables: vec![t("orders", &["order_id"]), t("promotions", &[])],
            joins: vec![Join {
                name: Some("orders_to_promotions".into()),
                from_alias: "orders".into(),
                table: "promotions".into(),
                fk_columns: vec!["promo_code".into()],
                ref_columns: vec!["promo_code".into()],
                cardinality: Cardinality::ManyToMany,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_view_without_one_is_borrowed() {
        let mut d = def();
        d.joins[0].cardinality = Cardinality::ManyToOne;
        assert!(matches!(lower_many_to_many(&d), Cow::Borrowed(_)));
    }

    #[test]
    fn both_tables_reference_a_table_of_the_distinct_keys() {
        let d = def();
        let out = lower_many_to_many(&d);
        let key = &out.tables[2];
        assert_eq!(key.alias, "__sv_m2m_0");
        assert_eq!(key.pk_columns, vec!["promo_code".to_string()]);
        assert_eq!(
            key.definition.as_deref(),
            Some(
                "SELECT DISTINCT \"promotions\".\"promo_code\" FROM \"m_promotions\" AS \
                 \"promotions\" WHERE \"promotions\".\"promo_code\" IS NOT NULL"
            )
        );
        assert!(is_key_table(&out, "__sv_m2m_0"));
        assert!(!is_key_table(&out, "promotions"));
        assert_eq!(relationship_of(&out, "__sv_m2m_0"), "orders_to_promotions");
        // The relationship keeps its name, and references the key table.
        let j = &out.joins[0];
        assert_eq!(j.name.as_deref(), Some("orders_to_promotions"));
        assert_eq!(
            (j.from_alias.as_str(), j.table.as_str()),
            ("orders", "__sv_m2m_0")
        );
        assert_eq!(j.cardinality, Cardinality::ManyToOne);
        let back = &out.joins[1];
        assert_eq!(
            (back.from_alias.as_str(), back.table.as_str()),
            ("promotions", "__sv_m2m_0")
        );
        assert_eq!(back.fk_columns, vec!["promo_code".to_string()]);
        assert_eq!(
            back.name.as_deref(),
            Some("orders_to_promotions (many-to-many)")
        );
        assert_eq!(back.cardinality, Cardinality::ManyToOne);
    }

    #[test]
    fn a_path_through_the_key_table_crosses_the_relationship() {
        let d = def();
        let out = lower_many_to_many(&d);
        let path = |p: &[&str]| p.iter().map(|t| (*t).to_string()).collect::<Vec<_>>();
        assert_eq!(
            crossed(&out, &path(&["orders", "__sv_m2m_0", "promotions"])).as_deref(),
            Some("orders_to_promotions")
        );
        assert_eq!(crossed(&out, &path(&["orders", "__sv_m2m_0"])), None);
        assert_eq!(crossed(&out, &path(&["orders", "customers"])), None);
        assert!(has_key_table(&out));
        assert!(!has_key_table(&d));
    }

    #[test]
    fn a_declared_table_with_the_prefix_is_no_key_table() {
        let mut d = def();
        d.joins[0].cardinality = Cardinality::ManyToOne;
        d.tables.push(TableRef {
            alias: "__sv_m2m_x".into(),
            definition: Some("SELECT 1 AS k".into()),
            pk_columns: vec!["k".into()],
            ..Default::default()
        });
        d.joins.push(Join {
            from_alias: "orders".into(),
            table: "__sv_m2m_x".into(),
            fk_columns: vec!["k".into()],
            ref_columns: vec!["k".into()],
            name: Some(Join::default().generated_name_at(1)),
            generated_name: true,
            ..Default::default()
        });
        assert!(!is_key_table(&d, "__sv_m2m_x"));
        assert!(!has_key_table(&d));
    }

    #[test]
    fn quoted_key_columns_are_read_as_the_columns_they_name() {
        let mut d = def();
        d.joins[0].fk_columns = vec!["\"Promo Code\"".into()];
        d.joins[0].ref_columns = vec!["\"Promo Code\"".into()];
        d.table_columns
            .insert("promotions".into(), vec!["Promo Code".into()]);
        let out = lower_many_to_many(&d);
        assert_eq!(
            out.tables[2].definition.as_deref(),
            Some(
                "SELECT DISTINCT \"promotions\".\"Promo Code\" FROM \"m_promotions\" AS \
                 \"promotions\" WHERE \"promotions\".\"Promo Code\" IS NOT NULL"
            )
        );
        assert_eq!(
            out.table_columns["__sv_m2m_0"],
            vec!["Promo Code".to_string()]
        );
    }

    #[test]
    fn no_referenced_columns_is_left_many_to_one() {
        let mut d = def();
        d.joins[0].ref_columns.clear();
        let out = lower_many_to_many(&d);
        assert_eq!(out.tables.len(), 2);
        assert_eq!(out.joins[0].cardinality, Cardinality::ManyToOne);
    }

    #[test]
    fn a_taken_name_gets_an_underscore() {
        let mut d = def();
        d.tables.push(TableRef {
            alias: "__sv_m2m_0".into(),
            table: "x".into(),
            ..Default::default()
        });
        let out = lower_many_to_many(&d);
        assert_eq!(out.tables[3].alias, "__sv_m2m_0_");
        assert!(!is_key_table(&out, "__sv_m2m_0"));
    }
}
