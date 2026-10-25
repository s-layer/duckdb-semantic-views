//! C-ABI entry points the C++ grammar bridge calls (`cpp/src/shim.cpp`).

use super::transform_panic_guard;
use super::tree::Tree;
use crate::ffi_util::{publish_owned_string, write_error_to_buffer};

/// FFI export: the grammar changes to register ([`super::changes`]), as a
/// heap-owned buffer the caller releases with `sv_free_buffer`.
///
/// Returns 0 on success, 1 if building the list panicked.
///
/// # Safety
///
/// `out_ptr` / `out_len` must each point to a writable slot, or both be null.
#[no_mangle]
pub unsafe extern "C" fn sv_grammar_changes_rust(out_ptr: *mut *mut u8, out_len: *mut usize) -> u8 {
    let result = std::panic::catch_unwind(super::changes);
    match result {
        Ok(text) => {
            publish_owned_string(text, out_ptr, out_len);
            0
        }
        Err(_) => 1,
    }
}

/// FFI export: lower one matched statement rule to SQL ([`super::transform`],
/// then `crate::parse::lower_action`).
///
/// `tree_ptr` / `tree_len` hold the serialised parse subtree ([`Tree::decode`]).
///
/// Returns:
///   0 = success: the SQL is written to `*sql_out_ptr` / `*sql_out_len`
///       (release with `sv_free_buffer`).
///   1 = the statement is ours but invalid: the whole message is also
///       written to `*sql_out_ptr` / `*sql_out_len` (release with
///       `sv_free_buffer`), because `error_out` is a fixed-size buffer that
///       cuts long messages off; `error_out` holds the capped copy.
///       `*position_out` is the error's byte offset in the query, or `u32::MAX`.
///       A bridge failure (a tree that does not decode) and a panic are also
///       rc=1, with an internal-error message, so the user sees a parser error
///       rather than an `InternalException`, which can invalidate the database.
///
/// # Safety
///
/// - `tree_ptr` must point to `tree_len` readable bytes.
/// - `sql_out_ptr` / `sql_out_len` must each point to a writable slot, or both be null.
/// - `error_out` must point to `error_out_len` writable bytes.
/// - `position_out` must point to a writable `u32`, or be null.
#[no_mangle]
pub unsafe extern "C" fn sv_grammar_transform_rust(
    tree_ptr: *const u8,
    tree_len: usize,
    sql_out_ptr: *mut *mut u8,
    sql_out_len: *mut usize,
    error_out: *mut u8,
    error_out_len: usize,
    position_out: *mut u32,
) -> u8 {
    transform_panic_guard(error_out, error_out_len, position_out, || {
        let decoded = if tree_ptr.is_null() {
            Err("empty parse tree".to_string())
        } else {
            Tree::decode(std::slice::from_raw_parts(tree_ptr, tree_len))
        };
        let tree = match decoded {
            Ok(tree) => tree,
            Err(message) => {
                if !position_out.is_null() {
                    *position_out = u32::MAX;
                }
                write_error_to_buffer(
                    error_out,
                    error_out_len,
                    &format!(
                        "semantic_views: internal error: the grammar bridge sent a parse tree \
                         that does not decode ({message}; please report this bug)"
                    ),
                );
                return 1;
            }
        };
        match super::transform(&tree).and_then(crate::parse::lower_action) {
            Ok(sql) => {
                publish_owned_string(sql, sql_out_ptr, sql_out_len);
                0
            }
            Err(err) => {
                write_error_to_buffer(error_out, error_out_len, &err.message);
                publish_owned_string(err.message, sql_out_ptr, sql_out_len);
                if !position_out.is_null() {
                    *position_out = err
                        .position
                        .and_then(|p| u32::try_from(p).ok())
                        .unwrap_or(u32::MAX);
                }
                1
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::sv_grammar_transform_rust;

    /// rc=2 is thrown as an `InternalException`, which can invalidate the
    /// database; a tree that does not decode is a bridge bug, reported to the
    /// user as an ordinary parser error.
    #[test]
    fn a_tree_that_does_not_decode_is_a_parser_error() {
        let bytes = [0xffu8; 3];
        let mut out_ptr = std::ptr::null_mut();
        let mut out_len = 0usize;
        let mut error = [0u8; 256];
        let mut position = 0u32;
        let rc = unsafe {
            sv_grammar_transform_rust(
                bytes.as_ptr(),
                bytes.len(),
                &mut out_ptr,
                &mut out_len,
                error.as_mut_ptr(),
                error.len(),
                &mut position,
            )
        };
        let message = std::ffi::CStr::from_bytes_until_nul(&error)
            .unwrap()
            .to_string_lossy();
        assert_eq!(rc, 1, "{message}");
        assert!(message.contains("parse tree"), "{message}");
        assert_eq!(position, u32::MAX);
    }
}
