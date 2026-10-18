//! YAML export: renders a [`SemanticViewDefinition`] as a YAML string
//! suitable for round-trip through `CREATE SEMANTIC VIEW ... FROM YAML $$ ... $$`.
//!
//! Internal fields populated at define time (column types, timestamps,
//! database/schema context) are stripped before serialization. The render
//! logic lives here (always compiled, unit-tested under `cargo test`).
//! The extension-only `VScalar` wrapper lives in [`crate::ddl::read_yaml`].

use crate::model::SemanticViewDefinition;

/// Export a semantic view definition as a YAML string.
///
/// Clones the definition and strips internal runtime fields that are
/// repopulated at define time:
/// - `created_on` (DDL-time timestamp)
/// - `database_name` / `schema_name` / `resolution_schema_name` /
///   `temp_tables` (connection context: where the view lives and where its
///   body resolved)
///
/// After stripping, `serde(skip_serializing_if)` on these fields ensures
/// they are omitted from the YAML output entirely. (`schema_version` lives
/// only in the stored JSON, not on this struct, so it is never rendered
/// here — see `model::CURRENT_SCHEMA_VERSION`.)
pub fn render_yaml_export(def: &SemanticViewDefinition) -> Result<String, String> {
    let mut export = def.clone();
    export.created_on = None;
    export.database_name = None;
    export.schema_name = None;
    export.resolution_schema_name = None;
    export.temp_tables = Vec::new();

    yaml_serde::to_string(&export).map_err(|e| format!("YAML serialization error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AccessModifier, Dimension, Fact, Join, Materialization, Metric, NonAdditiveDim, NullsOrder,
        SortOrder, TableRef, WindowOrderBy, WindowSpec,
    };

    /// Helper: build a minimal definition with internal fields populated.
    fn def_with_internals() -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            }],
            dimensions: vec![Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            }],
            metrics: vec![Metric {
                name: "revenue".to_string(),
                expr: "SUM(o.amount)".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            }],
            // Internal fields -- should be stripped
            created_on: Some("2026-04-20T12:00:00Z".to_string()),
            database_name: Some("mydb".to_string()),
            schema_name: Some("main".to_string()),
            resolution_schema_name: Some("main".to_string()),
            temp_tables: vec!["orders".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn strips_temp_tables() {
        let yaml = render_yaml_export(&def_with_internals()).unwrap();
        assert!(
            !yaml.contains("temp_tables"),
            "temp_tables should be stripped from YAML: {yaml}"
        );
    }

    #[test]
    fn strips_created_on() {
        let yaml = render_yaml_export(&def_with_internals()).unwrap();
        assert!(
            !yaml.contains("created_on"),
            "created_on should be stripped from YAML: {yaml}"
        );
    }

    #[test]
    fn strips_database_name() {
        let yaml = render_yaml_export(&def_with_internals()).unwrap();
        assert!(
            !yaml.contains("database_name"),
            "database_name should be stripped from YAML: {yaml}"
        );
    }

    #[test]
    fn strips_schema_name() {
        let yaml = render_yaml_export(&def_with_internals()).unwrap();
        // Substring, so this covers `resolution_schema_name` too — both are
        // connection context repopulated at define time, and an exported YAML
        // re-imported elsewhere must not carry either.
        assert!(
            !yaml.contains("schema_name"),
            "schema_name should be stripped from YAML: {yaml}"
        );
    }

    #[test]
    fn preserves_user_facing_fields() {
        let mut def = def_with_internals();
        def.joins = vec![Join {
            table: "c".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            ..Default::default()
        }];
        def.facts = vec![Fact {
            name: "unit_price".to_string(),
            expr: "o.price / o.qty".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }];
        def.comment = Some("Revenue analytics".to_string());

        let yaml = render_yaml_export(&def).unwrap();
        assert!(yaml.contains("tables:"), "tables missing from YAML: {yaml}");
        assert!(
            yaml.contains("dimensions:"),
            "dimensions missing from YAML: {yaml}"
        );
        assert!(
            yaml.contains("metrics:"),
            "metrics missing from YAML: {yaml}"
        );
        assert!(yaml.contains("joins:"), "joins missing from YAML: {yaml}");
        assert!(yaml.contains("facts:"), "facts missing from YAML: {yaml}");
        assert!(
            yaml.contains("comment:"),
            "comment missing from YAML: {yaml}"
        );
        assert!(
            yaml.contains("Revenue analytics"),
            "comment value missing from YAML: {yaml}"
        );
    }

    #[test]
    fn roundtrip_export_reimport_equal() {
        let def = def_with_internals();
        let yaml = render_yaml_export(&def).unwrap();
        let reimported =
            SemanticViewDefinition::from_yaml("roundtrip", &yaml).expect("reimport should succeed");

        // Build expected: original with internal fields zeroed
        let mut expected = def;
        expected.created_on = None;
        expected.database_name = None;
        expected.schema_name = None;
        expected.resolution_schema_name = None;
        expected.temp_tables = Vec::new();

        assert_eq!(expected, reimported);
    }

    #[test]
    fn handles_empty_definition() {
        let def = SemanticViewDefinition::default();
        let yaml = render_yaml_export(&def).unwrap();
        // Should serialize without error; dims/metrics will be empty lists
        assert!(!yaml.contains("column_type_names"));
        assert!(!yaml.contains("column_types_inferred"));
        assert!(!yaml.contains("created_on"));
        assert!(!yaml.contains("database_name"));
        assert!(!yaml.contains("schema_name"));
    }

    #[test]
    fn handles_definition_with_materializations() {
        let mut def = def_with_internals();
        def.materializations = vec![Materialization {
            name: "daily_rev".to_string(),
            table: "daily_revenue_agg".to_string(),
            dimensions: vec!["region".to_string()],
            metrics: vec!["revenue".to_string()],
        }];

        let yaml = render_yaml_export(&def).unwrap();
        assert!(
            yaml.contains("materializations:"),
            "materializations missing from YAML: {yaml}"
        );
        assert!(
            yaml.contains("daily_rev"),
            "materialization name missing: {yaml}"
        );
        assert!(
            yaml.contains("daily_revenue_agg"),
            "materialization table missing: {yaml}"
        );

        // Round-trip check
        let reimported = SemanticViewDefinition::from_yaml("mat_roundtrip", &yaml).unwrap();
        assert_eq!(reimported.materializations.len(), 1);
        assert_eq!(reimported.materializations[0].name, "daily_rev");
    }

    #[test]
    fn handles_all_metadata_annotations() {
        let def = SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                comment: Some("Main orders table".to_string()),
                synonyms: vec!["order_facts".to_string()],
                ..Default::default()
            }],
            dimensions: vec![Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                comment: Some("Geographic region".to_string()),
                synonyms: vec!["area".to_string(), "territory".to_string()],
                is_filter: false,
                ..Default::default()
            }],
            metrics: vec![Metric {
                name: "internal_rev".to_string(),
                expr: "SUM(o.amount)".to_string(),
                source_table: Some("o".to_string()),
                access: AccessModifier::Private,
                comment: Some("Internal revenue metric".to_string()),
                synonyms: vec!["rev".to_string()],
                ..Default::default()
            }],
            facts: vec![Fact {
                name: "unit_price".to_string(),
                expr: "o.price / o.qty".to_string(),
                source_table: Some("o".to_string()),
                access: AccessModifier::Private,
                comment: Some("Price per unit".to_string()),
                synonyms: vec!["price_per_item".to_string()],
                is_filter: false,
                ..Default::default()
            }],
            comment: Some("Revenue analytics view".to_string()),
            ..Default::default()
        };

        let yaml = render_yaml_export(&def).unwrap();
        // Check metadata present
        assert!(yaml.contains("Private"), "access Private missing: {yaml}");
        assert!(
            yaml.contains("Geographic region"),
            "dim comment missing: {yaml}"
        );
        assert!(yaml.contains("area"), "dim synonym missing: {yaml}");
        assert!(yaml.contains("territory"), "dim synonym missing: {yaml}");
        assert!(
            yaml.contains("Revenue analytics view"),
            "view comment missing: {yaml}"
        );
        assert!(
            yaml.contains("Internal revenue metric"),
            "metric comment missing: {yaml}"
        );
        assert!(
            yaml.contains("Price per unit"),
            "fact comment missing: {yaml}"
        );

        // Round-trip
        let reimported = SemanticViewDefinition::from_yaml("meta_roundtrip", &yaml).unwrap();
        assert_eq!(reimported.metrics[0].access, AccessModifier::Private);
        assert_eq!(
            reimported.dimensions[0].comment.as_deref(),
            Some("Geographic region")
        );
        assert_eq!(reimported.dimensions[0].synonyms, vec!["area", "territory"]);
    }

    #[test]
    fn handles_semi_additive_and_window_metrics() {
        let def = SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            }],
            dimensions: vec![Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            }],
            metrics: vec![
                Metric {
                    name: "balance".to_string(),
                    expr: "SUM(o.amount)".to_string(),
                    source_table: Some("o".to_string()),
                    non_additive_by: vec![NonAdditiveDim {
                        dimension: "date_dim".to_string(),
                        order: SortOrder::Desc,
                        nulls: NullsOrder::First,
                    }],
                    ..Default::default()
                },
                Metric {
                    name: "avg_qty_7d".to_string(),
                    expr: "AVG(total_qty) OVER (...)".to_string(),
                    window_spec: Some(WindowSpec {
                        window_function: "AVG".to_string(),
                        inner_metric: "total_qty".to_string(),
                        excluding_dims: vec!["region".to_string()],
                        order_by: vec![WindowOrderBy {
                            expr: "date_dim".to_string(),
                            order: SortOrder::Asc,
                            nulls: NullsOrder::Last,
                        }],
                        frame_clause: Some(
                            "RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW".to_string(),
                        ),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let yaml = render_yaml_export(&def).unwrap();
        assert!(
            yaml.contains("non_additive_by"),
            "non_additive_by missing: {yaml}"
        );
        assert!(yaml.contains("window_spec"), "window_spec missing: {yaml}");

        // Round-trip
        let reimported = SemanticViewDefinition::from_yaml("advanced_roundtrip", &yaml).unwrap();
        assert_eq!(reimported.metrics[0].non_additive_by.len(), 1);
        assert_eq!(
            reimported.metrics[0].non_additive_by[0].dimension,
            "date_dim"
        );
        assert!(reimported.metrics[1].window_spec.is_some());
        let ws = reimported.metrics[1].window_spec.as_ref().unwrap();
        assert_eq!(ws.window_function, "AVG");
        assert_eq!(ws.inner_metric, "total_qty");
    }

    /// `is_filter` survives the YAML round-trip on both a dimension and a fact.
    ///
    /// The export is `def.clone()` through serde, so the `#[serde]` derive
    /// already carries the field — this guards the derive rather than adding
    /// behaviour. It would fail if the attribute were dropped or renamed, or if
    /// `render_yaml_export` grew a strip step that reset the flag.
    #[test]
    fn is_filter_survives_yaml_round_trip() {
        let def = SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            }],
            dimensions: vec![
                Dimension {
                    name: "is_domestic".to_string(),
                    expr: "o.country = 'US'".to_string(),
                    source_table: Some("o".to_string()),
                    is_filter: true,
                    ..Default::default()
                },
                Dimension {
                    name: "region".to_string(),
                    expr: "o.region".to_string(),
                    source_table: Some("o".to_string()),
                    ..Default::default()
                },
            ],
            facts: vec![Fact {
                name: "is_large".to_string(),
                expr: "o.amount > 100".to_string(),
                source_table: Some("o".to_string()),
                is_filter: true,
                ..Default::default()
            }],
            ..Default::default()
        };

        let yaml = render_yaml_export(&def).unwrap();
        assert!(yaml.contains("is_filter"), "is_filter missing: {yaml}");

        let rt = SemanticViewDefinition::from_yaml("filters_roundtrip", &yaml).unwrap();
        assert!(rt.dimensions[0].is_filter, "filter dimension lost its flag");
        assert!(
            !rt.dimensions[1].is_filter,
            "plain dimension must not gain the flag"
        );
        assert!(rt.facts[0].is_filter, "filter fact lost its flag");
    }

    /// A definition with no filters must not emit `is_filter` at all, so YAML
    /// exported from pre-existing views stays byte-identical
    /// (`skip_serializing_if` on the `false` default).
    #[test]
    fn no_filters_emits_no_is_filter_key() {
        let yaml = render_yaml_export(&def_with_internals()).unwrap();
        assert!(
            !yaml.contains("is_filter"),
            "is_filter must be omitted when false: {yaml}"
        );
    }
}
