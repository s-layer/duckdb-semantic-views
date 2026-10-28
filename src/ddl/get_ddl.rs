//! `GET_DDL` scalar function: wraps
//! [`crate::render_ddl::render_create_ddl_qualified`] as a C++ Catalog API
//! scalar so that `SELECT GET_DDL('SEMANTIC_VIEW', 'name')` works inside
//! `DuckDB`.
//!
//! The optional third argument — `GET_DDL('SEMANTIC_VIEW', 'name', true)` —
//! renders the view's own schema into the `CREATE` name, matching Snowflake's
//! `use_fully_qualified_names`. It is registered as a second arity on the same
//! `ScalarFunctionSet` (see `sv_register_get_ddl` in `cpp/src/shim.cpp`).
//!
//! The render logic itself lives in [`crate::render_ddl`] (always compiled,
//! unit-tested under `cargo test`). This module adds the extension-only Rust
//! FFI dispatcher reached from `sv_register_get_ddl` in `cpp/src/shim.cpp`.
//!
//! # Phase 65 Plan 05 Task 4 (Wave 3) — Batch 3 final cleanup
//!
//! The legacy `GetDdlScalar` `VScalar` impl block was retired in the same
//! commit that deleted the H2 `query_conn` allocation; all live invocations
//! of `SELECT GET_DDL(...)` now route through [`sv_get_ddl_exec_rust`] below.

use crate::catalog::CatalogReader;
use crate::model::SemanticViewDefinition;
use crate::render_ddl::render_create_ddl_qualified;

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 Task 4 (Wave 3) — sv_get_ddl_exec_rust
// ---------------------------------------------------------------------------
// FFI dispatcher for the migrated `get_ddl(object_type, name)` scalar.
// Invoked once per row by the C++ exec callback `sv_get_ddl_exec` in
// cpp/src/shim.cpp, in the scope of the executing query, so the catalog is
// read in that query's transaction (TECH-DEBT #19), as the read-path bind
// dispatchers do (see `src/ddl/read_ffi.rs` module docs).

/// # Safety
///
/// `type_ptr` and `name_ptr` must each point to the corresponding number of UTF-8 bytes (not
/// NUL-terminated). `qualify` is `0` (bare name) or non-zero (schema-qualified
/// name); the C++ side maps the optional third `BOOLEAN` argument onto it and
/// passes `0` when the caller wrote the two-argument form.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_get_ddl_exec_rust(
    type_ptr: *const u8,
    type_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
    qualify: u8,
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
        "sv_get_ddl_exec_rust",
        || unsafe {
            get_ddl(
                type_ptr, type_len, name_ptr, name_len, sp_ptr, sp_len, qualify,
            )
        },
    )
}

/// Body for [`sv_get_ddl_exec_rust`]: validate the object type, resolve the
/// view, and render its `CREATE OR REPLACE SEMANTIC VIEW` DDL.
///
/// # Safety
///
/// `type_ptr` / `name_ptr` must each be null or point to the matching number
/// of readable bytes.
// The argument list mirrors the FFI boundary above (three ptr/len pairs plus
// the qualify flag); collapsing it into a struct would only move the same
// unsafety one hop.
#[allow(clippy::too_many_arguments)]
/// Whether `obj_type` names a semantic view: `SEMANTIC_VIEW`, as in
/// Snowflake's semantic-view guides, or `SEMANTIC VIEW`, as in its `GET_DDL`
/// reference. Case-insensitive; the two words may be separated by any run of
/// whitespace.
#[cfg_attr(not(feature = "extension"), allow(dead_code))]
fn is_semantic_view_type(obj_type: &str) -> bool {
    if obj_type.eq_ignore_ascii_case("SEMANTIC_VIEW") {
        return true;
    }
    let mut words = obj_type.split_whitespace();
    matches!(
        (words.next(), words.next(), words.next()),
        (Some(a), Some(b), None) if a.eq_ignore_ascii_case("SEMANTIC") && b.eq_ignore_ascii_case("VIEW")
    )
}

#[cfg(feature = "extension")]
unsafe fn get_ddl(
    type_ptr: *const u8,
    type_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
    qualify: u8,
) -> Result<Vec<u8>, String> {
    use crate::ddl::read_ffi::read_str_arg;

    let obj_type = read_str_arg(type_ptr, type_len, "object_type")?;
    let raw_name = read_str_arg(name_ptr, name_len, "view name")?;

    if !is_semantic_view_type(&obj_type) {
        return Err(format!(
            "GET_DDL: unsupported object type '{obj_type}'. Only 'SEMANTIC_VIEW' or \
             'SEMANTIC VIEW' is supported."
        ));
    }

    // C-2 (code-review 2026-07-11): normalize the requested name like every
    // other single-view read path (FF-4/PA-8 sweep). Lenient contract mirrors
    // read_yaml's `resolve_bare_name`: a name that does not parse as an
    // identifier is looked up verbatim and fails with the canonical message.
    let view = crate::ident::parse_view_ref_lenient(&raw_name);
    let name = view.name.clone();
    let search_path = unsafe { crate::ddl::read_ffi::read_search_path(sp_ptr, sp_len) }?;

    // FF-9: a catalog-read failure is distinct from "no views" (propagated).
    let json = CatalogReader::lookup_in_caller(&view, &search_path)?
        .ok_or_else(|| crate::catalog::view_not_found_msg(&view.to_string()))?;
    // C-2: `from_json` for the canonical "invalid definition for semantic
    // view '<name>'" context on corrupt rows.
    let def = SemanticViewDefinition::from_json(&name, &json)?;
    render_create_ddl_qualified(&name, &def, qualify != 0)
        .map(String::into_bytes)
        .map_err(|e| format!("GET_DDL error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::is_semantic_view_type;

    #[test]
    fn both_snowflake_spellings_name_a_semantic_view() {
        for t in [
            "SEMANTIC_VIEW",
            "semantic_view",
            "SEMANTIC VIEW",
            "semantic view",
            "Semantic   View",
            " SEMANTIC\tVIEW ",
        ] {
            assert!(is_semantic_view_type(t), "{t:?}");
        }
    }

    #[test]
    fn other_object_types_are_not_a_semantic_view() {
        for t in [
            "TABLE",
            "VIEW",
            "SEMANTIC",
            "SEMANTICVIEW",
            "SEMANTIC VIEW X",
            "SEMANTIC_VIEWS",
            "",
        ] {
            assert!(!is_semantic_view_type(t), "{t:?}");
        }
    }
}
