//! FFI entry points for the semantic-view `parser_override` path (AR-1).
//!
//! These are the C-ABI symbols the C++ shim (`cpp/src/shim.cpp`) links
//! against: the parser-override hook (`sv_parser_override_rust`), the
//! parse-function validation hook (`sv_parse_function_rust`), and the
//! output-buffer reclaimer (`sv_free_buffer`). The
//! `run_validation_for_parse_function` bridge and the `publish_owned_sql`
//! helper live here too.
//!
//! AR-7: the empty `OverrideContext` lifecycle
//! (`sv_make_override_context` / `sv_drop_override_context` + the opaque
//! `ctx_ptr` threaded through the two hooks) was retired. It became a pure
//! no-op after Phase 65 Plan 06 moved the catalog pre-checks into the
//! emitted SQL, so the struct, its allocator/deallocator, and the
//! `SemanticViewsParserInfo::rust_state` round-trip carried no state.
//!
//! All of this is `unsafe` C-boundary plumbing, isolated from the pure parse
//! logic in the sibling modules. The hooks delegate to `plan_rewrite`
//! (syntax) and `rewrite_to_native_sql` (catalog-aware emission); buffer
//! handling goes through `crate::ffi_util`. `sv_free_buffer` is re-exported
//! from the parent module so `crate::parse::*` paths (and cross-module tests)
//! resolve unchanged.

#[cfg(feature = "extension")]
use super::rewrite_to_native_sql;
#[cfg(any(feature = "extension", test))]
use super::{detect_ddl_kind, detect_near_miss};
// `plan_rewrite` only backs the syntax-only `run_validation_for_parse_function`
// used in non-extension unit tests; the extension build routes through
// `rewrite_to_native_sql` instead, so importing it there is unused.
#[cfg(all(not(feature = "extension"), test))]
use super::plan_rewrite;
#[cfg(any(feature = "extension", test))]
use crate::errors::ParseError;

// ---------------------------------------------------------------------------
// FFI entry points (extension feature-gated)
// ---------------------------------------------------------------------------
//
// AR-7 (was Phase 65 Plan 06): the `OverrideContext` struct and its
// `sv_make_override_context` / `sv_drop_override_context` allocator pair are
// gone. Pre-Plan-06 the context carried a `CatalogReader` over a long-lived
// `duckdb_connection` used for `catalog.exists()` pre-checks; Plan 06 replaced
// those with pure-SQL guards (`SELECT CASE WHEN [NOT] EXISTS THEN error(...)
// ELSE TRUE END; <DML>`) that run on the caller's connection in the same
// transaction. The struct then held no state, so the hooks no longer take an
// opaque `ctx_ptr` and the C++ `SemanticViewsParserInfo` no longer round-trips
// a `rust_state` pointer.

// The error writer, buffer leak/reclaim, and publish helpers all live in
// `crate::ffi_util` (ST-4 consolidation) — this module used to carry its
// own copies, which was how the FF-5 truncation divergence happened.
// Convention: `write_error_to_buffer` only for short, bounded strings
// (error messages); unboundedly large outputs (rewritten SQL) go through
// `publish_owned_string` + `sv_free_buffer` — silently truncating SQL
// produced confusing downstream parser errors (v0.8.0 buffer-truncation
// fix).
#[cfg(any(feature = "extension", test))]
use crate::ffi_util::write_error_to_buffer;

#[cfg(feature = "extension")]
use crate::ffi_util::reclaim_c_buffer;

/// FFI export: free a heap buffer produced by an earlier
/// `sv_parser_override_rust` success return.
///
/// Safe to call with a null pointer (no-op).
///
/// # Safety
///
/// `ptr`/`len` must be the exact pair the Rust side returned via its
/// `sql_out_ptr` / `sql_out_len` out-parameters. Calling with any other
/// pair (or twice on the same pair) is undefined behaviour.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_free_buffer(ptr: *mut u8, len: usize) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reclaim_c_buffer(ptr, len);
    }));
}

/// Internal helper: publish an owned `String` to the FFI out-parameters.
/// Both-or-drop contract (see [`crate::ffi_util::publish_owned_bytes`]).
///
/// # Safety
///
/// Either both `sql_out_ptr` and `sql_out_len` must point to writable
/// `*mut u8` / `usize` slots, or both must be null. Mixed null is treated
/// as "drop and skip writing."
#[cfg(feature = "extension")]
unsafe fn publish_owned_sql(sql: String, sql_out_ptr: *mut *mut u8, sql_out_len: *mut usize) {
    crate::ffi_util::publish_owned_string(sql, sql_out_ptr, sql_out_len);
}

/// FFI entry point for `parser_override`. The sole DDL entry point for the
/// extension as of v0.8.0 — the legacy `parse_function/parse_stub` path was
/// retired. Rewrites recognized semantic-view DDL into native SQL suitable
/// for re-parsing through `DuckDB`'s own parser and execution on the caller's
/// connection.
///
/// Returns:
///   0 = success: heap-owned native SQL pointer + length written to
///       `*sql_out_ptr` / `*sql_out_len`. Caller takes ownership and must
///       release via `sv_free_buffer`. The buffer is **not** NUL-terminated;
///       read exactly `*sql_out_len` bytes.
///   1 = validation error / near-miss suggestion: error message written to
///       `error_out`. (Currently unused under `FALLBACK_OVERRIDE`; kept for
///       Phase 62 Plan 03 once `parse_function` returns to caret rendering.)
///   2 = not ours: defer to default parser. Used both for genuinely
///       non-semantic SQL and for the early-return on null/empty input or
///       invalid UTF-8.
///
/// # Safety
///
/// - `query_ptr` must point to bytes of length `query_len` (validated as
///   UTF-8 here; invalid UTF-8 returns 2 rather than triggering UB).
/// - `sql_out_ptr` must point to a writable `*mut u8` slot, or be null.
/// - `sql_out_len` must point to a writable `usize` slot, or be null.
/// - `error_out` must point to a writable buffer of `error_out_len` bytes.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_parser_override_rust(
    query_ptr: *const u8,
    query_len: usize,
    sql_out_ptr: *mut *mut u8,
    sql_out_len: *mut usize,
    error_out: *mut u8,
    error_out_len: usize,
) -> u8 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if query_ptr.is_null() || query_len == 0 {
            return 2_u8; // not ours
        }
        // Reject invalid UTF-8 cleanly rather than relying on
        // from_utf8_unchecked (B2 hardening). DuckDB query strings are
        // UTF-8 by spec but a malformed input must not trigger UB.
        let bytes = std::slice::from_raw_parts(query_ptr, query_len);
        let Ok(query) = std::str::from_utf8(bytes) else {
            return 2; // not ours — defer
        };

        let outcome = rewrite_to_native_sql(query);
        // Remember a multi-statement string so that parse_function can name
        // the real cause (TECH-DEBT #85). Noted on every call, rewritten or
        // not: a string whose read call gets the search path injected (rc=0)
        // still fails when the plain re-parse below meets its DDL. A DDL
        // rewrite, though, took the whole string as ONE statement, whatever
        // `;` it holds in a literal or comment -- nothing to remember.
        let one_ddl_statement = matches!(outcome, Ok(Some(_))) && detect_ddl_kind(query).is_some();
        note_override_query(if one_ddl_statement { "" } else { query });
        match outcome {
            Ok(Some(sql)) => {
                publish_owned_sql(sql, sql_out_ptr, sql_out_len);
                0 // success — native SQL handed to caller
            }
            Ok(None) => {
                // Genuinely not ours — defer to the default parser. If the
                // input is a near-miss for one of our DDL prefixes (e.g.
                // `CRETAE SEMANTIC VIEW`), `parse_function` (registered
                // alongside `parser_override` from Phase 62 Plan 03 onward)
                // will pick this up after the default parser fails on the
                // unrecognised prefix and re-render the suggestion via
                // DISPLAY_EXTENSION_ERROR with caret position.
                let _ = (error_out, error_out_len); // unused under Phase 62
                2 // not ours, defer to default parser
            }
            Err(err) => {
                // Phase 62: defer to default parser → `sv_parse_stub`
                // (registered as `parse_function`) re-runs validation and
                // returns DISPLAY_EXTENSION_ERROR with caret position. The
                // synthesised `SELECT error('...')` workaround used in
                // v0.8.0 (sql_throwing) has been deleted now that DuckDB's
                // ParserException::SyntaxError caret rendering is reachable
                // again via the parse_function code path. Resolves
                // TECH-DEBT 22.
                //
                // Phase 65.1 WR-06: preserve the original Err message in
                // `error_out` even though the C++ side currently ignores
                // the rc=2 channel. `rewrite_to_native_sql` can fail in
                // non-deterministic ways (e.g. JSON serialisation, the
                // metadata-via-SQL now() rendering, the unreachable
                // 'internal error' dispatch paths). If the second
                // invocation under sv_parse_stub produces a different
                // error than this one — because catalog state changed
                // mid-call, or a transient panic-caught failure
                // reproduces differently — the user otherwise loses any
                // trace of the first failure. The buffer is heap-allocated
                // and the cost is sub-microsecond; keeping the channel
                // populated lets future tooling (debug-build flag,
                // tracing hook) surface both errors without another ABI
                // change. Safety: error_out_len is the caller's declared
                // capacity; write_error_to_buffer respects it and
                // truncates at a char boundary.
                if !error_out.is_null() && error_out_len > 0 {
                    write_error_to_buffer(error_out, error_out_len, &err.message);
                }
                2
            }
        }
    }));

    result.unwrap_or(2) // on panic: not ours
}

// ---------------------------------------------------------------------------
// TECH-DEBT #85: telling "override off" apart from "multi-statement string"
// ---------------------------------------------------------------------------

/// Message for rc=3 when `parser_override` genuinely did not run.
#[cfg(any(feature = "extension", test))]
const OVERRIDE_INACTIVE_MSG: &str = "semantic_views: parser_override is not active for \
                                     this connection (allow_parser_override_extension is \
                                     'DEFAULT' or 'STRICT'). Re-enable with: \
                                     SET allow_parser_override_extension='FALLBACK';";

/// Message for rc=3 when `parser_override` DID run, on a query string that
/// held several statements, and declined it.
#[cfg(any(feature = "extension", test))]
const MULTI_STATEMENT_MSG: &str = "semantic_views: semantic-view DDL must be sent as its own \
                                   statement, one statement per call. This query string also \
                                   contains other statements; send each statement in a \
                                   separate call (a single trailing ';' is fine).";

// The last query string `parser_override` was handed on this thread, kept
// only when it looks like several statements (a `;` with more text after it)
// and mentions SEMANTIC (every statement `parse_function` reports on does),
// else empty. DuckDB's `Parser::ParseQuery` calls the override and then -- on
// the same thread, within the same call -- `parse_function` on each statement
// the default parser could not handle, so this is how `parse_function` learns
// that the override DID run on the string the statement came from. The
// override overwrites the record on every query it sees, and every
// `parse_function` call consumes it, so a record cannot outlive the query it
// came from into a later one -- in particular not into a query on a
// connection whose override is off (the record is per thread, not per
// connection). A string the override rewrote as one DDL statement is not
// recorded at all, even when a literal or comment in it holds a `;`. The one
// survivor is a multi-statement string mentioning SEMANTIC that parsed
// without any parse_function call, which means it held no semantic-view DDL
// as its own statement, so no later DDL statement can match it.
//
// Only queries the user sent count. DuckDB 2.0 runs a string's earlier
// statements before it parses the later ones, and a statement that reads a
// semantic view runs its own queries on this thread meanwhile (on a per-call
// `Connection probe`, each through the override and, if it fails to parse,
// `parse_function`). Each probe site in `cpp/src/shim.cpp` holds an
// `SvNestedQueryGuard` while its connection is open, and while one is held
// the record is neither written nor consumed (`NESTED_QUERY_DEPTH`).
//
// The same 2.0 change lets a record outlive its query: when an earlier
// statement of the string fails as it runs, the DDL after it is never parsed.
// So the record also names the database it was made on (`RECORD_DATABASE`,
// from the key the shim passes with each hook call), and `parse_function` on
// another database never reads it.
#[cfg(any(feature = "extension", test))]
thread_local! {
    static OVERRIDE_MULTI_STATEMENT: std::cell::RefCell<String> =
        const { std::cell::RefCell::new(String::new()) };
    static RECORD_DATABASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HOOK_DATABASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static NESTED_QUERY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// FFI export: the database (any value unique to it; the shim passes its
/// parser-extension info pointer) of the parser hook call about to be made
/// on this thread. See `OVERRIDE_MULTI_STATEMENT`.
#[cfg(any(feature = "extension", test))]
#[no_mangle]
pub extern "C" fn sv_parser_hook_database(key: usize) {
    HOOK_DATABASE.with(|db| db.set(key));
}

/// FFI export: the extension is about to run queries of its own on this
/// thread (see `OVERRIDE_MULTI_STATEMENT`). Paired with
/// [`sv_nested_query_exit`] by `SvNestedQueryGuard` in `cpp/src/shim.cpp`.
#[cfg(any(feature = "extension", test))]
#[no_mangle]
pub extern "C" fn sv_nested_query_enter() {
    NESTED_QUERY_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
}

/// FFI export: the queries [`sv_nested_query_enter`] announced are done.
#[cfg(any(feature = "extension", test))]
#[no_mangle]
pub extern "C" fn sv_nested_query_exit() {
    NESTED_QUERY_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
}

/// FFI export: whether the extension is running queries of its own on this
/// thread, so the query the parser hooks see is one of them.
#[cfg(any(feature = "extension", test))]
#[no_mangle]
pub extern "C" fn sv_nested_query_active() -> bool {
    NESTED_QUERY_DEPTH.with(|depth| depth.get() > 0)
}

/// Whether `query` holds more than one statement, approximately: a `;` with
/// non-whitespace text after it. A `;` inside a string literal can make this
/// true for a single statement; that only matters if `parse_function` is then
/// handed a strict part of that string, which a single statement never causes.
#[cfg(any(feature = "extension", test))]
fn looks_multi_statement(query: &str) -> bool {
    query
        .trim_end()
        .trim_end_matches(';')
        .split_once(';')
        .is_some_and(|(_, rest)| !rest.trim().is_empty())
}

/// Record the query string `parser_override` was handed (see
/// `OVERRIDE_MULTI_STATEMENT`). Anything else clears the record and frees it,
/// so a large script does not stay allocated for the life of the thread.
#[cfg(any(feature = "extension", test))]
pub(crate) fn note_override_query(query: &str) {
    if sv_nested_query_active() {
        return;
    }
    let keep = looks_multi_statement(query)
        && query
            .as_bytes()
            .windows(b"semantic".len())
            .any(|w| w.eq_ignore_ascii_case(b"semantic"));
    if keep {
        RECORD_DATABASE.with(|db| db.set(HOOK_DATABASE.with(std::cell::Cell::get)));
    }
    OVERRIDE_MULTI_STATEMENT.with(|cell| {
        *cell.borrow_mut() = if keep {
            query.to_owned()
        } else {
            String::new()
        };
    });
}

/// Take the record (see `OVERRIDE_MULTI_STATEMENT`), leaving it empty. A
/// query the extension runs itself gets nothing and leaves the record alone,
/// and a record made on another database is dropped unread.
#[cfg(any(feature = "extension", test))]
fn take_override_query() -> String {
    if sv_nested_query_active() {
        return String::new();
    }
    let record = OVERRIDE_MULTI_STATEMENT.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    let same_database =
        RECORD_DATABASE.with(std::cell::Cell::get) == HOOK_DATABASE.with(std::cell::Cell::get);
    if same_database {
        record
    } else {
        String::new()
    }
}

/// Whether `statement` (as handed to `parse_function`) is one whole statement
/// of `recorded`, the multi-statement string the override last saw.
#[cfg(any(feature = "extension", test))]
fn part_of_multi_statement_query(recorded: &str, statement: &str) -> bool {
    let statement = statement.trim().trim_end_matches(';').trim_end();
    if statement.is_empty() {
        return false;
    }
    recorded.match_indices(statement).any(|(at, _)| {
        let before = recorded[..at].trim_end();
        let after = recorded[at + statement.len()..].trim_start();
        (before.is_empty() || before.ends_with(';')) && (after.is_empty() || after.starts_with(';'))
    })
}

/// FFI entry point for `parse_function` — Phase 62's error-reporting layer.
///
/// Called by `DuckDB`'s `Parser::ParseQuery` after the default parser fails on
/// an unrecognised prefix (e.g. `CREATE SEMANTIC VIEW …` or `CRETAE …`).
/// Re-runs validation against the user's input and returns the validation
/// error message + a byte-offset position so `DuckDB`'s
/// `ParserException::SyntaxError` can render `LINE 1: … ^` (caret) at the
/// offending token.
///
/// Return code (`u8`):
///   * `0` — success / unreachable. `parser_override` should have produced
///     rewritten SQL on the success path; if validation succeeds AND we
///     reach `parse_function`, the override didn't fire. We map this to
///     rc=3 in practice; rc=0 is the defensive "internal error" case.
///   * `1` — recognised prefix, but body is invalid OR a near-miss
///     (`CRETAE` etc.) suggestion was produced. `error_out` gets the
///     message; `position_out` gets the byte offset (or `u32::MAX` if no
///     position is available).
///   * `2` — not ours; defer (`DISPLAY_ORIGINAL_ERROR` on the C++ side).
///   * `3` — valid DDL but `parser_override` didn't fire (override setting
///     is `DEFAULT` or `STRICT`, e.g. after `CALL disable_peg_parser()`
///     reset the setting). `error_out` gets an actionable hint
///     (`SET allow_parser_override_extension='FALLBACK'`); `position_out=0`
///     so the caret lands on the `C` of `CREATE` / `D` of `DROP`.
///
/// The extension build re-runs the full rewrite (`rewrite_to_native_sql`); unit
/// tests (no `extension` feature) fall back to syntax-only validation via
/// `plan_rewrite`. Either way the purpose is to recover the `ParseError`
/// message and caret position for a *structurally invalid* DDL statement (e.g. a
/// malformed CREATE body), reproducing exactly what `parser_override` saw.
/// Catalog-level
/// conditions — DROP-of-missing, rename-into-an-existing-name — are NOT
/// rewrite-time errors: `rewrite_drop` / `rewrite_alter_*` emit execution-time
/// SQL guards, so a well-formed-but-catalog-invalid statement rewrites
/// successfully (`Ok(Some)`) and its error surfaces when the emitted SQL runs,
/// not here (such a statement maps to rc=3 if `parser_override` is inactive).
///
/// # Safety
///
/// - `query_ptr` must point to bytes of length `query_len`. Invalid UTF-8
///   makes us return rc=2 (defer) rather than triggering UB.
/// - `error_out` must point to a writable buffer of `error_out_len` bytes,
///   or be null. Null is treated as "do not write the message" (rc still
///   computed correctly).
/// - `position_out` must point to a writable `u32`, or be null. Null is
///   treated as "do not write the position".
#[cfg(any(feature = "extension", test))]
#[no_mangle]
pub unsafe extern "C" fn sv_parse_function_rust(
    query_ptr: *const u8,
    query_len: usize,
    error_out: *mut u8,
    error_out_len: usize,
    position_out: *mut u32,
) -> u8 {
    parse_function_panic_guard(error_out, error_out_len, position_out, || {
        // Consumed by every call, whatever it returns: any parse_function
        // call ends the query it came from, so the record must not outlive
        // it into a later query (TECH-DEBT #85).
        let multi_statement_query = take_override_query();

        // Initialise position_out to UINT32_MAX (no-position sentinel).
        if !position_out.is_null() {
            *position_out = u32::MAX;
        }

        // UTF-8 check; defer rather than synthesise an error on bad bytes.
        if query_ptr.is_null() || query_len == 0 {
            return 2_u8;
        }
        let bytes = std::slice::from_raw_parts(query_ptr, query_len);
        let Ok(query) = std::str::from_utf8(bytes) else {
            return 2_u8;
        };

        // Recognised DDL prefix?
        if detect_ddl_kind(query).is_none() {
            // Not a recognised prefix — try near-miss detection so the
            // user sees `Did you mean CREATE SEMANTIC VIEW?` instead of
            // a generic default-parser syntax error.
            if let Some(err) = detect_near_miss(query) {
                write_error_to_buffer(error_out, error_out_len, &err.message);
                if !position_out.is_null() {
                    *position_out = err
                        .position
                        .and_then(|p| u32::try_from(p).ok())
                        .unwrap_or(u32::MAX);
                }
                return 1_u8;
            }
            return 2_u8; // genuinely not ours
        }

        // Recognised prefix — re-run validation to recover the ParseError
        // wording + caret for a structurally-invalid statement, exactly as
        // parser_override saw it. Catalog-level conditions (DROP-of-missing,
        // rename collision) are execution-time SQL guards, not rewrite-time
        // errors, so they rewrite to Ok(Some) here and are not reproduced.
        // Unit tests fall back to syntax-only validation (see the two cfg'd
        // definitions below).
        let result = run_validation_for_parse_function(query);

        match result {
            Ok(Some(_rewritten)) => {
                // Valid DDL but we got here. Either `parser_override` ran on
                // a multi-statement string holding this statement and did
                // not rewrite it into something that parses (TECH-DEBT #85 --
                // it only rewrites DDL in a string that is one statement), or it did not run at all. Most
                // common cause of the latter: `disable_peg_parser` reset
                // `allow_parser_override_extension` to DEFAULT (TECH-DEBT 21).
                // Position 0 puts the caret on the `C` of CREATE / `D` of
                // DROP / etc.
                let msg = if part_of_multi_statement_query(&multi_statement_query, query) {
                    MULTI_STATEMENT_MSG
                } else {
                    OVERRIDE_INACTIVE_MSG
                };
                write_error_to_buffer(error_out, error_out_len, msg);
                if !position_out.is_null() {
                    *position_out = 0;
                }
                3_u8
            }
            Ok(None) => {
                // detect_ddl_kind matched but validate returned None —
                // unreachable for a matched prefix. Defensive.
                write_error_to_buffer(
                    error_out,
                    error_out_len,
                    "semantic_views: internal error — recognised DDL prefix \
                     produced no rewrite (please report this bug)",
                );
                1_u8
            }
            Err(parse_err) => {
                write_error_to_buffer(error_out, error_out_len, &parse_err.message);
                if !position_out.is_null() {
                    *position_out = parse_err
                        .position
                        .and_then(|p| u32::try_from(p).ok())
                        .unwrap_or(u32::MAX);
                }
                1_u8
            }
        }
    })
}

/// Run `body` under `catch_unwind` and convert a panic into the
/// `parse_function` return-code contract.
///
/// Split out of [`sv_parse_function_rust`] so the panic arm is reachable from
/// a unit test with a real unwinding panic (the production body has no
/// injectable fault), and so the arm's diagnostic lives next to the wording
/// the read dispatchers use.
///
/// FF-16 (code-review 2026-08-08): this arm used to return rc=2 ("not ours"),
/// which routed a Rust-side panic straight into `DISPLAY_ORIGINAL_ERROR` — the
/// user saw `DuckDB`'s generic `syntax error at or near "SEMANTIC"` and the
/// extension's internal failure left no trace anywhere, indistinguishable from
/// "extension not loaded". The 17 read dispatchers
/// (`crate::ddl::read_ffi::run_dispatcher`) surface their panics as
/// `internal error: panic inside <name>`; this one now does the same, returning
/// rc=1 so `sv_parse_stub` renders it through `DISPLAY_EXTENSION_ERROR`.
///
/// The override side (`sv_parser_override_rust`) deliberately keeps rc=2 on
/// panic: it runs *before* the default parser, and a panic there must still let
/// the `parse_function` pass have its chance at the statement.
///
/// # Safety
///
/// `error_out` must be null or point to `error_out_len` writable bytes;
/// `position_out` must be null or point to a writable `u32`.
#[cfg(any(feature = "extension", test))]
unsafe fn parse_function_panic_guard<F>(
    error_out: *mut u8,
    error_out_len: usize,
    position_out: *mut u32,
    body: F,
) -> u8
where
    F: FnOnce() -> u8,
{
    // `if let ... else` rather than `match`, mirroring
    // `crate::ddl::read_ffi::run_dispatcher`'s shape (and satisfying
    // clippy::single_match_else).
    if let Ok(rc) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        rc
    } else {
        // No caret position is meaningful for an internal failure: the fault
        // is ours, not a location in the user's statement.
        if !position_out.is_null() {
            *position_out = u32::MAX;
        }
        write_error_to_buffer(
            error_out,
            error_out_len,
            "semantic_views: internal error: panic inside \
             sv_parse_function_rust (please report this bug)",
        );
        1
    }
}

/// Re-run validation for the `parse_function` path. Mirrors what
/// `sv_parser_override_rust` did at parse time (`rewrite_to_native_sql`), so a
/// structurally-invalid DDL statement surfaces the same `ParseError` wording +
/// position that `parser_override` produced. Catalog-level errors (DROP-of-missing,
/// rename collision) are enforced by guards in the *emitted* SQL at execution
/// time, so they are not reproduced here — such statements return `Ok(Some(_))`.
///
/// Returning `Ok(Some(_))` means "validation succeeded" — at the
/// `parse_function` call site this can only happen when `parser_override`
/// itself didn't run, so the caller maps it to rc=3 (actionable hint).
#[cfg(feature = "extension")]
fn run_validation_for_parse_function(query: &str) -> Result<Option<String>, ParseError> {
    rewrite_to_native_sql(query)
}

/// Test-only sibling of `run_validation_for_parse_function` — pure syntax
/// validation. Under `cargo test` the `extension` feature is OFF (default
/// features = bundled), so `rewrite_to_native_sql` is unavailable; only the
/// `Ok(Some)/Ok(None)/Err` shape matters (the caller discards the string).
#[cfg(all(not(feature = "extension"), test))]
fn run_validation_for_parse_function(query: &str) -> Result<Option<String>, ParseError> {
    plan_rewrite(query).map(|opt| opt.map(|_| String::new()))
}

// ---------------------------------------------------------------------------
// Tests — FF-16 panic-arm contract
// ---------------------------------------------------------------------------
//
// The panic arm of `sv_parse_function_rust` cannot be reached through the
// public entry point (no injectable fault in the validation body), so
// `parse_function_panic_guard` exists as the seam these tests drive with a
// real unwinding panic. Everything else about the entry point — the rc=0/1/2/3
// contract for well-formed inputs — is covered in `super::rewrite`'s test
// module (`call_sv_parse_function`).

#[cfg(test)]
mod tests {
    use super::parse_function_panic_guard;
    use super::{note_override_query, sv_parse_function_rust};

    const DDL: &str = "CREATE SEMANTIC VIEW sales AS TABLES (o AS orders PRIMARY KEY (id)) \
                       DIMENSIONS (o.region AS o.region) METRICS (o.revenue AS SUM(o.amount))";

    /// Call `sv_parse_function_rust` on `statement`, returning `(rc, message)`.
    fn call_parse_function(statement: &str) -> (u8, String) {
        let mut error_buf = vec![0_u8; 1024];
        let mut position: u32 = u32::MAX;
        let rc = unsafe {
            sv_parse_function_rust(
                statement.as_ptr(),
                statement.len(),
                error_buf.as_mut_ptr(),
                error_buf.len(),
                &raw mut position,
            )
        };
        let nul = error_buf.iter().position(|&b| b == 0).unwrap_or(0);
        (rc, String::from_utf8_lossy(&error_buf[..nul]).into_owned())
    }

    // TECH-DEBT #85. Each #[test] runs on its own thread, so the thread-local
    // record starts empty in every test below.

    /// Control: `parse_function` reached for valid DDL with no declined
    /// query on record means the override never ran -- the original
    /// "not active" advice is the right one.
    #[test]
    fn valid_ddl_without_a_declined_query_reports_override_inactive() {
        let (rc, msg) = call_parse_function(DDL);
        assert_eq!(
            rc, 3,
            "valid DDL reaching parse_function is rc=3; msg={msg}"
        );
        assert!(msg.contains("parser_override is not active"), "got: {msg}");
        assert!(!msg.contains("one statement per call"), "got: {msg}");
    }

    /// The override declined a multi-statement string holding the DDL
    /// after another statement; the DDL alone then reaches parse_function.
    #[test]
    fn ddl_after_another_statement_says_one_statement_per_call() {
        note_override_query(&format!("CREATE TABLE orders (id INTEGER); {DDL};"));
        let (rc, msg) = call_parse_function(DDL);
        assert_eq!(rc, 3, "msg={msg}");
        assert!(msg.contains("one statement per call"), "got: {msg}");
        assert!(!msg.contains("parser_override is not active"), "got: {msg}");
    }

    /// Same, with the DDL first and another statement after it.
    #[test]
    fn ddl_before_another_statement_says_one_statement_per_call() {
        note_override_query(&format!("{DDL}; SELECT 1;"));
        let (rc, msg) = call_parse_function(DDL);
        assert_eq!(rc, 3, "msg={msg}");
        assert!(msg.contains("one statement per call"), "got: {msg}");
    }

    /// Only a `;` followed by more text marks a string as multi-statement;
    /// a trailing `;` (or several, or trailing whitespace) does not.
    #[test]
    fn looks_multi_statement_ignores_trailing_semicolons() {
        use super::looks_multi_statement;
        assert!(!looks_multi_statement("SELECT 1"));
        assert!(!looks_multi_statement("SELECT 1;"));
        assert!(!looks_multi_statement("SELECT 1; ;  \n"));
        assert!(looks_multi_statement("SELECT 1; SELECT 2"));
        assert!(looks_multi_statement("CREATE TABLE t (x INT);\nSELECT 1;"));
    }

    /// Control: the last declined query on this thread was a single
    /// statement (or an unrelated multi-statement string), so the override
    /// cannot be blamed on splitting -- keep the "not active" message.
    #[test]
    fn unrelated_or_single_declined_query_keeps_override_inactive() {
        note_override_query("SELECT 1");
        let (_, msg) = call_parse_function(DDL);
        assert!(msg.contains("parser_override is not active"), "got: {msg}");

        note_override_query("SELECT 1; SELECT 2;");
        let (_, msg) = call_parse_function(DDL);
        assert!(msg.contains("parser_override is not active"), "got: {msg}");
    }

    /// The record is consumed by the `parse_function` call that reads it, so
    /// a later standalone statement -- possibly on another connection whose
    /// override is off -- is not blamed on a string that already failed.
    #[test]
    fn record_is_consumed_when_read() {
        note_override_query(&format!("SELECT 1; {DDL}"));
        let (_, msg) = call_parse_function(DDL);
        assert!(msg.contains("one statement per call"), "got: {msg}");
        let (_, msg) = call_parse_function(DDL);
        assert!(msg.contains("parser_override is not active"), "got: {msg}");
    }

    /// Any `parse_function` call consumes the record, not only the one that
    /// reports on it: an earlier statement of the same string that is not
    /// ours (`SELEC 1` here) ends the query, and must not leave the record
    /// for a later query's DDL.
    #[test]
    fn record_is_consumed_by_a_statement_that_is_not_ours() {
        note_override_query(&format!("SELEC 1; {DDL}"));
        let (rc, _) = call_parse_function("SELEC 1");
        assert_eq!(rc, 2, "a typo far from every DDL prefix is not ours");
        let (_, msg) = call_parse_function(DDL);
        assert!(msg.contains("parser_override is not active"), "got: {msg}");
    }

    /// DuckDB 2.0 runs a string's earlier statements before it parses the
    /// later ones, so a semantic-view read ahead of the DDL runs the
    /// extension's own queries through the parser hooks first. They neither
    /// replace the record nor consume it, even one that fails to parse.
    #[test]
    fn queries_the_extension_runs_itself_leave_the_record_alone() {
        use super::{sv_nested_query_active, sv_nested_query_enter, sv_nested_query_exit};
        note_override_query(&format!(
            "SELECT * FROM semantic_view('existing', metrics := ['revenue']); {DDL}"
        ));
        sv_nested_query_enter();
        sv_nested_query_enter();
        note_override_query("SELECT 1 AS probe; SELECT 2 AS semantic");
        let (rc, _) = call_parse_function("SELEC 1 AS probe");
        assert_eq!(rc, 2, "a typo far from every DDL prefix is not ours");
        sv_nested_query_exit();
        assert!(sv_nested_query_active(), "the outer guard is still held");
        sv_nested_query_exit();
        assert!(!sv_nested_query_active());
        let (rc, msg) = call_parse_function(DDL);
        assert_eq!(rc, 3, "msg={msg}");
        assert!(msg.contains("one statement per call"), "got: {msg}");
    }

    /// A record made on one database is not read by `parse_function` on
    /// another: DuckDB 2.0 leaves a record unread when an earlier statement
    /// of its string fails as it runs, and that string's DDL sent alone to a
    /// database whose override is off must keep the override-off message.
    #[test]
    fn a_record_from_another_database_is_not_read() {
        use super::sv_parser_hook_database;
        sv_parser_hook_database(1);
        note_override_query(&format!("SELECT * FROM missing_table; {DDL}"));
        sv_parser_hook_database(2);
        let (_, msg) = call_parse_function(DDL);
        assert!(msg.contains("parser_override is not active"), "got: {msg}");
        sv_parser_hook_database(1);
        let (_, msg) = call_parse_function(DDL);
        assert!(
            msg.contains("parser_override is not active"),
            "the record is dropped, got: {msg}"
        );
    }

    /// A statement matches only as a whole statement of the recorded string,
    /// not as a prefix of a longer one (`sales` inside `sales_v2`).
    #[test]
    fn statement_must_match_a_whole_recorded_statement() {
        note_override_query("SELECT 1; DROP SEMANTIC VIEW sales_v2;");
        let (_, msg) = call_parse_function("DROP SEMANTIC VIEW sales");
        assert!(msg.contains("parser_override is not active"), "got: {msg}");
    }

    /// A multi-statement string that cannot hold semantic-view DDL (every
    /// such statement says SEMANTIC) is not copied, and a big one recorded
    /// earlier is released rather than kept at its capacity.
    #[test]
    fn strings_without_semantic_are_not_kept() {
        use super::OVERRIDE_MULTI_STATEMENT;
        note_override_query(&format!("SELECT 1; {DDL}"));
        note_override_query("CREATE TABLE t (x INT); INSERT INTO t VALUES (1);");
        OVERRIDE_MULTI_STATEMENT.with(|cell| {
            let slot = cell.borrow();
            assert!(slot.is_empty(), "got: {slot}");
            assert_eq!(slot.capacity(), 0, "the earlier record must be released");
        });
    }

    /// Call the guard with `body`, returning `(rc, message, position)`.
    fn call_guard(body: impl FnOnce() -> u8) -> (u8, String, u32) {
        let mut error_buf = vec![0_u8; 1024];
        let mut position: u32 = 7; // sentinel that is neither 0 nor u32::MAX
        let rc = unsafe {
            parse_function_panic_guard(
                error_buf.as_mut_ptr(),
                error_buf.len(),
                &raw mut position,
                body,
            )
        };
        let nul = error_buf.iter().position(|&b| b == 0).unwrap_or(0);
        let msg = String::from_utf8_lossy(&error_buf[..nul]).into_owned();
        (rc, msg, position)
    }

    /// FF-16: a panic inside the parser-hook Rust code must reach the user as
    /// the extension's own internal error (rc=1 → `DISPLAY_EXTENSION_ERROR`),
    /// not as rc=2 "not ours" — which routed it into DuckDB's generic
    /// `syntax error at or near "SEMANTIC"` with no trace of the real failure,
    /// indistinguishable from the extension not being loaded at all.
    #[test]
    fn panic_surfaces_as_an_extension_error_not_as_not_ours() {
        // Silence the default panic hook for the duration: the panic is
        // deliberate and its backtrace would otherwise pollute test output.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let (rc, msg, pos) = call_guard(|| panic!("deliberate test panic"));
        std::panic::set_hook(prev);

        assert_eq!(
            rc, 1,
            "a panic must take the DISPLAY_EXTENSION_ERROR channel (rc=1), \
             not defer as 'not ours' (rc=2); msg={msg}"
        );
        assert!(
            msg.contains("internal error: panic inside sv_parse_function_rust"),
            "panic diagnostic must match the read dispatchers' wording \
             (`internal error: panic inside <name>`); got: {msg:?}"
        );
        assert_eq!(
            pos,
            u32::MAX,
            "no caret position is meaningful for an internal failure"
        );
    }

    /// Null out-parameters must not turn the panic arm into UB or a different
    /// return code — the C++ side is allowed to pass either as null.
    #[test]
    fn panic_arm_tolerates_null_out_parameters() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let rc = unsafe {
            parse_function_panic_guard(std::ptr::null_mut(), 0, std::ptr::null_mut(), || {
                panic!("deliberate test panic")
            })
        };
        std::panic::set_hook(prev);
        assert_eq!(
            rc, 1,
            "null out-params must not change the panic return code"
        );
    }

    /// The guard is transparent on the non-panicking path: it must return the
    /// body's rc verbatim and leave both out-parameters exactly as the body
    /// left them (the rc=0/1/2/3 contract is the body's to define).
    #[test]
    fn non_panicking_body_passes_through_untouched() {
        for expected in [0_u8, 1, 2, 3] {
            let (rc, msg, pos) = call_guard(|| expected);
            assert_eq!(rc, expected, "guard must not rewrite a non-panic rc");
            assert!(
                msg.is_empty(),
                "guard must not write a diagnostic on the non-panic path; got {msg:?}"
            );
            assert_eq!(
                pos, 7,
                "guard must not touch position_out on the non-panic path"
            );
        }
    }
}
