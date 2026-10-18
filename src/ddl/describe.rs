use std::collections::HashMap;

use crate::catalog::CatalogReader;
use crate::model::{AccessModifier, SemanticViewDefinition};
use crate::query::member_types::MemberTypes;

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 Task 3 (Wave 2) — sv_describe_semantic_view_bind_rust
// ---------------------------------------------------------------------------
// FFI dispatcher for the migrated describe_semantic_view(view_name) TF.
// 5-column VARCHAR (object_kind, object_name, parent_entity, property,
// property_value). Same bridge + borrow contract as Wave 0 spike.

/// # Safety
///
/// `conn` is a borrowed handle; `name_ptr` must point to `name_len` UTF-8 bytes.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_describe_semantic_view_bind_rust(
    conn: libduckdb_sys::duckdb_connection,
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    crate::ddl::read_ffi::run_dispatcher(
        conn,
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_describe_semantic_view_bind_rust",
        |borrowed| unsafe { describe_view_rows(borrowed, name_ptr, name_len, sp_ptr, sp_len) },
    )
}

/// Body for [`sv_describe_semantic_view_bind_rust`]: resolve the view and
/// serialize its DESCRIBE property rows over the shared varchar wire format.
///
/// # Safety
///
/// `name_ptr` must be null or point to `name_len` readable bytes.
#[cfg(feature = "extension")]
unsafe fn describe_view_rows(
    borrowed: &crate::ddl::read_ffi::BorrowedConnection,
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
) -> Result<Vec<u8>, String> {
    use crate::ddl::read_ffi::{probe_catalog_table_present, read_str_arg, serialize_varchar_rows};

    let raw_name = read_str_arg(name_ptr, name_len, "view name")?;
    // FF-4: normalize so quoted-identifier inputs resolve like `semantic_view()`.
    let view = crate::ident::parse_view_ref(&raw_name)
        .map_err(|e| format!("Invalid view name '{raw_name}': {e}"))?;
    let name = view.name.clone();
    let search_path = unsafe { crate::ddl::read_ffi::read_search_path(sp_ptr, sp_len) }?;
    // FF-9: a probe-query failure is distinct from "no views" (propagated).
    let present = probe_catalog_table_present(borrowed)?;
    let reader = CatalogReader::new(borrowed, present);
    let json = reader
        .lookup(&view, &search_path)?
        .ok_or_else(|| crate::catalog::view_not_found_msg(&view.to_string()))?;
    let def = SemanticViewDefinition::from_json(&name, &json)?;
    let alias_map = def.alias_to_table_map();
    let base_table = def.base_table().to_string();
    // TECH-DEBT #51: DATA_TYPE rows report each member's type as probed now,
    // on this bind's connection (see `query::member_types`).
    let types = crate::ddl::read_ffi::member_types_for_bind(
        borrowed,
        &name,
        &def,
        crate::query::member_types::Wanted::ALL,
    )?;

    let mut internal: Vec<DescribeRow> = Vec::new();
    if let Some(ref comment) = def.comment {
        internal.push(DescribeRow {
            object_kind: String::new(),
            object_name: String::new(),
            parent_entity: String::new(),
            property: "COMMENT".to_string(),
            property_value: comment.clone(),
        });
    }
    collect_table_rows(&def, &mut internal);
    collect_relationship_rows(&def, &alias_map, &mut internal);
    collect_fact_rows(&def, &base_table, &alias_map, &types, &mut internal);
    collect_dimension_rows(&def, &base_table, &alias_map, &types, &mut internal);
    collect_metric_rows(&def, &base_table, &alias_map, &types, &mut internal);
    collect_materialization_rows(&def, &mut internal);

    let rows: Vec<Vec<String>> = internal
        .into_iter()
        .map(|r| {
            vec![
                r.object_kind,
                r.object_name,
                r.parent_entity,
                r.property,
                r.property_value,
            ]
        })
        .collect();
    serialize_varchar_rows(&rows)
}

/// A single property row in the DESCRIBE output.
///
/// Each row represents one property of one object in the semantic view.
/// Output schema: `(object_kind, object_name, parent_entity, property, property_value)`.
struct DescribeRow {
    object_kind: String,
    object_name: String,
    parent_entity: String,
    property: String,
    property_value: String,
}

// Phase 65 Plan 05 Batch 3: legacy `DescribeBindData` + `DescribeInitData`
// retired with the H2 query_conn allocation. `DescribeRow` remains because
// `sv_describe_semantic_view_bind_rust` (above) still calls the
// `collect_*` helpers in this file to assemble property rows for the
// C++ Catalog API path's wire format.

/// Format column names as a JSON array: `["col1","col2"]`.
/// Matches Snowflake format: no spaces after commas.
///
/// Delegates to `serde_json` so a value containing `"`, `\`, or a control
/// character is escaped into valid JSON instead of breaking out of the
/// string literal (C-12, code-review 2026-07-11) — the previous
/// `format!("\"{s}\"")` emitted `["a"b"]` for a synonym `a"b`, which the
/// DESCRIBE output documents as a JSON array. `serde_json` writes no space
/// after the comma and passes UTF-8 through unescaped, so all-ASCII inputs
/// stay byte-identical to the prior hand-rolled format.
pub(crate) fn format_json_array(items: &[String]) -> String {
    // Serializing a slice of `String` to JSON is infallible (no floats, no
    // non-string map keys, in-memory writer), so `expect` documents an
    // impossibility rather than swallowing a real error.
    serde_json::to_string(items).expect("serializing a &[String] to JSON is infallible")
}

/// Collect TABLE property rows from the definition.
///
/// Each table alias emits: `BASE_TABLE_DATABASE_NAME`, `BASE_TABLE_SCHEMA_NAME`,
/// `BASE_TABLE_NAME`, and optionally `PRIMARY_KEY` (only when non-empty).
fn collect_table_rows(def: &SemanticViewDefinition, rows: &mut Vec<DescribeRow>) {
    let db_name = def.database_name.clone().unwrap_or_default();
    let sch_name = def.schema_name.clone().unwrap_or_default();

    for table in &def.tables {
        let obj_name = table.table.clone();
        // A table that was temporary at CREATE lives in `temp.main`.
        let (table_db, table_schema) =
            if crate::expand::temp_table_name(&table.table, def).is_some() {
                ("temp".to_string(), "main".to_string())
            } else {
                (db_name.clone(), sch_name.clone())
            };

        rows.push(DescribeRow {
            object_kind: "TABLE".to_string(),
            object_name: obj_name.clone(),
            parent_entity: String::new(),
            property: "BASE_TABLE_DATABASE_NAME".to_string(),
            property_value: table_db,
        });
        rows.push(DescribeRow {
            object_kind: "TABLE".to_string(),
            object_name: obj_name.clone(),
            parent_entity: String::new(),
            property: "BASE_TABLE_SCHEMA_NAME".to_string(),
            property_value: table_schema,
        });
        rows.push(DescribeRow {
            object_kind: "TABLE".to_string(),
            object_name: obj_name.clone(),
            parent_entity: String::new(),
            property: "BASE_TABLE_NAME".to_string(),
            property_value: table.table.clone(),
        });
        if !table.pk_columns.is_empty() {
            rows.push(DescribeRow {
                object_kind: "TABLE".to_string(),
                object_name: obj_name.clone(),
                parent_entity: String::new(),
                property: "PRIMARY_KEY".to_string(),
                property_value: format_json_array(&table.pk_columns),
            });
        }
        if let Some(ref comment) = table.comment {
            rows.push(DescribeRow {
                object_kind: "TABLE".to_string(),
                object_name: obj_name.clone(),
                parent_entity: String::new(),
                property: "COMMENT".to_string(),
                property_value: comment.clone(),
            });
        }
        if !table.synonyms.is_empty() {
            rows.push(DescribeRow {
                object_kind: "TABLE".to_string(),
                object_name: obj_name,
                parent_entity: String::new(),
                property: "SYNONYMS".to_string(),
                property_value: format_json_array(&table.synonyms),
            });
        }
    }
}

/// Collect RELATIONSHIP property rows from the definition.
///
/// Each named join emits: `TABLE`, `REF_TABLE`, `FOREIGN_KEY`, `REF_KEY`.
/// Unnamed/legacy joins are skipped.
fn collect_relationship_rows(
    def: &SemanticViewDefinition,
    alias_map: &HashMap<String, String>,
    rows: &mut Vec<DescribeRow>,
) {
    for join in &def.joins {
        let rel_name = match &join.name {
            Some(n) => n.clone(),
            None => continue, // skip unnamed legacy joins
        };
        if join.from_alias.is_empty() {
            continue; // skip legacy joins without from_alias
        }
        let from_table = alias_map
            .get(&join.from_alias)
            .cloned()
            .unwrap_or_else(|| join.from_alias.clone());
        let ref_table = alias_map
            .get(&join.table)
            .cloned()
            .unwrap_or_else(|| join.table.clone());

        rows.push(DescribeRow {
            object_kind: "RELATIONSHIP".to_string(),
            object_name: rel_name.clone(),
            parent_entity: from_table.clone(),
            property: "TABLE".to_string(),
            property_value: from_table.clone(),
        });
        rows.push(DescribeRow {
            object_kind: "RELATIONSHIP".to_string(),
            object_name: rel_name.clone(),
            parent_entity: from_table.clone(),
            property: "REF_TABLE".to_string(),
            property_value: ref_table,
        });
        rows.push(DescribeRow {
            object_kind: "RELATIONSHIP".to_string(),
            object_name: rel_name.clone(),
            parent_entity: from_table.clone(),
            property: "FOREIGN_KEY".to_string(),
            property_value: format_json_array(&join.fk_columns),
        });
        rows.push(DescribeRow {
            object_kind: "RELATIONSHIP".to_string(),
            object_name: rel_name,
            parent_entity: from_table,
            property: "REF_KEY".to_string(),
            property_value: format_json_array(&join.ref_columns),
        });
    }
}

/// Collect FACT property rows from the definition.
///
/// Each fact emits: `TABLE`, `EXPRESSION`, `DATA_TYPE`, `ACCESS_MODIFIER`, plus
/// `COMMENT` / `SYNONYMS` / `LABELS` when set. `LABELS` is `["FILTER"]` for a
/// named filter — like `ACCESS_MODIFIER`, it is surfaced here rather than as a
/// `SHOW SEMANTIC FACTS` column, which stays at its 8 Snowflake-aligned columns.
fn collect_fact_rows(
    def: &SemanticViewDefinition,
    base_table: &str,
    alias_map: &HashMap<String, String>,
    types: &MemberTypes,
    rows: &mut Vec<DescribeRow>,
) {
    for fact in &def.facts {
        let parent = fact
            .source_table
            .as_ref()
            .and_then(|a| alias_map.get(a).cloned())
            .unwrap_or_else(|| base_table.to_string());

        rows.push(DescribeRow {
            object_kind: "FACT".to_string(),
            object_name: fact.name.clone(),
            parent_entity: parent.clone(),
            property: "TABLE".to_string(),
            property_value: parent.clone(),
        });
        rows.push(DescribeRow {
            object_kind: "FACT".to_string(),
            object_name: fact.name.clone(),
            parent_entity: parent.clone(),
            property: "EXPRESSION".to_string(),
            property_value: fact.expr.clone(),
        });
        rows.push(DescribeRow {
            object_kind: "FACT".to_string(),
            object_name: fact.name.clone(),
            parent_entity: parent.clone(),
            property: "DATA_TYPE".to_string(),
            property_value: types.fact(&fact.name, fact.output_type.as_ref()),
        });
        if let Some(ref comment) = fact.comment {
            rows.push(DescribeRow {
                object_kind: "FACT".to_string(),
                object_name: fact.name.clone(),
                parent_entity: parent.clone(),
                property: "COMMENT".to_string(),
                property_value: comment.clone(),
            });
        }
        if !fact.synonyms.is_empty() {
            rows.push(DescribeRow {
                object_kind: "FACT".to_string(),
                object_name: fact.name.clone(),
                parent_entity: parent.clone(),
                property: "SYNONYMS".to_string(),
                property_value: format_json_array(&fact.synonyms),
            });
        }
        if fact.is_filter {
            rows.push(DescribeRow {
                object_kind: "FACT".to_string(),
                object_name: fact.name.clone(),
                parent_entity: parent.clone(),
                property: "LABELS".to_string(),
                property_value: format_json_array(&["FILTER".to_string()]),
            });
        }
        rows.push(DescribeRow {
            object_kind: "FACT".to_string(),
            object_name: fact.name.clone(),
            parent_entity: parent,
            property: "ACCESS_MODIFIER".to_string(),
            property_value: match fact.access {
                AccessModifier::Public => "PUBLIC".to_string(),
                AccessModifier::Private => "PRIVATE".to_string(),
            },
        });
    }
}

/// Collect DIMENSION property rows from the definition.
///
/// Each dimension emits: `TABLE`, `EXPRESSION`, `DATA_TYPE`, plus `COMMENT` /
/// `SYNONYMS` / `LABELS` when set. `LABELS` is `["FILTER"]` for a named filter.
fn collect_dimension_rows(
    def: &SemanticViewDefinition,
    base_table: &str,
    alias_map: &HashMap<String, String>,
    types: &MemberTypes,
    rows: &mut Vec<DescribeRow>,
) {
    for dim in &def.dimensions {
        let parent = dim
            .source_table
            .as_ref()
            .and_then(|a| alias_map.get(a).cloned())
            .unwrap_or_else(|| base_table.to_string());

        rows.push(DescribeRow {
            object_kind: "DIMENSION".to_string(),
            object_name: dim.name.clone(),
            parent_entity: parent.clone(),
            property: "TABLE".to_string(),
            property_value: parent.clone(),
        });
        rows.push(DescribeRow {
            object_kind: "DIMENSION".to_string(),
            object_name: dim.name.clone(),
            parent_entity: parent.clone(),
            property: "EXPRESSION".to_string(),
            property_value: dim.expr.clone(),
        });
        rows.push(DescribeRow {
            object_kind: "DIMENSION".to_string(),
            object_name: dim.name.clone(),
            parent_entity: parent.clone(),
            property: "DATA_TYPE".to_string(),
            property_value: types.dimension(&dim.name, dim.output_type.as_ref()),
        });
        if let Some(ref comment) = dim.comment {
            rows.push(DescribeRow {
                object_kind: "DIMENSION".to_string(),
                object_name: dim.name.clone(),
                parent_entity: parent.clone(),
                property: "COMMENT".to_string(),
                property_value: comment.clone(),
            });
        }
        if !dim.synonyms.is_empty() {
            rows.push(DescribeRow {
                object_kind: "DIMENSION".to_string(),
                object_name: dim.name.clone(),
                parent_entity: parent.clone(),
                property: "SYNONYMS".to_string(),
                property_value: format_json_array(&dim.synonyms),
            });
        }
        if dim.is_filter {
            rows.push(DescribeRow {
                object_kind: "DIMENSION".to_string(),
                object_name: dim.name.clone(),
                parent_entity: parent,
                property: "LABELS".to_string(),
                property_value: format_json_array(&["FILTER".to_string()]),
            });
        }
    }
}

/// Collect METRIC and `DERIVED_METRIC` property rows from the definition.
///
/// Metrics with `source_table: Some(...)` emit as METRIC (TABLE, EXPRESSION, `DATA_TYPE`).
/// Metrics with `source_table: None` emit as `DERIVED_METRIC` (EXPRESSION, `DATA_TYPE` only).
// One row-builder per metric shape (regular, window, semi-additive, derived);
// each branch emits a distinct fixed set of property rows, so the length is
// inherent to the DESCRIBE row schema rather than tangled logic.
#[allow(clippy::too_many_lines)]
fn collect_metric_rows(
    def: &SemanticViewDefinition,
    base_table: &str,
    alias_map: &HashMap<String, String>,
    types: &MemberTypes,
    rows: &mut Vec<DescribeRow>,
) {
    for metric in &def.metrics {
        let is_derived = metric.source_table.is_none();
        let object_kind = if is_derived {
            "DERIVED_METRIC"
        } else {
            "METRIC"
        };
        let parent = if is_derived {
            String::new()
        } else {
            metric
                .source_table
                .as_ref()
                .and_then(|a| alias_map.get(a).cloned())
                .unwrap_or_else(|| base_table.to_string())
        };

        if !is_derived {
            rows.push(DescribeRow {
                object_kind: object_kind.to_string(),
                object_name: metric.name.clone(),
                parent_entity: parent.clone(),
                property: "TABLE".to_string(),
                property_value: parent.clone(),
            });
        }
        rows.push(DescribeRow {
            object_kind: object_kind.to_string(),
            object_name: metric.name.clone(),
            parent_entity: parent.clone(),
            property: "EXPRESSION".to_string(),
            property_value: metric.expr.clone(),
        });
        rows.push(DescribeRow {
            object_kind: object_kind.to_string(),
            object_name: metric.name.clone(),
            parent_entity: parent.clone(),
            property: "DATA_TYPE".to_string(),
            property_value: types.metric(&metric.name, metric.output_type.as_ref()),
        });
        if let Some(ref comment) = metric.comment {
            rows.push(DescribeRow {
                object_kind: object_kind.to_string(),
                object_name: metric.name.clone(),
                parent_entity: parent.clone(),
                property: "COMMENT".to_string(),
                property_value: comment.clone(),
            });
        }
        if !metric.synonyms.is_empty() {
            rows.push(DescribeRow {
                object_kind: object_kind.to_string(),
                object_name: metric.name.clone(),
                parent_entity: parent.clone(),
                property: "SYNONYMS".to_string(),
                property_value: format_json_array(&metric.synonyms),
            });
        }
        rows.push(DescribeRow {
            object_kind: object_kind.to_string(),
            object_name: metric.name.clone(),
            parent_entity: parent.clone(),
            property: "ACCESS_MODIFIER".to_string(),
            property_value: match metric.access {
                AccessModifier::Public => "PUBLIC".to_string(),
                AccessModifier::Private => "PRIVATE".to_string(),
            },
        });
        if !metric.non_additive_by.is_empty() {
            // C-5 (code-review 2026-07-11): single-sourced with GET_DDL. The
            // previous inline copy omitted NULLS LAST (asymmetric with the
            // always-explicit NULLS that GET_DDL emits for the same object).
            rows.push(DescribeRow {
                object_kind: object_kind.to_string(),
                object_name: metric.name.clone(),
                parent_entity: parent.clone(),
                property: "NON_ADDITIVE_BY".to_string(),
                property_value: crate::render_ddl::render_non_additive_entries(
                    &metric.non_additive_by,
                ),
            });
        }
        if let Some(ref ws) = metric.window_spec {
            // C-5: single-sourced with GET_DDL. The previous inline copy
            // dropped `frame_clause` entirely — DESCRIBE silently
            // under-reported metrics carrying RANGE/ROWS frames.
            rows.push(DescribeRow {
                object_kind: object_kind.to_string(),
                object_name: metric.name.clone(),
                parent_entity: parent,
                property: "WINDOW_SPEC".to_string(),
                property_value: crate::render_ddl::render_window_spec(ws),
            });
        }
    }
}

/// Collect MATERIALIZATION property rows from the definition.
///
/// Each materialization emits three rows: TABLE, DIMENSIONS, METRICS.
/// When `def.materializations` is empty, no rows are added.
fn collect_materialization_rows(def: &SemanticViewDefinition, rows: &mut Vec<DescribeRow>) {
    for mat in &def.materializations {
        rows.push(DescribeRow {
            object_kind: "MATERIALIZATION".to_string(),
            object_name: mat.name.clone(),
            parent_entity: String::new(),
            property: "TABLE".to_string(),
            property_value: mat.table.clone(),
        });
        rows.push(DescribeRow {
            object_kind: "MATERIALIZATION".to_string(),
            object_name: mat.name.clone(),
            parent_entity: String::new(),
            property: "DIMENSIONS".to_string(),
            property_value: format_json_array(&mat.dimensions),
        });
        rows.push(DescribeRow {
            object_kind: "MATERIALIZATION".to_string(),
            object_name: mat.name.clone(),
            parent_entity: String::new(),
            property: "METRICS".to_string(),
            property_value: format_json_array(&mat.metrics),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_json_array_single() {
        assert_eq!(format_json_array(&["id".to_string()]), r#"["id"]"#);
    }

    #[test]
    fn format_json_array_multiple() {
        let cols = vec!["first_name".to_string(), "last_name".to_string()];
        assert_eq!(format_json_array(&cols), r#"["first_name","last_name"]"#);
    }

    #[test]
    fn format_json_array_empty() {
        let cols: Vec<String> = vec![];
        assert_eq!(format_json_array(&cols), "[]");
    }

    /// C-12 (code-review 2026-07-11): a value containing `"` or `\` must be
    /// escaped so the DESCRIBE output stays valid JSON. The previous
    /// `format!("\"{s}\"")` emitted `["a"b"]` — unparseable.
    #[test]
    fn format_json_array_escapes_quote_and_backslash() {
        let items = vec![r#"a"b"#.to_string(), r"c\d".to_string()];
        let json = format_json_array(&items);
        assert_eq!(json, r#"["a\"b","c\\d"]"#);
        // Round-trips through a real JSON parser back to the originals.
        let parsed: Vec<String> = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed, items);
    }

    /// C-12: control characters (a synonym pasted with an embedded newline or
    /// tab) are escaped rather than emitted raw into the JSON string.
    #[test]
    fn format_json_array_escapes_control_characters() {
        let items = vec!["line1\nline2\t".to_string()];
        let json = format_json_array(&items);
        assert_eq!(json, r#"["line1\nline2\t"]"#);
        let parsed: Vec<String> = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed, items);
    }

    /// C-12: non-ASCII passes through as raw UTF-8 (valid JSON), matching the
    /// prior format so existing DESCRIBE fixtures stay byte-identical.
    #[test]
    fn format_json_array_preserves_utf8_unescaped() {
        assert_eq!(format_json_array(&["café★".to_string()]), "[\"café★\"]");
    }

    #[test]
    fn window_spec_property_row_emitted() {
        use crate::model::{
            AccessModifier, Dimension, Metric, NullsOrder, SortOrder, TableRef, WindowOrderBy,
            WindowSpec,
        };
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
                    name: "total_qty".to_string(),
                    expr: "SUM(o.qty)".to_string(),
                    source_table: Some("o".to_string()),
                    access: AccessModifier::Public,
                    ..Default::default()
                },
                Metric {
                    name: "avg_qty".to_string(),
                    expr: "AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY region)"
                        .to_string(),
                    source_table: Some("o".to_string()),
                    access: AccessModifier::Public,
                    window_spec: Some(WindowSpec {
                        window_function: "AVG".to_string(),
                        inner_metric: "total_qty".to_string(),
                        extra_args: vec![],
                        excluding_dims: vec!["region".to_string()],
                        partition_dims: vec![],
                        order_by: vec![WindowOrderBy {
                            expr: "region".to_string(),
                            order: SortOrder::Desc,
                            nulls: NullsOrder::First,
                        }],
                        frame_clause: None,
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let alias_map = def.alias_to_table_map();
        let mut rows = Vec::new();
        collect_metric_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );

        // Find the WINDOW_SPEC row
        let ws_row = rows
            .iter()
            .find(|r| r.property == "WINDOW_SPEC")
            .expect("Should have WINDOW_SPEC row");
        assert_eq!(ws_row.object_name, "avg_qty");
        assert!(
            ws_row
                .property_value
                .contains("AVG(total_qty) OVER (PARTITION BY EXCLUDING region"),
            "WINDOW_SPEC value should contain parsed spec: {}",
            ws_row.property_value
        );

        // Regular metric should NOT have WINDOW_SPEC
        let total_rows: Vec<&DescribeRow> = rows
            .iter()
            .filter(|r| r.object_name == "total_qty" && r.property == "WINDOW_SPEC")
            .collect();
        assert!(
            total_rows.is_empty(),
            "Regular metric should not have WINDOW_SPEC row"
        );
    }

    #[test]
    fn window_spec_property_includes_frame_clause_and_explicit_nulls() {
        // C-5 regression (code-review 2026-07-11): DESCRIBE's inline
        // window-spec renderer had drifted from GET_DDL's — it dropped
        // `frame_clause` entirely (a RANGE/ROWS frame silently vanished from
        // DESCRIBE output) and emitted NULLS FIRST only. Both must now render
        // through the shared `render_ddl` helpers, byte-identical to GET_DDL.
        use crate::model::{
            AccessModifier, Metric, NonAdditiveDim, NullsOrder, SortOrder, TableRef, WindowOrderBy,
            WindowSpec,
        };
        let ws = WindowSpec {
            window_function: "SUM".to_string(),
            inner_metric: "total_qty".to_string(),
            extra_args: vec![],
            excluding_dims: vec![],
            partition_dims: vec!["region".to_string()],
            order_by: vec![WindowOrderBy {
                expr: "d".to_string(),
                order: SortOrder::Asc,
                nulls: NullsOrder::Last,
            }],
            frame_clause: Some("RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW".into()),
        };
        let def = SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            }],
            metrics: vec![Metric {
                name: "windowed".to_string(),
                expr: "SUM(total_qty) OVER (...)".to_string(),
                source_table: Some("o".to_string()),
                access: AccessModifier::Public,
                window_spec: Some(ws.clone()),
                non_additive_by: vec![NonAdditiveDim {
                    dimension: "snap_date".to_string(),
                    order: SortOrder::Desc,
                    nulls: NullsOrder::Last,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let alias_map = def.alias_to_table_map();
        let mut rows = Vec::new();
        collect_metric_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );

        let ws_row = rows
            .iter()
            .find(|r| r.property == "WINDOW_SPEC")
            .expect("Should have WINDOW_SPEC row");
        assert!(
            ws_row
                .property_value
                .contains("RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW"),
            "frame clause must survive into DESCRIBE output: {}",
            ws_row.property_value
        );
        assert_eq!(
            ws_row.property_value,
            crate::render_ddl::render_window_spec(&ws),
            "DESCRIBE and GET_DDL must render the identical window spec"
        );

        let na_row = rows
            .iter()
            .find(|r| r.property == "NON_ADDITIVE_BY")
            .expect("Should have NON_ADDITIVE_BY row");
        assert_eq!(
            na_row.property_value, "snap_date DESC NULLS LAST",
            "NULLS LAST must be explicit (previously omitted)"
        );
    }

    /// Build a one-table def with one dimension and one fact, each flagged as a
    /// named filter per the arguments.
    fn filter_def(dim_is_filter: bool, fact_is_filter: bool) -> SemanticViewDefinition {
        use crate::model::{Dimension, Fact, TableRef};
        SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            }],
            dimensions: vec![Dimension {
                name: "is_domestic".to_string(),
                expr: "o.country = 'US'".to_string(),
                source_table: Some("o".to_string()),
                is_filter: dim_is_filter,
                ..Default::default()
            }],
            facts: vec![Fact {
                name: "is_large".to_string(),
                expr: "o.amount > 100".to_string(),
                source_table: Some("o".to_string()),
                is_filter: fact_is_filter,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// A named filter surfaces as a `LABELS` property row valued `["FILTER"]`.
    /// Like `ACCESS_MODIFIER`, this is a DESCRIBE property row rather than a new
    /// `SHOW` column — `SHOW SEMANTIC {DIMENSIONS,FACTS}` stays at its 8
    /// Snowflake-aligned columns.
    #[test]
    fn labels_property_row_emitted_for_filter_dimension_and_fact() {
        let def = filter_def(true, true);
        let alias_map = def.alias_to_table_map();
        let mut rows = Vec::new();
        collect_dimension_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );
        collect_fact_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );

        let dim_row = rows
            .iter()
            .find(|r| r.object_kind == "DIMENSION" && r.property == "LABELS")
            .expect("dimension should have a LABELS row");
        assert_eq!(dim_row.object_name, "is_domestic");
        assert_eq!(dim_row.property_value, r#"["FILTER"]"#);
        assert_eq!(dim_row.parent_entity, "orders");

        let fact_row = rows
            .iter()
            .find(|r| r.object_kind == "FACT" && r.property == "LABELS")
            .expect("fact should have a LABELS row");
        assert_eq!(fact_row.object_name, "is_large");
        assert_eq!(fact_row.property_value, r#"["FILTER"]"#);
        assert_eq!(fact_row.parent_entity, "orders");
    }

    /// A non-filter member emits no `LABELS` row at all, matching how `COMMENT`
    /// and `SYNONYMS` are omitted when unset rather than emitted empty.
    #[test]
    fn no_labels_row_when_member_is_not_a_filter() {
        let def = filter_def(false, false);
        let alias_map = def.alias_to_table_map();
        let mut rows = Vec::new();
        collect_dimension_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );
        collect_fact_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );
        assert!(
            !rows.iter().any(|r| r.property == "LABELS"),
            "no spurious LABELS row for a plain member"
        );
    }

    /// The flag is per-member: flagging the dimension must not label the fact.
    #[test]
    fn labels_row_is_per_member() {
        let def = filter_def(true, false);
        let alias_map = def.alias_to_table_map();
        let mut rows = Vec::new();
        collect_dimension_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );
        collect_fact_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );
        let labelled: Vec<&str> = rows
            .iter()
            .filter(|r| r.property == "LABELS")
            .map(|r| r.object_kind.as_str())
            .collect();
        assert_eq!(labelled, vec!["DIMENSION"]);
    }

    /// The fact `ACCESS_MODIFIER` row is emitted last and consumes the parent
    /// entity; inserting `LABELS` ahead of it must not disturb either row.
    #[test]
    fn labels_row_precedes_access_modifier_and_both_keep_their_parent() {
        let def = filter_def(false, true);
        let alias_map = def.alias_to_table_map();
        let mut rows = Vec::new();
        collect_fact_rows(
            &def,
            "orders",
            &alias_map,
            &MemberTypes::default(),
            &mut rows,
        );
        let props: Vec<&str> = rows.iter().map(|r| r.property.as_str()).collect();
        let labels_at = props.iter().position(|p| *p == "LABELS").expect("LABELS");
        let access_at = props
            .iter()
            .position(|p| *p == "ACCESS_MODIFIER")
            .expect("ACCESS_MODIFIER");
        assert!(labels_at < access_at, "LABELS before ACCESS_MODIFIER");
        assert_eq!(rows[access_at].property_value, "PUBLIC");
        assert!(
            rows.iter().all(|r| r.parent_entity == "orders"),
            "every fact row keeps its parent entity"
        );
    }
}
