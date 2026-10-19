use crate::catalog::CatalogReader;
use crate::model::SemanticViewDefinition;

// ---------------------------------------------------------------------------
// list_semantic_views — Phase 65 Plan 05 Task 1 (Wave 0 bridge spike)
// ---------------------------------------------------------------------------
//
// The legacy `ListSemanticViewsVTab` below (`impl VTab`) was registered via
// duckdb-rs's `register_table_function_with_extra_info`, which marshals
// `ClientContext &` away (Plan 01 Spike A6). That registration path is
// RETIRED for `list_semantic_views`; the function is now registered via the
// C++ Catalog API path (`cpp/src/shim.cpp::sv_register_list_semantic_views`
// → `sv_register_table_function`). The bind callback runs
// `sv_list_semantic_views_bind_rust` (defined below) in the caller's scope, so
// the catalog is read in the caller's transaction (TECH-DEBT #19).
//
// The legacy `ListSemanticViewsVTab` + `ListTerseSemanticViewsVTab` impl
// blocks were deleted in the Batch 3 cleanup commit (along with all 17
// other dead VTab/VScalar carcasses) together with the H2 query_conn
// allocation in `src/lib.rs::init_extension`.

/// FFI entry point: read the catalog and serialize all semantic views as a
/// length-prefixed binary buffer for the C++ bind callback to parse.
///
/// Called from `cpp/src/shim.cpp::sv_list_semantic_views_bind`. Reads the
/// catalog as the caller's transaction sees it ([`CatalogReader::for_caller`])
/// and serializes the rows via
/// [`crate::ddl::read_ffi::serialize_varchar_rows`] — the shared, AR-3
/// self-describing wire format (a schema header of column type tags followed
/// by the length-prefixed cells). See that function for the authoritative
/// byte layout; it is intentionally NOT duplicated here to avoid drift.
///
/// The 6 columns match the v0.9.0 Rust `VTab` shape exactly:
/// (`created_on`, name, kind, `database_name`, `schema_name`, comment).
///
/// # Safety
///
/// `out_ptr` / `out_len` / `error_buf` are the C++ side's out-parameters (see
/// [`crate::ddl::read_ffi::run_dispatcher`]).
///
/// # Return codes
///
/// * `0` — success; `(out_ptr, out_len)` populated. Caller MUST release
///   via `sv_free_buffer(ptr, len)`.
/// * `1` — catalog read error (e.g. the `semantic_layer._definitions`
///   table is missing) OR serialization failure; `error_buf` populated.
/// * `2` — internal error (a panic caught at the FFI boundary by
///   `catch_unwind`); `error_buf` populated.
///
/// # Catalog-table-present probing
///
/// Whether `semantic_layer._definitions` exists is part of the same read in
/// the caller's transaction, so presence and rows always agree; a read-only
/// database that never had the table lists no views.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_list_semantic_views_bind_rust(
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    crate::ddl::read_ffi::run_dispatcher(
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_list_semantic_views_bind_rust",
        || list_view_rows(/* include_comment = */ true),
    )
}

/// Shared body for both `list_semantic_views()` (6 columns) and
/// `list_terse_semantic_views()` (5 columns — no trailing `comment`): read
/// every definition, and serialize the rows over the shared
/// varchar wire format, name-sorted for byte-stable output.
///
/// FF-9: a genuine catalog-read failure surfaces as an error rather than
/// being folded into "no views" (an attached read-only DB without a
/// bootstrapped catalog still returns 0 rows).
#[cfg(feature = "extension")]
fn list_view_rows(include_comment: bool) -> Result<Vec<u8>, String> {
    use crate::ddl::read_ffi::serialize_varchar_rows;

    let reader = CatalogReader::for_caller()?;
    let entries = reader.list_all()?;

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(entries.len());
    for (name, json) in &entries {
        let def = SemanticViewDefinition::from_json(name, json).ok();
        let (created_on, database_name, schema_name, comment) = match &def {
            Some(d) => (
                d.created_on.clone().unwrap_or_default(),
                d.database_name.clone().unwrap_or_default(),
                d.schema_name.clone().unwrap_or_default(),
                d.comment.clone().unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new(), String::new()),
        };
        let mut row = vec![
            created_on,
            name.clone(),
            "SEMANTIC_VIEW".to_string(),
            database_name,
            schema_name,
        ];
        if include_comment {
            row.push(comment);
        }
        rows.push(row);
    }
    // Sorted by (schema, name) so two same-named views in different schemas
    // have a stable order rather than an arbitrary one.
    rows.sort_by(|a, b| a[4].cmp(&b[4]).then_with(|| a[1].cmp(&b[1])));

    // FF-6: the shared serializer errors (rather than clamping a length to
    // u32::MAX and desyncing the header from the payload) if a cell or the
    // row count overflows the wire format's u32 fields.
    serialize_varchar_rows(&rows)
}

// ---------------------------------------------------------------------------
// list_terse_semantic_views — Phase 65 Plan 05 Task 2 (Wave 1)
// ---------------------------------------------------------------------------
//
// Same bridge mechanism as `list_semantic_views` (see the docs at the top of
// this file): the bind callback calls `sv_list_terse_semantic_views_bind_rust`
// in the caller's scope.

/// FFI dispatcher for the migrated `list_terse_semantic_views()` table
/// function — 5-column subset of `list_semantic_views()` (no `comment`).
///
/// Serializes via the shared [`crate::ddl::read_ffi::serialize_varchar_rows`]
/// (AR-3 self-describing wire format — see that function for the byte layout).
///
/// Columns: (`created_on`, name, kind, `database_name`, `schema_name`).
///
/// # Safety
///
/// Out-parameters as for `sv_list_semantic_views_bind_rust`. Caller must
/// release the returned buffer via `sv_free_buffer(*out_ptr, *out_len)`.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_list_terse_semantic_views_bind_rust(
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    crate::ddl::read_ffi::run_dispatcher(
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_list_terse_semantic_views_bind_rust",
        || list_view_rows(/* include_comment = */ false),
    )
}
