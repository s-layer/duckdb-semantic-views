//! `READ_YAML_FROM_SEMANTIC_VIEW` scalar function: wraps
//! [`crate::render_yaml::render_yaml_export`] as a C++ Catalog API scalar so
//! that `SELECT READ_YAML_FROM_SEMANTIC_VIEW('name')` works inside `DuckDB`.
//!
//! The render logic itself lives in [`crate::render_yaml`] (always compiled,
//! unit-tested under `cargo test`). This module adds the extension-only Rust
//! FFI dispatcher reached from `sv_register_read_yaml_from_semantic_view` in
//! `cpp/src/shim.cpp`.
//!
//! # Phase 65 Plan 05 Task 4 (Wave 3) — Batch 3 final cleanup
//!
//! The legacy `ReadYamlFromSemanticViewScalar` `VScalar` impl block was
//! retired in the same commit that deleted the H2 `query_conn` allocation; all
//! live invocations of `SELECT READ_YAML_FROM_SEMANTIC_VIEW(...)` now route
//! through [`sv_read_yaml_from_semantic_view_exec_rust`] below.

use crate::catalog::CatalogReader;
use crate::model::SemanticViewDefinition;
use crate::render_yaml::render_yaml_export;

/// Parse a potentially qualified view name into a reference.
/// Supports: `"view_name"`, `"schema.view_name"`, `"database.schema.view_name"`.
///
/// Delegates to [`crate::ident::parse_view_ref_lenient`] (PA-10, code-review
/// 2026-07-02): the previous naive `rsplit('.')` split inside quoted parts,
/// so `"a.b"` resolved to `b"` instead of `a.b`. Malformed names fall back to
/// an unqualified reference carrying the input verbatim, so the lookup fails
/// with the canonical "does not exist" rather than a grammar error.
fn resolve_view_ref(input: &str) -> crate::ident::ViewRef {
    crate::ident::parse_view_ref_lenient(input)
}

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 Task 4 (Wave 3) — sv_read_yaml_from_semantic_view_exec_rust
// ---------------------------------------------------------------------------
// FFI dispatcher for the migrated `read_yaml_from_semantic_view(name)`
// scalar. Invoked once per row by the C++ exec callback
// `sv_read_yaml_from_semantic_view_exec` in cpp/src/shim.cpp, in the scope of
// the executing query, like `sv_get_ddl_exec_rust` and the read-path bind
// dispatchers (see `src/ddl/read_ffi.rs` module docs).

/// # Safety
///
/// `name_ptr` must point to `name_len` UTF-8 bytes (not NUL-terminated).
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_read_yaml_from_semantic_view_exec_rust(
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
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_read_yaml_from_semantic_view_exec_rust",
        || unsafe { read_yaml_export(name_ptr, name_len, sp_ptr, sp_len) },
    )
}

/// Body for [`sv_read_yaml_from_semantic_view_exec_rust`]: resolve the view
/// and render its YAML export.
///
/// # Safety
///
/// `name_ptr` must be null or point to `name_len` readable bytes.
#[cfg(feature = "extension")]
unsafe fn read_yaml_export(
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
) -> Result<Vec<u8>, String> {
    use crate::ddl::read_ffi::read_str_arg;

    let raw_name = read_str_arg(name_ptr, name_len, "view name")?;
    let view = resolve_view_ref(&raw_name);
    let bare_name = view.name.clone();
    let search_path = unsafe { crate::ddl::read_ffi::read_search_path(sp_ptr, sp_len) }?;

    // FF-9: a catalog-read failure is distinct from "no views" (propagated).
    let json = CatalogReader::lookup_in_caller(&view, &search_path)?
        .ok_or_else(|| crate::catalog::view_not_found_msg(&view.to_string()))?;
    // C-2 (code-review 2026-07-11): `from_json` for the canonical
    // "invalid definition for semantic view '<name>'" context on corrupt rows.
    let def = SemanticViewDefinition::from_json(&bare_name, &json)?;
    render_yaml_export(&def).map(String::into_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // These assert on BOTH slots of the parsed reference. The name slot is the
    // behaviour they have always pinned; the schema slot is what the qualifier
    // now decides — semantic views are schema-scoped, so a qualifier is no
    // longer discarded on the way to the lookup.

    #[test]
    fn resolve_view_ref_unqualified() {
        let r = resolve_view_ref("my_view");
        assert_eq!(r.name, "my_view");
        assert_eq!(r.schema, None);
    }

    #[test]
    fn resolve_view_ref_schema_qualified() {
        let r = resolve_view_ref("main.my_view");
        assert_eq!(r.name, "my_view");
        assert_eq!(r.schema.as_deref(), Some("main"));
    }

    #[test]
    fn resolve_view_ref_fully_qualified() {
        let r = resolve_view_ref("memory.main.my_view");
        assert_eq!(r.name, "my_view");
        assert_eq!(r.schema.as_deref(), Some("main"));
        assert_eq!(r.database.as_deref(), Some("memory"));
    }

    #[test]
    fn resolve_view_ref_empty() {
        // Not a well-formed identifier, so the lenient fallback carries the
        // text verbatim and the lookup fails with the canonical "does not
        // exist" rather than an identifier-grammar error.
        let r = resolve_view_ref("");
        assert_eq!(r.name, "");
        assert_eq!(r.schema, None);
    }

    #[test]
    fn resolve_view_ref_quoted_dot_not_split() {
        // PA-10: the old rsplit('.') split inside the quoted part.
        let dotted = resolve_view_ref("\"a.b\"");
        assert_eq!(dotted.name, "a.b");
        assert_eq!(
            dotted.schema, None,
            "a quoted dot is part of the NAME, not a qualifier"
        );
        let spaced = resolve_view_ref("main.\"my view\"");
        assert_eq!(spaced.name, "my view");
        assert_eq!(spaced.schema.as_deref(), Some("main"));
    }

    #[test]
    fn resolve_view_ref_folds_to_lowercase() {
        // View-name lookup folds to lowercase the same way `normalize_view_name`
        // and every other lookup path does — for quoted names too. Under
        // DuckDB's identifier rule (and this project's documented view-name
        // normalization) quoting only lets a name carry special characters; it
        // does NOT preserve case. Stored view names are lowercase, so a request
        // written `"MyView"` must resolve to `myview` to find the view.
        assert_eq!(resolve_view_ref("MyView").name, "myview");
        assert_eq!(resolve_view_ref("\"MyView\"").name, "myview");
        // The qualifier folds on the same rule — it is matched against a stored
        // schema name case-insensitively.
        assert_eq!(
            resolve_view_ref("MySchema.MyView").schema.as_deref(),
            Some("myschema")
        );
    }
}
