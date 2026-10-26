//! Member-expression reference scoping (TECH-DEBT #52, PAR-3).
//!
//! Snowflake's [validation
//! rules](https://docs.snowflake.com/en/user-guide/views-semantic/validation-rules)
//! scope a member expression to its own logical table: "expressions can refer
//! to base table columns or other expressions on the same logical table", and
//! "cannot refer to base table columns from other tables". To reach another
//! table you declare a relationship, define a *fact* on the source table, and
//! refer to that fact — the form PAR-6 made work end to end.
//!
//! The rule was already the same here. What differed was **when** it was
//! enforced: Snowflake rejects at `CREATE`, while this extension accepted the
//! DDL and let the reference surface at query time as a `DuckDB` binder error
//! naming an unknown alias — an error about the generated SQL rather than about
//! the semantic-layer rule that was broken. This module closes that gap.

use std::collections::HashSet;

use crate::errors::ParseError;
use crate::expr_tokens::scan_references;
use crate::ident::normalize_ident_part;
use crate::model::SemanticViewDefinition;

/// Reject a member expression that names a raw column of another logical table.
///
/// For every fact, dimension, and metric expression, each **qualified**
/// reference chain (`x.y`, `x.y.z`) must have a qualifier that is either:
///
/// - the member's own `source_table` (or, for a member that declares none, the
///   base table — it sits at the root grain);
/// - not a declared table alias at all, in which case the chain is something
///   this layer does not model (a struct path root, a bound parameter, a name
///   that simply does not exist) and `DuckDB` is the right place for it to fail;
/// - the qualifier of a declared **fact** or **metric** of that name — the
///   legal cross-table forms, which must keep working: `c.cust_discount`
///   naming a fact on `c` (PAR-6), and a derived metric composing
///   `t1.metric_a + t2.metric_b`.
///
/// Anything else is a raw foreign column, and is rejected here with a message
/// naming the rule rather than the alias.
///
/// Deliberately conservative in one direction: a qualifier that matches no
/// declared table is left alone. Rejecting those would turn every expression
/// this layer cannot fully parse into a `CREATE` failure, and the whole point
/// of the check is to catch the case where the *semantic* model says the
/// reference is out of scope — which requires knowing the qualifier names a
/// table in the model.
pub fn validate_member_references(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    if def.tables.is_empty() {
        return Ok(());
    }

    // Membership sets: every reference chain in every member expression is
    // tested against both, so they are hashed rather than scanned.
    let base_alias = def.tables.first().map(|t| normalize_ident_part(&t.alias));
    let table_aliases: HashSet<String> = def
        .tables
        .iter()
        .map(|t| normalize_ident_part(&t.alias))
        .collect();

    // Keys that a qualified chain may legitimately resolve to: every declared
    // fact and metric under its own-qualified spelling (`c.cust_discount`).
    // A bare reference needs no entry — bare chains are not checked at all.
    let mut member_keys: HashSet<String> = HashSet::new();
    for fact in &def.facts {
        if let Some(ref src) = fact.source_table {
            member_keys.insert(normalize_ident_part(&format!("{src}.{}", fact.name)));
        }
    }
    for met in &def.metrics {
        if let Some(ref src) = met.source_table {
            member_keys.insert(normalize_ident_part(&format!("{src}.{}", met.name)));
        }
    }

    // (kind, name, source_table, expression) for every member that carries one.
    let members = def
        .facts
        .iter()
        .map(|f| ("fact", &f.name, f.source_table.as_ref(), &f.expr))
        .chain(
            def.dimensions
                .iter()
                .map(|d| ("dimension", &d.name, d.source_table.as_ref(), &d.expr)),
        )
        .chain(
            def.metrics
                .iter()
                .map(|m| ("metric", &m.name, m.source_table.as_ref(), &m.expr)),
        );

    for (kind, name, source_table, expr) in members {
        let own = source_table
            .map(|s| normalize_ident_part(s))
            .or_else(|| base_alias.clone());
        for chain in scan_references(expr) {
            if chain.is_bare() {
                continue; // A bare name is a column of the member's own table or a member reference.
            }
            let qualifier = chain.first_part_key();
            if own.as_deref() == Some(qualifier.as_str()) {
                continue; // A column of the member's own table.
            }
            if !table_aliases.contains(&qualifier) {
                continue; // Not a table in this model -- not ours to judge.
            }
            if member_keys.contains(&chain.key()) {
                continue; // A declared fact or metric, own-qualified: the legal cross-table form.
            }
            return Err(ParseError::positionless(format!(
                "semantic view: {kind} '{name}' references '{raw}', a column of \
                 table '{qualifier}', but a {kind} expression may only reference \
                 columns of its own table{own_note}. To use a value from another \
                 table, define a FACT on that table and reference the fact by \
                 name (e.g. '{qualifier}.<fact_name>').",
                raw = chain.raw.trim(),
                own_note = own
                    .as_deref()
                    .map_or_else(String::new, |o| format!(" ('{o}')")),
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccessModifier, Cardinality, Dimension, Fact, Join, Metric, TableRef};

    fn table(alias: &str) -> TableRef {
        TableRef {
            alias: alias.to_string(),
            table: alias.to_string(),
            pk_columns: vec!["id".to_string()],
            unique_constraints: vec![],
            comment: None,
            synonyms: vec![],
            tags: Vec::new(),
        }
    }

    fn fact(name: &str, expr: &str, src: Option<&str>) -> Fact {
        Fact {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: src.map(str::to_string),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
        }
    }

    fn dim(name: &str, expr: &str, src: Option<&str>) -> Dimension {
        Dimension {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: src.map(str::to_string),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            tags: Vec::new(),
        }
    }

    fn metric(name: &str, expr: &str, src: Option<&str>) -> Metric {
        Metric {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: src.map(str::to_string),
            output_type: None,
            using_relationships: vec![],
            comment: None,
            synonyms: vec![],
            access: AccessModifier::Public,
            non_additive_by: vec![],
            window_spec: None,
            tags: Vec::new(),
        }
    }

    fn base_def() -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![table("o"), table("c")],
            joins: vec![Join {
                from_alias: "o".to_string(),
                table: "c".to_string(),
                fk_columns: vec!["customer_id".to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some("o_to_c".to_string()),
                cardinality: Cardinality::ManyToOne,
            }],
            ..SemanticViewDefinition::default()
        }
    }

    /// PAR-3 / #52: the case the whole check exists for.
    #[test]
    fn rejects_a_raw_column_of_another_table() {
        let mut def = base_def();
        def.metrics = vec![metric("margin", "SUM(o.amount - c.discount)", Some("o"))];
        let err = validate_member_references(&def).unwrap_err().message;
        assert!(
            err.contains("c.discount") && err.contains("its own table"),
            "must name the offending reference and the rule: {err}"
        );
        assert!(
            err.contains("define a FACT"),
            "must point at the supported cross-table form: {err}"
        );
    }

    /// The legal cross-table form PAR-6 implemented must not be caught by it.
    #[test]
    fn accepts_a_cross_table_fact_reference() {
        let mut def = base_def();
        def.facts = vec![fact("cust_discount", "c.discount", Some("c"))];
        def.metrics = vec![metric(
            "margin",
            "SUM(o.amount - c.cust_discount)",
            Some("o"),
        )];
        assert!(validate_member_references(&def).is_ok());
    }

    /// A derived metric composing metrics that live on other tables is legal in
    /// both systems, and is computed per grain.
    #[test]
    fn accepts_a_derived_metric_over_metrics_on_other_tables() {
        let mut def = base_def();
        def.metrics = vec![
            metric("order_total", "SUM(o.amount)", Some("o")),
            metric("cust_total", "SUM(c.balance)", Some("c")),
            metric("ratio", "o.order_total / c.cust_total", None),
        ];
        assert!(validate_member_references(&def).is_ok());
    }

    /// TECH-DEBT #54's form: a dimension referencing a fact on its own table.
    #[test]
    fn accepts_a_same_table_fact_reference_in_a_dimension() {
        let mut def = base_def();
        def.facts = vec![fact("net_line", "o.amount - o.discount", Some("o"))];
        def.dimensions = vec![dim(
            "band",
            "CASE WHEN o.net_line > 0 THEN 1 END",
            Some("o"),
        )];
        assert!(validate_member_references(&def).is_ok());
    }

    /// A member with no `source_table` sits at the root grain, so the base
    /// table is its own table.
    #[test]
    fn accepts_a_base_table_column_in_a_source_less_member() {
        let mut def = base_def();
        def.metrics = vec![metric("cnt", "COUNT(o.id)", None)];
        assert!(validate_member_references(&def).is_ok());
    }

    /// A qualifier that names no declared table is not this layer's business:
    /// a struct path, a bound parameter, or simply a typo that `DuckDB` will
    /// report. Rejecting these would make the check fire on expressions the
    /// semantic model has no opinion about.
    #[test]
    fn ignores_a_qualifier_that_is_not_a_declared_table() {
        let mut def = base_def();
        def.dimensions = vec![dim("city", "o.address.city", Some("o"))];
        def.metrics = vec![metric("weird", "SUM(unknown_thing.col)", Some("o"))];
        assert!(validate_member_references(&def).is_ok());
    }

    /// Function calls are not references (`scan_references` excludes call
    /// heads), so a schema-qualified UDF must not be read as a foreign column.
    #[test]
    fn ignores_a_qualified_function_call() {
        let mut def = base_def();
        def.metrics = vec![metric("m", "SUM(c.udf(o.amount))", Some("o"))];
        assert!(
            validate_member_references(&def).is_ok(),
            "a call head is not a column reference"
        );
    }

    /// String literals are skipped by the tokenizer, so a foreign-looking name
    /// inside quotes is text, not a reference.
    #[test]
    fn ignores_a_foreign_name_inside_a_string_literal() {
        let mut def = base_def();
        def.dimensions = vec![dim("label", "'c.discount is not a ref'", Some("o"))];
        assert!(validate_member_references(&def).is_ok());
    }

    /// The check covers dimensions and facts, not only metrics.
    #[test]
    fn rejects_a_raw_foreign_column_in_a_dimension_and_in_a_fact() {
        let mut def = base_def();
        def.dimensions = vec![dim("d", "c.region", Some("o"))];
        assert!(validate_member_references(&def).is_err(), "dimension");

        let mut def2 = base_def();
        def2.facts = vec![fact("f", "o.amount * c.rate", Some("o"))];
        assert!(validate_member_references(&def2).is_err(), "fact");
    }

    /// Case and quoting follow `DuckDB`'s rule (CLAUDE.md): the qualifier matches
    /// its table however it is spelled, so a quoted or mixed-case reference is
    /// judged the same as a bare one.
    #[test]
    fn matches_the_qualifier_case_insensitively_and_through_quotes() {
        let mut def = base_def();
        def.metrics = vec![metric("m", "SUM(\"C\".discount)", Some("o"))];
        assert!(
            validate_member_references(&def).is_err(),
            "a quoted foreign qualifier is still foreign"
        );

        let mut def2 = base_def();
        def2.metrics = vec![metric("m", "SUM(\"O\".amount)", Some("o"))];
        assert!(
            validate_member_references(&def2).is_ok(),
            "a quoted own-table qualifier is still the member's own table"
        );
    }
}
