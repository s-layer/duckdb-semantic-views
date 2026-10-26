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
//! # Reads run in the caller's transaction (TECH-DEBT #19)
//!
//! Every dispatcher here runs inside a C++ bind or scalar callback that named
//! its caller's `ClientContext` for the duration of the call (`SvCallerScope`
//! in `cpp/src/shim.cpp`). The dispatcher reads the catalog, types members and
//! plans `EXPLAIN` through [`caller_definitions`], [`caller_describe`] and
//! [`caller_explain`], which work in that context and its transaction, so a
//! read sees what the statement around it sees: the transaction's own
//! uncommitted CREATE / DROP / RENAME / ALTER TABLE, and the session's
//! temporary tables. No connection crosses the boundary.

#![cfg(feature = "extension")]

use crate::ffi_util::wire_len;

extern "C" {
    /// Defined in `cpp/src/shim.cpp`: the interrupt flag of the `ClientContext`
    /// whose read is running on this thread (`false` outside one).
    fn sv_bind_caller_interrupted() -> bool;

    fn sv_caller_definitions(
        only_name: u8,
        name_ptr: *const u8,
        name_len: usize,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> u8;
    fn sv_caller_describe(
        database_ptr: *const u8,
        database_len: usize,
        schema_ptr: *const u8,
        schema_len: usize,
        sql_ptr: *const u8,
        sql_len: usize,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> u8;
    fn sv_caller_explain(
        database_ptr: *const u8,
        database_len: usize,
        schema_ptr: *const u8,
        schema_len: usize,
        sql_ptr: *const u8,
        sql_len: usize,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> u8;
    fn sv_caller_free(ptr: *mut u8);
}

/// Whether the caller of the read now running has been cancelled.
///
/// The binds the caller-context reads run are the caller's own, so `DuckDB`'s
/// interrupt checks inside them see its flag; this reads the same flag between
/// them, so long-running bind work (the member-type probes) stops at the next
/// one.
#[must_use]
pub fn bind_caller_interrupted() -> bool {
    // SAFETY: a plain flag read; no arguments, callable from any thread.
    unsafe { sv_bind_caller_interrupted() }
}

/// How a caller-context read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerError {
    /// The read failed; the message is `DuckDB`'s.
    Failed(String),
    /// The caller was cancelled while the read ran.
    Interrupted(String),
}

impl std::fmt::Display for CallerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(m) | Self::Interrupted(m) => f.write_str(m),
        }
    }
}

/// Run one `sv_caller_*` call and take ownership of its buffer.
fn caller_call(call: impl FnOnce(*mut *mut u8, *mut usize) -> u8) -> Result<Vec<u8>, CallerError> {
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len: usize = 0;
    let rc = call(&raw mut ptr, &raw mut len);
    let bytes = if ptr.is_null() {
        None
    } else {
        // SAFETY: on return the C++ side hands over a malloc'd buffer of `len`
        // bytes, released below with its own `free`.
        let copy = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
        unsafe { sv_caller_free(ptr) };
        Some(copy)
    };
    let message = |bytes: Option<Vec<u8>>| {
        bytes.map_or_else(
            || "out of memory reading the catalog".to_string(),
            |b| String::from_utf8_lossy(&b).into_owned(),
        )
    };
    match (rc, bytes) {
        (0, Some(bytes)) => Ok(bytes),
        (2, bytes) => Err(CallerError::Interrupted(message(bytes))),
        (_, bytes) => Err(CallerError::Failed(message(bytes))),
    }
}

/// A little-endian reader over a `sv_caller_*` payload.
struct Payload<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Payload<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| "truncated catalog payload".to_string())?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn string(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).map_err(|_| "catalog payload is not valid UTF-8".to_string())
    }

    fn nullable(&mut self) -> Result<Option<String>, String> {
        if self.u8()? == 1 {
            Ok(None)
        } else {
            self.string().map(Some)
        }
    }

    fn finish(&self) -> Result<(), String> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err("trailing bytes in catalog payload".to_string())
        }
    }
}

/// `semantic_layer._definitions` as the caller's transaction sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallerDefinitions {
    /// Whether the catalog table exists (it may not on a read-only database
    /// that never had one).
    pub present: bool,
    /// The database the table was looked for in: the managed catalog.
    pub database: String,
    /// `(schema_name, name, definition)` per row, in storage order.
    pub rows: Vec<(Option<String>, Option<String>, Option<String>)>,
}

/// Read the rows of `semantic_layer._definitions` in the caller's
/// transaction, its own uncommitted changes included: every row, or with
/// `only_name` only those whose name is that one byte for byte.
///
/// # Errors
///
/// The scan's error, or a malformed payload.
pub fn caller_definitions(only_name: Option<&str>) -> Result<CallerDefinitions, String> {
    let (filter, name) = match only_name {
        Some(name) => (1, name),
        None => (0, ""),
    };
    // SAFETY: the out-pointers are live locals of `caller_call`; `name` is
    // borrowed for the call and read as `len` bytes.
    let bytes = caller_call(|p, l| unsafe {
        sv_caller_definitions(filter, name.as_ptr(), name.len(), p, l)
    })
    .map_err(|e| e.to_string())?;
    let mut payload = Payload {
        bytes: &bytes,
        at: 0,
    };
    let present = payload.u8()? == 1;
    let database = payload.string()?;
    let mut rows = Vec::new();
    if present {
        let count = payload.u32()?;
        for _ in 0..count {
            rows.push((
                payload.nullable()?,
                payload.nullable()?,
                payload.nullable()?,
            ));
        }
    }
    payload.finish()?;
    Ok(CallerDefinitions {
        present,
        database,
        rows,
    })
}

type CallerSqlFn = unsafe extern "C" fn(
    *const u8,
    usize,
    *const u8,
    usize,
    *const u8,
    usize,
    *mut *mut u8,
    *mut usize,
) -> u8;

fn caller_sql(
    f: CallerSqlFn,
    database: Option<&str>,
    schema: Option<&str>,
    sql: &str,
) -> Result<Vec<u8>, CallerError> {
    let database = database.unwrap_or("");
    let schema = schema.unwrap_or("");
    caller_call(|p, l| {
        // SAFETY: each (ptr, len) pair is a live borrowed `str`; the C++ side
        // copies them before returning.
        unsafe {
            f(
                database.as_ptr(),
                database.len(),
                schema.as_ptr(),
                schema.len(),
                sql.as_ptr(),
                sql.len(),
                p,
                l,
            )
        }
    })
}

/// The output columns of `sql`, bound in the caller's context with its names
/// resolved as in a view stored in `database`.`schema`: `(name, type)` with the
/// type as `DESCRIBE` spells it.
///
/// # Errors
///
/// The bind's error, or the caller's cancellation.
pub fn caller_describe(
    database: Option<&str>,
    schema: Option<&str>,
    sql: &str,
) -> Result<Vec<(String, String)>, CallerError> {
    let bytes = caller_sql(sv_caller_describe, database, schema, sql)?;
    let malformed = CallerError::Failed;
    let mut payload = Payload {
        bytes: &bytes,
        at: 0,
    };
    let count = payload.u32().map_err(malformed)?;
    let mut out = Vec::new();
    for _ in 0..count {
        let name = payload.string().map_err(malformed)?;
        let data_type = payload.string().map_err(malformed)?;
        out.push((name, data_type));
    }
    payload.finish().map_err(malformed)?;
    Ok(out)
}

/// The physical plan `EXPLAIN` shows for `sql`, planned in the caller's context
/// with its names resolved as in a view stored in `database`.`schema`.
///
/// # Errors
///
/// The planner's error, or the caller's cancellation.
pub fn caller_explain(
    database: Option<&str>,
    schema: Option<&str>,
    sql: &str,
) -> Result<String, CallerError> {
    let bytes = caller_sql(sv_caller_explain, database, schema, sql)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Type `wanted`'s members of `def` for a SHOW / DESCRIBE bind, in the caller's
/// context (TECH-DEBT #19, #51). A member that cannot be typed is simply absent;
/// the only error is the caller's cancellation, which stops probing at once.
///
/// # Errors
///
/// `Interrupted!` when the bind's caller was cancelled mid-probe. The C++ bind
/// helpers raise that as a bare `InterruptException`.
pub fn member_types_for_bind(
    view_name: &str,
    def: &crate::model::SemanticViewDefinition,
    wanted: crate::query::member_types::Wanted,
) -> Result<crate::query::member_types::MemberTypes, String> {
    use crate::query::member_types::{ProbeError, ProbedColumn};
    let (database, schema) = crate::expand::body_qualifier(def);
    Ok(crate::query::member_types::infer_member_types(
        view_name,
        def,
        wanted,
        &bind_caller_interrupted,
        &mut |sql| match caller_describe(database, schema, sql) {
            Ok(cols) => Ok(cols
                .into_iter()
                .map(|(name, data_type)| ProbedColumn { name, data_type })
                .collect()),
            Err(CallerError::Interrupted(_)) => Err(ProbeError::Interrupted),
            Err(CallerError::Failed(m)) => Err(ProbeError::Failed(m)),
        },
    )?)
}

/// Whether the caller's catalog holds a semantic view `name` in `schema`, as
/// the caller's transaction sees it. The CREATE helpers ask before binding the
/// members: a CREATE that will not write (IF NOT EXISTS on a view that exists,
/// or a plain CREATE that will report "already exists") has nothing to check.
///
/// # Errors
///
/// The catalog scan's error.
pub fn view_present(name: &str, schema: &str) -> Result<bool, String> {
    let defs = caller_definitions(Some(name))?;
    Ok(defs.present
        && defs.rows.iter().any(|(row_schema, row_name, _)| {
            row_name.as_deref() == Some(name)
                && row_schema
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(schema))
        }))
}

/// Refuse a CREATE whose definition has a member `DuckDB` cannot bind, as
/// Snowflake compiles every member expression at CREATE.
///
/// `definition` is the JSON `enrich_definition_for_create` produced. It is
/// probed, without its materializations, as the stored view will be expanded:
/// its names resolve in
/// `database`.`schema` (the caller's current database and schema, which the
/// CREATE stamps), and a table written without a schema that is a temporary
/// table or view in the caller's session is that one (the INSERT records the
/// same set as `temp_tables`). The probes run in the caller's context, so they
/// see the caller's transaction.
///
/// # Errors
///
/// `semantic view '<name>': <kind> '<member>' does not bind: <DuckDB's error>`
/// for the first member that does not bind; `Interrupted!` when the caller was
/// cancelled, which the C++ helper raises as a bare `InterruptException`.
pub fn check_create_members(
    view_name: &str,
    definition: &str,
    database: &str,
    schema: &str,
) -> Result<(), String> {
    use crate::query::member_types::{ProbeError, ProbedColumn};
    let mut def: crate::model::SemanticViewDefinition =
        serde_json::from_str(definition).map_err(|e| format!("internal error: {e}"))?;
    def.database_name = Some(database.to_string());
    def.schema_name = Some(schema.to_string());
    def.resolution_schema_name = Some(schema.to_string());
    // A member is bound over the view's own tables. A request a
    // materialization covers would be routed to its table instead, which need
    // not exist yet and says nothing about the member's expression.
    def.materializations.clear();
    let (database, schema) = (Some(database), Some(schema));
    let mut temp = Vec::new();
    for name in std::mem::take(&mut def.temp_tables) {
        let probe = format!(
            "SELECT * FROM \"temp\".\"main\".{} LIMIT 0",
            crate::expand::quote_ident(&name)
        );
        match caller_describe(database, schema, &probe) {
            Ok(_) => temp.push(name),
            Err(CallerError::Interrupted(m)) => return Err(m),
            Err(CallerError::Failed(_)) => {}
        }
    }
    def.temp_tables = temp;
    let unbound = crate::query::member_types::first_unbound_member(
        view_name,
        &def,
        &bind_caller_interrupted,
        &mut |sql| match caller_describe(database, schema, sql) {
            Ok(cols) => Ok(cols
                .into_iter()
                .map(|(name, data_type)| ProbedColumn { name, data_type })
                .collect()),
            Err(CallerError::Interrupted(_)) => Err(ProbeError::Interrupted),
            Err(CallerError::Failed(m)) => Err(ProbeError::Failed(m)),
        },
    )?;
    match unbound {
        None => Ok(()),
        Some(u) => Err(format!(
            "semantic view '{view_name}': {} '{}' does not bind: {}",
            u.kind, u.name, u.error
        )),
    }
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
/// `catch_unwind` guard, publishing the success buffer, writing the error string, and the panic arm. The caller's
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
/// `out_ptr`/`out_len` are the
/// C++ out-parameters for the published buffer; `error_buf`/`error_buf_len`
/// the C++ diagnostic slot. `panic_label` names the dispatcher for the panic
/// message.
pub unsafe fn run_dispatcher<F>(
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
    panic_label: &str,
    body: F,
) -> u8
where
    F: FnOnce() -> Result<Vec<u8>, String>,
{
    // AssertUnwindSafe mirrors the per-dispatcher wrapper this replaces: the
    // captured raw pointers are not `UnwindSafe`, but the catch_unwind here is
    // purely to convert a panic into rc=2 — no state is observed after unwind.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match body() {
        Ok(buf) => {
            publish_owned_buffer(buf, out_ptr, out_len);
            0_u8
        }
        Err(msg) => {
            write_err(error_buf, error_buf_len, &msg);
            1_u8
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
