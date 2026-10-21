//! Shared FFI helpers for the Phase 65 Plan 05 read-path migrations.
//!
//! Plan 05 Task 1 (Wave 0 bridge spike) established the wire-format
//! convention for handing serialized result rows from the Rust dispatcher to
//! the C++ bind callback (see `src/ddl/list.rs::sv_list_semantic_views_bind_rust`).
//! As the remaining 16 read-side migrations adopt the same pattern, this
//! module centralises the reusable pieces so each per-function dispatcher
//! shrinks to "collect rows → serialize → return".
//!
//! # Wire format (length-prefixed binary, little-endian)
//!
//! ```text
//! u32 col_count                 ─┐ self-describing schema header (AR-3)
//! col_count × u8 type_tag        │ 1 = VARCHAR, 2 = BOOLEAN
//!                               ─┘
//! u32 row_count
//! for each row:
//!   for each column (in type_tag order):
//!     VARCHAR: u32 byte_len + byte_len bytes (UTF-8 payload)
//!     BOOLEAN: u8 (1 = TRUE, 0 = FALSE)
//! ```
//!
//! ## Self-describing schema header (AR-3)
//!
//! The payload opens with a schema header — the column count followed by one
//! type tag per column. This makes the column layout *self-describing* rather
//! than agreed out-of-band: the C++ bind reads the header and asserts it
//! against the column set it declared (count, and per-column VARCHAR/BOOLEAN),
//! turning a former convention-guarded coupling (the reintroduction of
//! TECH-DEBT #12's two-place schema agreement) into a machine-checked one. A
//! dispatcher that emits a different column shape than its C++ bind expects
//! now fails loudly at the header with a clear message, instead of desyncing
//! silently or misparsing cell bytes. The C++ side parses the payload with
//! `sv_read_u32_le` / `sv_read_string` (plus `sv_read_wire_schema` for the
//! header) in `cpp/src/shim.cpp`, then emits rows into the `DataChunk`.
//!
//! The header is emitted for every non-empty result. An empty result set
//! (`row_count == 0`) writes `col_count == 0` and no tags: there are no cells
//! to misalign, so the C++ side skips the schema assertion in that case.
//!
//! ## Variant: VARCHAR with a trailing BOOLEAN column
//!
//! `show_semantic_dimensions_for_metric` returns 3 VARCHAR + 1 BOOLEAN. Its
//! schema header carries tags `[VARCHAR, VARCHAR, VARCHAR, BOOLEAN]`; each
//! row emits the VARCHAR cells first, then a single trailing `u8` for the
//! BOOLEAN (1 = TRUE, 0 = FALSE). C++ side parses with `sv_read_u8` after the
//! string reads.
//!
//! # Borrow contract (critical)
//!
//! Every dispatcher in this module receives a `duckdb_connection` BORROWED
//! from a stack `Connection probe(*context.db)` constructed by the C++ bind
//! callback. The Rust side MUST NOT call `duckdb_disconnect` on the handle
//! — that would `delete` a stack object (UB). The C++ bind scope's
//! `~Connection()` handles teardown.

#![cfg(feature = "extension")]

use crate::ffi_util::wire_len;
use libduckdb_sys as ffi;
use std::ffi::{CStr, CString};

/// Borrowed `duckdb_connection` handle. The bridge contract: this handle is
/// owned by a stack `Connection probe(*context.db)` constructed in a C++
/// bind/exec callback. The Rust side MUST NOT call `duckdb_disconnect` —
/// doing so would `delete` a stack object (UB).
///
/// This newtype enforces the contract at compile time (Phase 65.1 D-10 /
/// WR-05): `ffi::duckdb_disconnect` accepts `*mut ffi::duckdb_connection`,
/// not `*mut BorrowedConnection`, so the call simply does not type-check.
/// Even though `BorrowedConnection` is `#[repr(transparent)]` wrapping
/// `duckdb_connection`, Rust newtype distinctness blocks the coercion at
/// the type level. Defence in depth alongside the AST-walk guard at
/// `tests/no_long_lived_conn.rs`.
///
/// Construct via the unsafe [`BorrowedConnection::new`] constructor at the
/// FFI boundary; access the raw handle via [`BorrowedConnection::as_raw`]
/// only for further FFI calls (`duckdb_query`, `duckdb_prepare`, ...).
///
/// # Negative compile coverage
///
/// The following snippet must fail to compile because `duckdb_disconnect`
/// takes `*mut duckdb_connection`, and `&mut BorrowedConnection` cannot be
/// coerced to that type — exactly the regression this newtype guards
/// against. The doctest uses a mutable binding so the failure is the
/// intended type mismatch, NOT an immutable-binding error.
///
/// ```compile_fail
/// use semantic_views::ddl::read_ffi::BorrowedConnection;
/// let mut b: BorrowedConnection = unsafe { std::mem::zeroed() };
/// // duckdb_disconnect takes *mut duckdb_connection. &mut BorrowedConnection
/// // is NOT *mut duckdb_connection, even though BorrowedConnection is
/// // #[repr(transparent)] wrapping duckdb_connection.
/// unsafe { libduckdb_sys::duckdb_disconnect(&mut b) };
/// ```
#[repr(transparent)]
pub struct BorrowedConnection(ffi::duckdb_connection);

impl BorrowedConnection {
    /// Wrap a raw FFI handle as a borrowed connection.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `conn` outlives this `BorrowedConnection`
    /// and that no other code path will call `duckdb_disconnect(conn)` while
    /// this borrow is live. Typical usage: construct immediately on entry to
    /// an `extern "C"` dispatcher from the raw `conn` parameter passed by the
    /// C++ bind/exec callback.
    #[must_use]
    pub unsafe fn new(conn: ffi::duckdb_connection) -> Self {
        Self(conn)
    }

    /// Access the underlying raw handle for further FFI calls
    /// (`duckdb_query`, `duckdb_prepare`, etc.). The caller MUST NOT pass
    /// the returned handle to `duckdb_disconnect`.
    #[must_use]
    pub fn as_raw(&self) -> ffi::duckdb_connection {
        self.0
    }

    /// Whether the wrapped handle is null. Cheaper than constructing a
    /// `CatalogReader` just to discover the bind callback handed us a
    /// null connection.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }
}
// Deliberately NO Drop, NO Clone, NO Copy, NO trait impls that could lead
// to disconnect — the type-level guard depends on the call surface staying
// minimal. Do not add `query()` / `prepare()` convenience methods here;
// they would encourage passing BorrowedConnection around without
// unwrapping intent at each FFI boundary.

extern "C" {
    /// Defined in `cpp/src/shim.cpp`: the interrupt flag of the `ClientContext`
    /// whose table-function bind is running on this thread (`false` outside one).
    fn sv_bind_caller_interrupted() -> bool;
}

/// Whether the caller of the table function now binding has been cancelled.
///
/// A bind dispatcher runs on a fresh `Connection(*context.db)` with its own
/// interrupt flag, which `duckdb_interrupt` on the caller never sets; this reads
/// the caller's flag instead, so long-running bind work (the member-type probes)
/// can stop between statements.
#[must_use]
pub fn bind_caller_interrupted() -> bool {
    // SAFETY: a plain flag read; no arguments, callable from any thread.
    unsafe { sv_bind_caller_interrupted() }
}

/// Type `wanted`'s members of `def` for a SHOW / DESCRIBE bind on this bind's
/// connection (TECH-DEBT #51). A member that cannot be typed is simply absent;
/// the only error is the caller's cancellation, which stops probing at once.
///
/// # Errors
///
/// `Interrupted!` when the bind's caller was cancelled mid-probe. The C++ bind
/// helpers raise that as a bare `InterruptException`.
pub fn member_types_for_bind(
    borrowed: &BorrowedConnection,
    view_name: &str,
    def: &crate::model::SemanticViewDefinition,
    wanted: crate::query::member_types::Wanted,
) -> Result<crate::query::member_types::MemberTypes, String> {
    // SAFETY: `borrowed` is the live per-call connection of this bind.
    Ok(unsafe {
        crate::query::member_types::infer_member_types_on(
            borrowed.as_raw(),
            view_name,
            def,
            wanted,
            &bind_caller_interrupted,
        )
    }?)
}

/// Probe whether `semantic_layer._definitions` exists on the given borrowed
/// connection.
///
/// Returns:
/// * `Ok(true)` — the catalog table is present.
/// * `Ok(false)` — the probe query succeeded but the table is absent (0 rows).
///   This is the Phase 63 read-only short-circuit path: an attached read-only
///   DB whose `_definitions` table was never bootstrapped returns 0 rows here,
///   which callers treat as "no views", not an error. Mirrors the read-only
///   short-circuit logic at `src/lib.rs` and the inline probe in
///   `src/ddl/list.rs`.
/// * `Err(msg)` — the probe query itself failed to execute (FF-9). Previously
///   any such failure was silently folded into `false` ("no views"), masking
///   catalog corruption or a broken connection as an empty result. Callers now
///   surface it as an error distinct from genuine absence.
///
/// # Safety
///
/// `borrowed` must wrap a valid `duckdb_connection`. The handle is borrowed
/// and must outlive this call — the typical caller is a bind dispatcher
/// running inside a C++ bind callback that owns a stack `Connection
/// probe(*context.db)`.
pub unsafe fn probe_catalog_table_present(borrowed: &BorrowedConnection) -> Result<bool, String> {
    let conn = borrowed.as_raw();
    let sql = CString::new(
        "SELECT 1 FROM information_schema.tables \
         WHERE table_schema = 'semantic_layer' AND table_name = '_definitions' LIMIT 1",
    )
    .map_err(|_| "catalog probe SQL contains an interior null byte".to_string())?;
    let mut result: ffi::duckdb_result = std::mem::zeroed();
    let rc = ffi::duckdb_query(conn, sql.as_ptr(), &raw mut result);
    let out = if rc == ffi::DuckDBSuccess {
        Ok(ffi::duckdb_row_count(&raw mut result) > 0)
    } else {
        let err_ptr = ffi::duckdb_result_error(&raw mut result);
        let msg = if err_ptr.is_null() {
            "catalog presence probe failed".to_string()
        } else {
            CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
        };
        Err(msg)
    };
    ffi::duckdb_destroy_result(&raw mut result);
    out
}

/// Write a NUL-terminated error message into the C-side `error_buf`,
/// truncating to at most `buf_len - 1` payload bytes on a UTF-8 char
/// boundary. Thin alias for the shared [`crate::ffi_util::write_error_to_buffer`]
/// (ST-4 consolidation) kept for the ~100 dispatcher call sites; the local
/// copy it replaced truncated mid-codepoint, producing invalid UTF-8 in
/// `BinderException` text (FF-5).
///
/// # Safety
///
/// `buf` must be either null OR point to writable storage of at least
/// `buf_len` bytes.
pub unsafe fn write_err(buf: *mut u8, buf_len: usize, msg: &str) {
    crate::ffi_util::write_error_to_buffer(buf, buf_len, msg);
}

/// Column type tag in the self-describing schema header (AR-3). Kept in sync
/// with the `SV_WIRE_VARCHAR` / `SV_WIRE_BOOL` constants in `cpp/src/shim.cpp`
/// — the C++ bind asserts the received tags against the column set it declared.
const WIRE_TAG_VARCHAR: u8 = 1;
/// See [`WIRE_TAG_VARCHAR`].
const WIRE_TAG_BOOL: u8 = 2;

/// Write the self-describing schema header (AR-3): `u32 col_count` followed by
/// one `u8` type tag per column. Emitting the header for every non-empty
/// payload lets the C++ bind assert the column layout it declared matches what
/// the dispatcher actually produced, rather than trusting an out-of-band
/// agreement.
fn write_wire_schema(buf: &mut Vec<u8>, tags: &[u8]) -> Result<(), String> {
    let col_count = wire_len(tags.len(), "column count")?;
    buf.extend_from_slice(&col_count.to_le_bytes());
    buf.extend_from_slice(tags);
    Ok(())
}

/// Serialize a vector of VARCHAR rows into the wire format described above.
///
/// `rows` is a `Vec<Vec<String>>` where every inner Vec must have the same
/// length (number of columns). The column count for the self-describing header
/// (AR-3) is taken from the first row; an empty row set emits a zero-column
/// header (the C++ side skips its schema assertion then). A ragged row set is
/// rejected: the header would otherwise advertise the first row's column count
/// while the payload bytes carry a different one, desyncing the C++ parser.
///
/// Returns `Err` if the rows are non-rectangular, or if the row count or any
/// cell length overflows the wire format's `u32` fields (see [`wire_len`]).
pub fn serialize_varchar_rows(rows: &[Vec<String>]) -> Result<Vec<u8>, String> {
    let n_cols = rows.first().map_or(0, Vec::len);
    if let Some(bad) = rows.iter().position(|r| r.len() != n_cols) {
        return Err(format!(
            "non-rectangular row set: row {bad} has {} columns, expected {n_cols}",
            rows[bad].len()
        ));
    }
    let cap = 4
        + n_cols
        + 4
        + rows
            .iter()
            .map(|r| r.iter().map(|s| 4 + s.len()).sum::<usize>())
            .sum::<usize>();
    let mut buf = Vec::with_capacity(cap);
    write_wire_schema(&mut buf, &vec![WIRE_TAG_VARCHAR; n_cols])?;
    let row_count = wire_len(rows.len(), "row count")?;
    buf.extend_from_slice(&row_count.to_le_bytes());
    for row in rows {
        for col in row {
            let len = wire_len(col.len(), "cell")?;
            buf.extend_from_slice(&len.to_le_bytes());
            buf.extend_from_slice(col.as_bytes());
        }
    }
    Ok(buf)
}

/// Serialize a vector of (VARCHAR-cells, BOOL) rows. Each row's strings are
/// emitted first (same shape as `serialize_varchar_rows`) followed by a
/// single trailing `u8` (1 = TRUE, 0 = FALSE).
///
/// The self-describing header (AR-3) carries the VARCHAR tags followed by a
/// trailing BOOLEAN tag; the VARCHAR column count is taken from the first
/// row's string vector. An empty row set emits a zero-column header. A ragged
/// row set (rows whose VARCHAR-cell counts differ) is rejected for the same
/// reason as [`serialize_varchar_rows`] — the header would disagree with the
/// payload bytes.
///
/// Returns `Err` on non-rectangular rows or the same overflow conditions as
/// [`serialize_varchar_rows`].
pub fn serialize_varchar_bool_rows(rows: &[(Vec<String>, bool)]) -> Result<Vec<u8>, String> {
    let n_varchar = rows.first().map_or(0, |(strs, _)| strs.len());
    if let Some(bad) = rows.iter().position(|(strs, _)| strs.len() != n_varchar) {
        return Err(format!(
            "non-rectangular row set: row {bad} has {} VARCHAR columns, expected {n_varchar}",
            rows[bad].0.len()
        ));
    }
    let cap = 4
        + (n_varchar + 1)
        + 4
        + rows
            .iter()
            .map(|(strs, _)| strs.iter().map(|s| 4 + s.len()).sum::<usize>() + 1)
            .sum::<usize>();
    let mut buf = Vec::with_capacity(cap);
    if rows.is_empty() {
        // No rows → zero-column header (C++ skips the schema assertion).
        write_wire_schema(&mut buf, &[])?;
    } else {
        let mut tags = vec![WIRE_TAG_VARCHAR; n_varchar];
        tags.push(WIRE_TAG_BOOL);
        write_wire_schema(&mut buf, &tags)?;
    }
    let row_count = wire_len(rows.len(), "row count")?;
    buf.extend_from_slice(&row_count.to_le_bytes());
    for (strs, b) in rows {
        for col in strs {
            let len = wire_len(col.len(), "cell")?;
            buf.extend_from_slice(&len.to_le_bytes());
            buf.extend_from_slice(col.as_bytes());
        }
        buf.push(u8::from(*b));
    }
    Ok(buf)
}

/// Hand a heap-owned `Vec<u8>` to the C++ side via the (ptr, len)
/// out-parameters. The C++ side MUST release the buffer with
/// `sv_free_buffer(ptr, len)` using the exact `(ptr, len)` pair this function
/// returns.
///
/// Thin alias for the shared [`crate::ffi_util::publish_owned_bytes`] (ST-4
/// consolidation), which uses the both-or-drop contract: if either
/// out-pointer is null the buffer is dropped and neither slot is written.
/// The local copy it replaced leaked the buffer and could desync
/// `(ptr, len)` by writing only one slot.
///
/// # Safety
///
/// Either both `out_ptr` and `out_len` point to writable slots, or the call
/// is treated as "drop and skip writing".
pub unsafe fn publish_owned_buffer(buf: Vec<u8>, out_ptr: *mut *mut u8, out_len: *mut usize) {
    crate::ffi_util::publish_owned_bytes(buf, out_ptr, out_len);
}

/// Shared scaffold for the read-side bind/exec dispatchers (ST-2).
///
/// Owns the boilerplate every dispatcher repeated verbatim: the
/// `catch_unwind` guard, the borrowed-connection null check, publishing the
/// success buffer, writing the error string, and the panic arm. The caller's
/// `body` does only the interesting work — parse args, read the catalog,
/// assemble + serialize rows — and returns the serialized wire buffer on
/// success or an error message.
///
/// Return-code contract (unchanged from the hand-written dispatchers):
/// `0` = success (buffer published), `1` = handled error (message in
/// `error_buf`), `2` = panic (message in `error_buf`).
///
/// # Safety
///
/// `conn` is a borrowed handle (see module-level borrow contract); it must
/// outlive the call and must not be disconnected. `out_ptr`/`out_len` are the
/// C++ out-parameters for the published buffer; `error_buf`/`error_buf_len`
/// the C++ diagnostic slot. `panic_label` names the dispatcher for the panic
/// message.
pub unsafe fn run_dispatcher<F>(
    conn: ffi::duckdb_connection,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
    panic_label: &str,
    body: F,
) -> u8
where
    F: FnOnce(&BorrowedConnection) -> Result<Vec<u8>, String>,
{
    // AssertUnwindSafe mirrors the per-dispatcher wrapper this replaces: the
    // captured raw pointers are not `UnwindSafe`, but the catch_unwind here is
    // purely to convert a panic into rc=2 — no state is observed after unwind.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let borrowed = BorrowedConnection::new(conn);
        if borrowed.is_null() {
            write_err(error_buf, error_buf_len, "duckdb_connection is null");
            return 1_u8;
        }
        match body(&borrowed) {
            Ok(buf) => {
                publish_owned_buffer(buf, out_ptr, out_len);
                0_u8
            }
            Err(msg) => {
                write_err(error_buf, error_buf_len, &msg);
                1_u8
            }
        }
    }));
    if let Ok(rc) = result {
        rc
    } else {
        write_err(
            error_buf,
            error_buf_len,
            &format!("internal error: panic inside {panic_label}"),
        );
        2
    }
}

/// Decode a `(ptr, len)` string argument passed from the C++ side, checking
/// for a null pointer and valid UTF-8 (ST-2). `what` names the argument for
/// the error message (e.g. `"view name"` → `"view name pointer is null"` /
/// `"view name is not valid UTF-8"`), matching the wording the hand-written
/// dispatchers used.
///
/// # Safety
///
/// If non-null, `ptr` must point to `len` readable bytes for the duration of
/// the call.
pub unsafe fn read_str_arg(ptr: *const u8, len: usize, what: &str) -> Result<String, String> {
    if ptr.is_null() {
        return Err(format!("{what} pointer is null"));
    }
    match std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => Err(format!("{what} is not valid UTF-8")),
    }
}

/// Decode the caller's schema resolution order from the shared varchar-list
/// wire format — the same encoding the `dimensions` / `metrics` / `facts`
/// named parameters use.
///
/// A null or empty buffer yields an empty path, which
/// [`crate::catalog::resolve_in_search_path`] falls back on (an explicit
/// `search_path := []`, or a v1.5 build that cannot read the caller's path).
/// That has to stay working, so absence is not an error.
///
/// # Safety
///
/// `ptr` must either be null (with `len == 0`) or point to `len` readable
/// bytes in the varchar-list wire format.
#[cfg(feature = "extension")]
pub unsafe fn read_search_path(ptr: *const u8, len: usize) -> Result<Vec<String>, String> {
    crate::query::wire::parse_varchar_list(ptr, len)
        .map_err(|detail| format!("malformed `search_path` payload: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_empty_row_set() {
        // Empty result → zero-column schema header, then row_count = 0.
        let buf = serialize_varchar_rows(&[]).unwrap();
        assert_eq!(
            buf,
            vec![
                0, 0, 0, 0, // col_count = 0 (no tags follow)
                0, 0, 0, 0, // row_count = 0
            ]
        );
    }

    #[test]
    fn serialize_single_row() {
        let rows = vec![vec!["a".to_string(), "bc".to_string()]];
        let buf = serialize_varchar_rows(&rows).unwrap();
        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            2, 0, 0, 0, // col_count = 2
            WIRE_TAG_VARCHAR, WIRE_TAG_VARCHAR, // tags: [VARCHAR, VARCHAR]
            1, 0, 0, 0, // row_count = 1
            1, 0, 0, 0,    // len("a") = 1
            b'a', // "a"
            2, 0, 0, 0, // len("bc") = 2
            b'b', b'c', // "bc"
        ];
        assert_eq!(buf, expected);
    }

    #[test]
    fn serialize_bool_suffix() {
        let rows = vec![
            (vec!["x".to_string()], true),
            (vec!["y".to_string()], false),
        ];
        let buf = serialize_varchar_bool_rows(&rows).unwrap();
        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            2, 0, 0, 0, // col_count = 2 (1 VARCHAR + trailing BOOL)
            WIRE_TAG_VARCHAR, WIRE_TAG_BOOL, // tags: [VARCHAR, BOOL]
            2, 0, 0, 0, // row_count = 2
            1, 0, 0, 0, b'x', 1, // ("x", true)
            1, 0, 0, 0, b'y', 0, // ("y", false)
        ];
        assert_eq!(buf, expected);
    }

    #[test]
    fn serialize_bool_empty_row_set() {
        // Empty bool-variant result → zero-column header, then row_count = 0.
        let buf = serialize_varchar_bool_rows(&[]).unwrap();
        assert_eq!(buf, vec![0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn serialize_varchar_rows_rejects_ragged() {
        // Second row has a different column count than the first — the schema
        // header (derived from row 0) would disagree with the payload bytes.
        let rows = vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["c".to_string()],
        ];
        let err = serialize_varchar_rows(&rows).unwrap_err();
        assert!(err.contains("non-rectangular"), "unexpected error: {err}");
        assert!(err.contains("row 1"), "unexpected error: {err}");
    }

    #[test]
    fn serialize_varchar_bool_rows_rejects_ragged() {
        let rows = vec![
            (vec!["a".to_string(), "b".to_string()], true),
            (vec!["c".to_string()], false),
        ];
        let err = serialize_varchar_bool_rows(&rows).unwrap_err();
        assert!(err.contains("non-rectangular"), "unexpected error: {err}");
        assert!(err.contains("row 1"), "unexpected error: {err}");
    }
}
