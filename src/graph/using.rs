//! USING relationship validation (Phase 32).
//!
//! Validates that all `using_relationships` references on metrics are valid:
//! derived metrics must not have USING, each relationship must exist, and
//! each relationship must originate from the metric's source table.

use crate::errors::ParseError;
use crate::model::SemanticViewDefinition;

/// Validate that all `using_relationships` references on metrics are valid.
///
/// For each metric with non-empty `using_relationships`:
/// 1. Derived metrics (`source_table` is None) must not have USING.
/// 2. Each referenced relationship name must exist in `def.joins`.
/// 3. Each referenced relationship must originate from the metric's `source_table`.
///
/// Returns `Ok(())` if all references are valid, `Err` with descriptive message otherwise.
pub fn validate_using_relationships(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    // Names resolve with `graph::find_relationship` (spelling first, then
    // DuckDB's identifier rule), the lookup the join resolver, the role-playing
    // fences, the fact rewrite and the per-grain path share.
    let available_names: Vec<&str> = def
        .joins
        .iter()
        .filter(|j| !j.generated_name)
        .filter_map(|j| j.name.as_deref())
        .collect();

    for metric in &def.metrics {
        if metric.using_relationships.is_empty() {
            continue;
        }

        // Check 1: derived metrics must not have USING
        if metric.source_table.is_none() {
            return Err(ParseError::positionless(format!(
                "USING clause not allowed on derived metric '{}'",
                metric.name
            )));
        }

        let metric_source = metric.source_table.as_ref().unwrap().to_ascii_lowercase();

        for rel_name in &metric.using_relationships {
            // Check 2: relationship must exist
            let found = super::find_relationship(&def.joins, rel_name);

            match found {
                None => {
                    return Err(ParseError::positionless(format!(
                        "unknown relationship '{rel_name}' in USING clause of metric '{}'. \
                         Available: [{}]",
                        metric.name,
                        available_names.join(", ")
                    )));
                }
                // A generated name depends on the relationship's position, so
                // a USING naming it would break when one is declared before it.
                Some(join) if join.generated_name => {
                    return Err(ParseError::positionless(format!(
                        "relationship '{rel_name}' in USING clause of metric '{}' was \
                         declared without a name, and its generated name can change; name \
                         it ('rel_name AS {}(...) REFERENCES {}') and USING that name",
                        metric.name, join.from_alias, join.table
                    )));
                }
                Some(join) => {
                    // Check 3: relationship must originate from metric's source_table
                    let from_lower = join.from_alias.to_ascii_lowercase();
                    if from_lower != metric_source {
                        return Err(ParseError::positionless(format!(
                            "relationship '{rel_name}' does not originate from table '{}' \
                             (metric '{}')",
                            metric.source_table.as_ref().unwrap(),
                            metric.name
                        )));
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::graph::validate_using_relationships;

    use super::super::test_helpers::*;

    #[test]
    fn validate_using_valid_reference() {
        // USING references existing named relationship -> Ok
        let def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![
                (Some("dep_airport"), "f", "a", vec!["dep_id"]),
                (Some("arr_airport"), "f", "a", vec!["arr_id"]),
            ],
            vec![("departure_count", Some("f"), vec!["dep_airport"])],
        );
        assert!(
            validate_using_relationships(&def).is_ok(),
            "Valid USING reference should be accepted"
        );
    }

    /// A relationship declared without a name cannot be named by `USING`:
    /// its generated name depends on its position, so it would change when
    /// a relationship is declared before it (PR #358 review).
    #[test]
    fn validate_using_generated_name_rejected() {
        let mut def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![(None, "f", "a", vec!["dep_id"])],
            vec![("departure_count", Some("f"), vec![])],
        );
        let generated = def.joins[0].generated_name_at(0);
        def.joins[0].name = Some(generated.clone());
        def.joins[0].generated_name = true;
        def.metrics[0].using_relationships = vec![generated];
        let err = validate_using_relationships(&def).unwrap_err().message;
        assert!(err.contains("declared without a name"), "{err}");
    }

    #[test]
    fn validate_using_unknown_relationship_rejected() {
        // USING references non-existent relationship -> Err with suggestion
        let def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![(Some("dep_airport"), "f", "a", vec!["dep_id"])],
            vec![("departure_count", Some("f"), vec!["nonexistent"])],
        );
        let err = validate_using_relationships(&def).unwrap_err().message;
        assert!(
            err.contains("unknown relationship") && err.contains("nonexistent"),
            "Expected unknown relationship error, got: {err}"
        );
        assert!(
            err.contains("dep_airport"),
            "Error should list available relationships: {err}"
        );
    }

    #[test]
    fn validate_using_wrong_source_table_rejected() {
        // USING references relationship from wrong source table -> Err
        let def = make_def_with_named_joins(
            vec![
                ("f", "flights", vec!["id"]),
                ("a", "airports", vec!["id"]),
                ("p", "passengers", vec!["id"]),
            ],
            vec![
                (Some("dep_airport"), "f", "a", vec!["dep_id"]),
                (Some("pax_to_flight"), "p", "f", vec!["flight_id"]),
            ],
            // Metric is on "p" but references "dep_airport" which originates from "f"
            vec![("pax_count", Some("p"), vec!["dep_airport"])],
        );
        let err = validate_using_relationships(&def).unwrap_err().message;
        assert!(
            err.contains("does not originate"),
            "Expected wrong source table error, got: {err}"
        );
    }

    #[test]
    fn validate_using_derived_metric_rejected() {
        // USING on derived metric (source_table is None) -> Err
        let def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![(Some("dep_airport"), "f", "a", vec!["dep_id"])],
            vec![("derived_met", None, vec!["dep_airport"])],
        );
        let err = validate_using_relationships(&def).unwrap_err().message;
        assert!(
            err.contains("derived metric") && err.contains("USING"),
            "Expected USING on derived metric error, got: {err}"
        );
    }

    // -------------------------------------------------------------------
    // Relationship names follow DuckDB identifier rules (CLAUDE.md;
    // `ident::ident_matches`, TECH-DEBT #28): quoting never changes which
    // relationship a name denotes, and case folds for ASCII letters only.
    // -------------------------------------------------------------------

    /// A relationship declared QUOTED (`"Dep_Airport"`, stored with its
    /// quotes) is named by the bare `dep_airport` — DuckDB matches a quoted
    /// identifier case-insensitively, exactly as it does a bare one. An
    /// ASCII-lowercase comparison kept the quote characters and rejected it as
    /// an unknown relationship.
    #[test]
    fn using_names_a_quoted_relationship_by_its_bare_spelling() {
        let def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![
                (Some("\"Dep_Airport\""), "f", "a", vec!["dep_id"]),
                (Some("arr_airport"), "f", "a", vec!["arr_id"]),
            ],
            vec![("departure_count", Some("f"), vec!["dep_airport"])],
        );
        assert!(
            validate_using_relationships(&def).is_ok(),
            "{:?}",
            validate_using_relationships(&def).err().map(|e| e.message)
        );
    }

    /// The converse: a bare declaration named by a quoted, differently-cased
    /// `USING` entry.
    #[test]
    fn using_names_a_bare_relationship_by_a_quoted_spelling() {
        let def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![
                (Some("dep_airport"), "f", "a", vec!["dep_id"]),
                (Some("arr_airport"), "f", "a", vec!["arr_id"]),
            ],
            vec![("departure_count", Some("f"), vec!["\"DEP_AIRPORT\""])],
        );
        assert!(
            validate_using_relationships(&def).is_ok(),
            "{:?}",
            validate_using_relationships(&def).err().map(|e| e.message)
        );
    }

    /// Control, pinning DuckDB parity rather than a fix: DuckDB folds case for
    /// ASCII letters only (`CREATE TABLE "ÉTAPE"` is not found as `étape`,
    /// checked against DuckDB 1.5.6), so neither does relationship matching.
    #[test]
    fn using_does_not_fold_non_ascii_case() {
        let def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![(Some("ÉTAPE"), "f", "a", vec!["dep_id"])],
            vec![("departure_count", Some("f"), vec!["étape"])],
        );
        let err = validate_using_relationships(&def).unwrap_err().message;
        assert!(err.contains("unknown relationship 'étape'"), "{err}");
        // ...while the ASCII letters of the same name still fold.
        let def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![(Some("ÉTAPE"), "f", "a", vec!["dep_id"])],
            vec![("departure_count", Some("f"), vec!["Étape"])],
        );
        assert!(validate_using_relationships(&def).is_ok());
    }

    // -------------------------------------------------------------------
    // PR #249 review: legacy stored names and dot-qualified names.
    // -------------------------------------------------------------------

    /// A stored pair named `dep` and `"dep"` (accepted before #249) is resolved
    /// by spelling first: `USING ("dep")` names the quoted relationship, which
    /// originates from the metric's table. Taking the first identifier match
    /// picked `dep` (from `c`) and rejected the metric.
    #[test]
    fn using_prefers_the_relationship_spelled_as_written() {
        let def = make_def_with_named_joins(
            vec![
                ("f", "flights", vec!["id"]),
                ("c", "carriers", vec!["id"]),
                ("a", "airports", vec!["id"]),
            ],
            vec![
                (Some("dep"), "c", "a", vec!["hub_id"]),
                (Some("\"dep\""), "f", "a", vec!["dep_id"]),
                (Some("f_to_c"), "f", "c", vec!["carrier_id"]),
            ],
            vec![("departure_count", Some("f"), vec!["\"dep\""])],
        );
        assert!(
            validate_using_relationships(&def).is_ok(),
            "{:?}",
            validate_using_relationships(&def).err().map(|e| e.message)
        );
    }

    /// `"x.y"` is one identifier containing a dot; `x.y` and `"x"."y"` are the
    /// two-part identifier. `USING ("x"."y")` names `x.y` (from `f`), not the
    /// first-declared `"x.y"` (from `c`), although both join to the string
    /// `x.y` once normalized.
    #[test]
    fn using_keeps_identifier_part_boundaries() {
        let def = make_def_with_named_joins(
            vec![
                ("f", "flights", vec!["id"]),
                ("c", "carriers", vec!["id"]),
                ("a", "airports", vec!["id"]),
            ],
            vec![
                (Some("\"x.y\""), "c", "a", vec!["hub_id"]),
                (Some("x.y"), "f", "a", vec!["dep_id"]),
                (Some("f_to_c"), "f", "c", vec!["carrier_id"]),
            ],
            vec![("departure_count", Some("f"), vec!["\"x\".\"y\""])],
        );
        assert!(
            validate_using_relationships(&def).is_ok(),
            "{:?}",
            validate_using_relationships(&def).err().map(|e| e.message)
        );
    }

    /// A one-part `"x.y"` does not name the two-part `x.y`: DuckDB resolves
    /// them as different identifiers, so the `USING` names no relationship.
    #[test]
    fn using_a_dotted_one_part_name_does_not_name_a_two_part_relationship() {
        let def = make_def_with_named_joins(
            vec![("f", "flights", vec!["id"]), ("a", "airports", vec!["id"])],
            vec![(Some("x.y"), "f", "a", vec!["dep_id"])],
            vec![("departure_count", Some("f"), vec!["\"x.y\""])],
        );
        let err = validate_using_relationships(&def).unwrap_err().message;
        assert!(err.contains("unknown relationship '\"x.y\"'"), "{err}");
    }
}
