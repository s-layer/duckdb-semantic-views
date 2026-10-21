//! `SHOW SEMANTIC MATERIALIZATIONS` dispatchers (ST-2).
//!
//! Materializations use a 7-column layout — `database_name, schema_name,
//! semantic_view_name, name, table, dimensions, metrics` — that does not fit
//! the shared 8-column `EntityRow` in [`crate::ddl::show_entities`], so they
//! keep their own row builder. The two `#[no_mangle]` dispatchers still route
//! through the shared [`crate::ddl::read_ffi::run_dispatcher`] scaffold like
//! every other read-side function.

#![cfg(feature = "extension")]

use crate::catalog::CatalogReader;
use crate::ddl::describe::format_json_array;
use crate::ddl::read_ffi::{read_str_arg, run_dispatcher, serialize_varchar_rows};
use crate::model::SemanticViewDefinition;

/// A single 7-column row of SHOW SEMANTIC MATERIALIZATIONS output.
struct ShowMatRow {
    database_name: String,
    schema_name: String,
    semantic_view_name: String,
    name: String,
    table: String,
    dimensions: String,
    metrics: String,
}

impl ShowMatRow {
    /// Flatten into the ordered VARCHAR cells for the wire format.
    fn into_cells(self) -> Vec<String> {
        vec![
            self.database_name,
            self.schema_name,
            self.semantic_view_name,
            self.name,
            self.table,
            self.dimensions,
            self.metrics,
        ]
    }
}

/// Collect materialization rows for a single already-parsed view. Parsing
/// (and the FF-9 decision of whether an unparseable definition is a hard error
/// or a skipped row) happens at the call site.
fn collect_mats(view_name: &str, def: &SemanticViewDefinition) -> Vec<ShowMatRow> {
    let db_name = def.database_name.clone().unwrap_or_default();
    let sch_name = def.schema_name.clone().unwrap_or_default();
    def.materializations
        .iter()
        .map(|m| ShowMatRow {
            database_name: db_name.clone(),
            schema_name: sch_name.clone(),
            semantic_view_name: view_name.to_string(),
            name: m.name.clone(),
            table: m.table.clone(),
            dimensions: format_json_array(&m.dimensions),
            metrics: format_json_array(&m.metrics),
        })
        .collect()
}

/// # Safety
///
/// The caller releases the returned buffer via `sv_free_buffer`.
#[no_mangle]
pub unsafe extern "C" fn sv_show_semantic_materializations_all_bind_rust(
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    run_dispatcher(
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_show_semantic_materializations_all_bind_rust",
        || {
            let reader = CatalogReader::for_caller()?;
            let entries = reader.list_all()?;
            let mut rows: Vec<Vec<String>> = Vec::new();
            for (name, json) in &entries {
                // FF-9: `_all` stays tolerant — skip a view whose stored JSON
                // won't parse rather than failing the whole listing.
                let Ok(def) = SemanticViewDefinition::from_json(name, json) else {
                    continue;
                };
                for r in collect_mats(name, &def) {
                    rows.push(r.into_cells());
                }
            }
            rows.sort_by(|a, b| a[2].cmp(&b[2]).then_with(|| a[3].cmp(&b[3])));
            serialize_varchar_rows(&rows)
        },
    )
}

/// # Safety
///
/// `name_ptr` must point to `name_len` UTF-8 bytes. The caller releases the returned buffer via `sv_free_buffer`.
#[no_mangle]
pub unsafe extern "C" fn sv_show_semantic_materializations_bind_rust(
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    run_dispatcher(
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_show_semantic_materializations_bind_rust",
        || {
            let view_name = unsafe { read_str_arg(name_ptr, name_len, "view name") }?;
            // FF-4: normalize so quoted-identifier inputs resolve like
            // `semantic_view()` does.
            let view = crate::ident::parse_view_ref(&view_name)
                .map_err(|e| format!("Invalid view name '{view_name}': {e}"))?;
            let view_name = view.name.clone();
            let search_path = unsafe { crate::ddl::read_ffi::read_search_path(sp_ptr, sp_len) }?;
            let reader = CatalogReader::for_caller()?;
            let Some(json) = reader.lookup(&view, &search_path)? else {
                return Err(crate::catalog::view_not_found_msg(&view.to_string()));
            };
            // FF-9: named single-view SHOW propagates a parse error rather than
            // silently returning zero rows for a corrupt definition.
            let def = SemanticViewDefinition::from_json(&view_name, &json)?;
            let mut internal = collect_mats(&view_name, &def);
            internal.sort_by(|a, b| a.name.cmp(&b.name));
            let rows: Vec<Vec<String>> = internal.into_iter().map(ShowMatRow::into_cells).collect();
            serialize_varchar_rows(&rows)
        },
    )
}
