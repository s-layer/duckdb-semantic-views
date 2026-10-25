//! Rust↔C++ FFI bridges for the `__sv_compute_*` helper table functions.
//!
//! Phase 65 Plan 04 introduces the first such helper, `__sv_compute_create_from_yaml`,
//! whose C++ bind callback (in `cpp/src/shim.cpp`) reads the YAML file directly
//! through `DuckDB`'s `FileSystem` API (Phase 65.1 D-01..D-03 replaced the
//! earlier `read_text(?)` SQL path), then calls into Rust here to parse the
//! YAML, run validation +
//! cardinality inference, and serialize the resulting JSON. The bind returns
//! the JSON in a single VARCHAR row; the outer INSERT the CREATE lowers to (in
//! `src/parse/native_sql.rs::emit_native_create_from_yaml_file`) wraps that row with
//! `json_merge_patch` + `json_object` to add the metadata fields
//! (`created_on`, `database_name`, `schema_name`) on the caller's connection.
//!
//! Read-elimination architecture (Phase 65 D-11): the YAML read no longer
//! goes through any long-lived extension-owned connection (the `catalog_conn`
//! static and the `OverrideContext` that once held it were both retired —
//! see AR-7). The bind reads the file through `DuckDB`'s `FileSystem` API and
//! opens no connection, so nothing outlives the user's `close()`.
//!
//! FFI safety conventions (as for the grammar bridge, `src/grammar/ffi.rs`):
//!   * Every entry point wraps its body in `std::panic::catch_unwind(
//!     AssertUnwindSafe(...))` so panics never cross the C++ boundary (UB).
//!   * Heap-owned UTF-8 buffers are allocated as `Box<[u8]>::into_raw` and
//!     released by the caller via `sv_free_buffer(ptr, len)` with the exact
//!     (ptr, len) pair Rust returned. The buffer is NOT NUL-terminated.
//!   * Error messages are written to a fixed-size `error_buf` via the
//!     shared [`crate::ffi_util::write_error_to_buffer`], capped at
//!     `error_buf_len - 1` bytes (on a UTF-8 char boundary) with a NUL
//!     terminator. Buffer publication uses the shared both-or-drop
//!     [`crate::ffi_util::publish_owned_string`] (ST-4 consolidation —
//!     this module's local copies truncated mid-codepoint and leaked on
//!     null out-pointers).

#[cfg(feature = "extension")]
use crate::ffi_util::{publish_owned_string, write_error_to_buffer as write_error_buf};
#[cfg(feature = "extension")]
use std::panic::AssertUnwindSafe;

/// FFI entry point: parse + enrich + serialize a YAML semantic-view definition.
///
/// Called from the C++ bind callback for `__sv_compute_create_from_yaml`
/// (see `cpp/src/shim.cpp::sv_create_from_yaml_bind`). The C++ side has
/// already read the YAML file directly through `DuckDB`'s `FileSystem` API,
/// so this function only sees the already-loaded bytes — no file I/O on the
/// Rust side.
///
/// Returns the metadata-less JSON definition. The outer INSERT the CREATE
/// lowers to wraps the JSON with `json_merge_patch(new_def, json_object(...))`
/// to populate `created_on` / `database_name` / `schema_name` on the
/// caller's connection (preserving D-21 transactional contract).
///
/// # Parameters
///
/// * `content_ptr`/`content_len` — YAML bytes (the result of `read_text` on
///   the user-supplied file path). Validated as UTF-8 here; non-UTF-8
///   returns rc=1.
/// * `name_ptr`/`name_len` — bare view name (the identifier after `CREATE
///   SEMANTIC VIEW`). Must be valid UTF-8.
/// * `comment_ptr`/`comment_len` — optional `COMMENT='...'` clause value.
///   When `len == 0` the helper leaves `def.comment` untouched so the
///   YAML's own `comment:` field (if any) survives. When `len > 0` the
///   supplied comment overrides the YAML's comment.
/// * `out_ptr`/`out_len` — on rc=0, point to a heap-owned UTF-8 buffer
///   containing the JSON. Caller MUST release via `sv_free_buffer`.
/// * `error_buf`/`error_buf_len` — on rc != 0, gets a NUL-terminated
///   message.
///
/// # Return codes
///
/// * `0` — success; `(out_ptr, out_len)` populated.
/// * `1` — YAML parse / size-cap / UTF-8 error; `error_buf` populated.
/// * `2` — enrichment / validation error (D-06 hard error, graph validation
///   failure, etc.); `error_buf` populated.
/// * `3` — internal error (panic across FFI, serialization failure);
///   `error_buf` populated.
///
/// # Safety
///
/// * `content_ptr` must point to `content_len` valid bytes. Empty input
///   (`content_len == 0`) returns rc=1 with a "YAML is empty" message.
/// * `name_ptr` must point to `name_len` valid UTF-8 bytes. Non-UTF-8 or
///   empty name returns rc=1.
/// * `comment_ptr` may be null when `comment_len == 0`.
/// * `out_ptr` / `out_len` must point to writable slots, or be null (in
///   which case the success buffer is leaked — defensive only).
/// * `error_buf` must point to `error_buf_len` writable bytes.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_compute_create_from_yaml_rust(
    content_ptr: *const u8,
    content_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    comment_ptr: *const u8,
    comment_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // ---- Decode inputs ----
        if content_ptr.is_null() {
            write_error_buf(error_buf, error_buf_len, "YAML content pointer is null");
            return 1_u8;
        }
        if name_ptr.is_null() || name_len == 0 {
            write_error_buf(error_buf, error_buf_len, "view name is empty");
            return 1_u8;
        }

        let content_bytes = std::slice::from_raw_parts(content_ptr, content_len);
        let Ok(content) = std::str::from_utf8(content_bytes) else {
            write_error_buf(error_buf, error_buf_len, "YAML content is not valid UTF-8");
            return 1_u8;
        };

        let name_bytes = std::slice::from_raw_parts(name_ptr, name_len);
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            write_error_buf(error_buf, error_buf_len, "view name is not valid UTF-8");
            return 1_u8;
        };

        let comment_opt: Option<String> = if comment_len == 0 || comment_ptr.is_null() {
            None
        } else {
            let comment_bytes = std::slice::from_raw_parts(comment_ptr, comment_len);
            if let Ok(s) = std::str::from_utf8(comment_bytes) {
                Some(s.to_owned())
            } else {
                write_error_buf(error_buf, error_buf_len, "COMMENT value is not valid UTF-8");
                return 1_u8;
            }
        };

        // ---- Parse YAML (size-cap enforced inside from_yaml_with_size_cap) ----
        let mut def =
            match crate::model::SemanticViewDefinition::from_yaml_with_size_cap(name, content) {
                Ok(def) => def,
                Err(e) => {
                    write_error_buf(error_buf, error_buf_len, &e);
                    return 1_u8;
                }
            };

        // The COMMENT='...' clause from the outer CREATE statement (if any)
        // overrides whatever comment the YAML carries. When no clause is
        // present, the YAML's own comment survives.
        if let Some(c) = comment_opt {
            def.comment = Some(c);
        }

        // ---- Enrich (validation + cardinality + serialize) ----
        // The slimmed enrich_definition_for_create (Phase 65 Plan 03 D-16)
        // returns a metadata-less JSON. Plan 04 Task 4's outer INSERT wraps
        // it with json_merge_patch to add now()/current_database()/
        // current_schema() on the caller's connection.
        match crate::ddl::define::enrich_definition_for_create(name, def) {
            Ok(json) => {
                publish_owned_string(json, out_ptr, out_len);
                0_u8
            }
            Err(e) => {
                write_error_buf(error_buf, error_buf_len, &e.message);
                2_u8
            }
        }
    }));
    if let Ok(rc) = result {
        rc
    } else {
        write_error_buf(
            error_buf,
            error_buf_len,
            "internal error: panic inside sv_compute_create_from_yaml_rust",
        );
        3
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "extension"))]
mod tests {
    use super::*;
    use std::ffi::CStr;

    /// Convenience: call the FFI function, return rc + new_def (if any) +
    /// error message (if any). Frees the heap-owned buffer via the FFI
    /// `sv_free_buffer` path to mirror real C++ caller behaviour.
    unsafe fn call(yaml: &str, name: &str, comment: Option<&str>) -> (u8, Option<String>, String) {
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err_buf = vec![0u8; 1024];

        let (comment_ptr, comment_len) = match comment {
            Some(c) => (c.as_ptr(), c.len()),
            None => (std::ptr::null(), 0),
        };

        let rc = sv_compute_create_from_yaml_rust(
            yaml.as_ptr(),
            yaml.len(),
            name.as_ptr(),
            name.len(),
            comment_ptr,
            comment_len,
            &mut out_ptr,
            &mut out_len,
            err_buf.as_mut_ptr(),
            err_buf.len(),
        );

        let new_def = if rc == 0 && !out_ptr.is_null() {
            let bytes = std::slice::from_raw_parts(out_ptr, out_len).to_vec();
            // Release through the same FFI path real callers use.
            crate::ffi_util::sv_free_buffer(out_ptr, out_len);
            Some(String::from_utf8(bytes).expect("FFI output is UTF-8"))
        } else {
            None
        };

        let err_str = CStr::from_ptr(err_buf.as_ptr() as *const i8)
            .to_string_lossy()
            .into_owned();

        (rc, new_def, err_str)
    }

    const VALID_YAML: &str = "\
base_table: t
tables:
  - alias: o
    table: t
    pk_columns:
      - id
dimensions:
  - name: id
    expr: o.id
    source_table: o
metrics:
  - name: c
    expr: COUNT(*)
    source_table: o
";

    #[test]
    fn happy_path_returns_metadata_less_json() {
        unsafe {
            let (rc, def, err) = call(VALID_YAML, "v", None);
            assert_eq!(rc, 0, "expected rc=0, got rc={rc} err={err}");
            let json = def.expect("rc=0 should populate new_def");
            // Should contain dimensions/metrics from the YAML.
            assert!(
                json.contains("\"dimensions\""),
                "json missing dimensions: {json}"
            );
            assert!(json.contains("\"metrics\""), "json missing metrics: {json}");
            // Metadata fields should NOT be populated — outer INSERT does that.
            // (created_on/database_name/schema_name use skip_serializing_if so
            // their absence is the success signal.)
            assert!(
                !json.contains("\"created_on\""),
                "json should not carry created_on: {json}"
            );
            assert!(
                !json.contains("\"database_name\""),
                "json should not carry database_name: {json}"
            );
        }
    }

    #[test]
    fn comment_clause_overrides_yaml_comment() {
        unsafe {
            let (rc, def, _err) = call(VALID_YAML, "v", Some("override-me"));
            assert_eq!(rc, 0);
            let json = def.unwrap();
            assert!(
                json.contains("\"override-me\""),
                "expected override comment in: {json}"
            );
        }
    }

    #[test]
    fn invalid_yaml_returns_rc1_with_error() {
        unsafe {
            let (rc, def, err) = call("not: valid: yaml: at all: :", "v", None);
            assert_eq!(rc, 1, "expected rc=1, got rc={rc}");
            assert!(def.is_none());
            assert!(!err.is_empty(), "expected error message");
        }
    }

    #[test]
    fn oversized_yaml_returns_rc1_with_size_cap_error() {
        unsafe {
            // 1 MiB + 1 byte — exceeds YAML_SIZE_CAP.
            let oversized = "a".repeat(crate::model::SemanticViewDefinition::YAML_SIZE_CAP + 1);
            let (rc, _def, err) = call(&oversized, "big", None);
            assert_eq!(rc, 1);
            assert!(err.contains("exceeds"), "expected size-cap message: {err}");
        }
    }

    #[test]
    fn empty_name_returns_rc1() {
        unsafe {
            let (rc, _def, err) = call(VALID_YAML, "", None);
            assert_eq!(rc, 1);
            assert!(err.contains("name"), "expected name-related error: {err}");
        }
    }

    #[test]
    fn null_content_pointer_returns_rc1() {
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err_buf = vec![0u8; 256];
        let rc = unsafe {
            sv_compute_create_from_yaml_rust(
                std::ptr::null(),
                0,
                b"v".as_ptr(),
                1,
                std::ptr::null(),
                0,
                &mut out_ptr,
                &mut out_len,
                err_buf.as_mut_ptr(),
                err_buf.len(),
            )
        };
        assert_eq!(rc, 1);
    }
}
