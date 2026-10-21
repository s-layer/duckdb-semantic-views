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
// → `sv_register_table_function`). The bind callback opens a per-call
// `Connection(*context.db)` and bridges to Rust via
// `sv_list_semantic_views_bind_rust` (defined below).
//
// The bridge mechanism is a `reinterpret_cast` of the stack `Connection *`
// to `duckdb_connection` — confirmed by reading `duckdb.cpp:266432-266447`
// where `duckdb_connect` is literally
// `reinterpret_cast<duckdb_connection>(new Connection(...))`. The Rust
// dispatcher receives a BORROWED handle: it MUST NOT call
// `duckdb_disconnect` (would `delete` the stack-allocated Connection — UB).
// The C++ bind scope's `~Connection()` handles teardown.
//
// The legacy `ListSemanticViewsVTab` + `ListTerseSemanticViewsVTab` impl
// blocks were deleted in the Batch 3 cleanup commit (along with all 17
// other dead VTab/VScalar carcasses) together with the H2 query_conn
// allocation in `src/lib.rs::init_extension`.

/// FFI entry point: read the catalog and serialize all semantic views as a
/// length-prefixed binary buffer for the C++ bind callback to parse.
///
/// Called from `cpp/src/shim.cpp::sv_list_semantic_views_bind`. The C++
/// side passes a per-call `duckdb_connection` borrowed from a stack
/// `Connection probe(*context.db)`. This function wraps the handle in a
/// `CatalogReader`, performs the catalog read, and serializes the rows via
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
/// The `conn` parameter is a BORROWED handle (bridge lifecycle, critical) — the
/// underlying C++ `Connection` is owned by a stack local in the C++ bind
/// callback. This function MUST NOT:
///
/// * call `duckdb_disconnect(conn)` (would `delete` a stack object — UB),
/// * stash the handle in long-lived storage (would dangle after bind),
/// * call functions that take ownership of the handle (none in the
///   `CatalogReader` path — `CatalogReader::new` only stores the raw pointer,
///   and the prepared-statement / query helpers in `src/catalog.rs` operate on
///   the handle without consuming it).
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
/// The original `init_extension` probes whether `semantic_layer._definitions`
/// exists once at LOAD time and passes the flag to `CatalogReader::new`.
/// Under the per-call connection model that probe can't be done at LOAD
/// time anymore (the per-call conn doesn't exist yet). We probe inline
/// here on every bind: cheap (single `information_schema.tables` lookup)
/// and correct against schema drift mid-session.
///
/// The probe runs on the same per-call connection the subsequent catalog
/// read uses, so probe and read agree with each other. It does **not**
/// share the caller's catalog / search-path view — that connection is a
/// fresh `Connection(*context.db)` on the primary catalog with its own
/// transaction, which is the whole premise of TECH-DEBT #19 (reads see
/// committed state and the primary catalog; unqualified names are resolved
/// by the caller's search path, which the shim reads off the caller's
/// context and passes in, not by this connection's own session state). This comment claimed the opposite until
/// the 2026-08-08 review.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_list_semantic_views_bind_rust(
    conn: libduckdb_sys::duckdb_connection,
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
        "sv_list_semantic_views_bind_rust",
        |borrowed| unsafe {
            list_view_rows(borrowed, /* include_comment = */ true)
        },
    )
}

/// Shared body for both `list_semantic_views()` (6 columns) and
/// `list_terse_semantic_views()` (5 columns — no trailing `comment`): probe
/// the catalog, read every definition, and serialize the rows over the shared
/// varchar wire format, name-sorted for byte-stable output.
///
/// FF-9: a genuine probe-query failure surfaces as an error rather than being
/// folded into "no views" (an attached read-only DB without a bootstrapped
/// catalog still returns 0 rows via `probe_catalog_table_present == false`).
///
/// # Safety
///
/// `borrowed` must wrap a live `duckdb_connection` (guaranteed by
/// `run_dispatcher`, which constructs and null-checks it before calling).
#[cfg(feature = "extension")]
unsafe fn list_view_rows(
    borrowed: &crate::ddl::read_ffi::BorrowedConnection,
    include_comment: bool,
) -> Result<Vec<u8>, String> {
    use crate::ddl::read_ffi::{probe_catalog_table_present, serialize_varchar_rows};

    let table_present = probe_catalog_table_present(borrowed)?;
    let reader = CatalogReader::new(borrowed, table_present);
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

// Phase 65.1 Plan 03a (IN-06 / D-26): the module-local duplicates of
// `probe_catalog_table_present` and `write_err` were DELETED. The canonical
// definitions in `src/ddl/read_ffi.rs` are imported at each call site.
// Single source of truth + the BorrowedConnection migration on the
// canonical version flows to all 17 read-side dispatchers.

// ---------------------------------------------------------------------------
// list_terse_semantic_views — Phase 65 Plan 05 Task 2 (Wave 1)
// ---------------------------------------------------------------------------
//
// Same bridge mechanism as `list_semantic_views` (see Wave 0 spike docs at
// the top of this file). Bind callback opens a per-call
// `Connection(*context.db)` and calls `sv_list_terse_semantic_views_bind_rust`.
// Borrow contract: dispatcher MUST NOT call `duckdb_disconnect` on the
// borrowed handle.

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
/// `conn` is a borrowed handle (see file-level docs). Caller must release
/// the returned buffer via `sv_free_buffer(*out_ptr, *out_len)`.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_list_terse_semantic_views_bind_rust(
    conn: libduckdb_sys::duckdb_connection,
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
        "sv_list_terse_semantic_views_bind_rust",
        |borrowed| unsafe {
            list_view_rows(borrowed, /* include_comment = */ false)
        },
    )
}
