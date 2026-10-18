// C++ helper for the DuckDB semantic_views extension.
//
// The Rust entry point (semantic_views_init_c_api, C_STRUCT ABI) owns the
// DuckDB handshake and function registration. After init, it calls
// sv_register_parser_hooks() here to install:
//
//   * parser_override (sv_parser_override) — the success path. Recognised
//     CREATE / DROP / ALTER / DESCRIBE / SHOW DDL is rewritten to native SQL
//     and re-parsed on the caller's connection (transactional behaviour).
//   * parse_function (sv_parse_stub) — Phase 62 Plan 03 error-reporting
//     layer. Called by DuckDB after the default parser fails on an
//     unrecognised prefix; re-runs validation and returns
//     DISPLAY_EXTENSION_ERROR with `error_location` so
//     ParserException::SyntaxError can render `LINE 1: … ^` (caret).
//   * plan_function (sv_plan_unreachable) — required sibling of
//     parse_function; should never fire because sv_parse_stub never returns
//     PARSE_SUCCESSFUL.
//
// All DuckDB C++ symbols come from the DuckDB engine linked statically into
// the extension (`duckdb_static`, built by scripts/ensure_engine.py; see
// cpp/engine.manifest). Symbol visibility on the cdylib restricts exports to
// just the Rust entry point, so these definitions stay internal to the binary.
//
// DuckDB 1.5.0 moved the parser extension type declarations from duckdb.hpp
// into duckdb.cpp. The compat header re-declares them so this translation
// unit can use them. See cpp/include/parser_extension_compat.hpp for details.

#include "parser_extension_compat.hpp"
#include "shim.hpp"
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstring>
#include <memory>
#include <mutex>
#include <system_error>
#include <thread>

// Destructors here release things while a DuckDB exception unwinds a bind:
// SvBindCancellationScope joins its watcher thread, the probe Connection
// closes, owned buffers are freed. MSVC emits those unwinding calls only
// under /EHsc (`_CPPUNWIND`), which the cc crate does not pass on its own
// (build.rs does). Without it every bind that threw left its watcher thread
// waiting on a condition variable in the dead frame, and the process crashed
// with an access violation once that stack was reused (windows_amd64, from
// the watcher's introduction in #248 until the flag).
#if defined(_MSC_VER) && !defined(_CPPUNWIND)
#error "shim.cpp must be compiled with C++ exception unwinding (/EHsc)"
#endif

using namespace duckdb;

// ---------------------------------------------------------------------------
// BORROW-contract bridge guard (Phase 65.1 Plan 10, WR-06, D-12)
// ---------------------------------------------------------------------------
// The `reinterpret_cast<duckdb_connection>(Connection*)` pattern used at ~20
// bind/exec callback sites in this file (e.g. shim.cpp:993, 1285, 1311, …)
// depends on `duckdb_connection` being layout-compatible with a single
// pointer. This compile-time guard catches DuckDB ABI drift that would
// change the size of `duckdb_connection` (e.g. wrapping it in a struct with
// extra fields). The complementary runtime probe in `sv_register_parser_hooks`
// catches representation drift the size check would miss (e.g. an indirection
// through `internal_ptr`).
//
// See cpp/src/shim.cpp BORROW contract docs and
// .planning/phases/65-overridecontext-connection-teardown/65-REVIEW.md WR-06.
static_assert(sizeof(duckdb_connection) == sizeof(void*),
    "duckdb_connection must be pointer-sized — bridge contract broken; "
    "see cpp/src/shim.cpp BORROW contract docs and 65-REVIEW.md WR-06 / "
    "Phase 65.1 D-12");

// ---------------------------------------------------------------------------
// sv_compiled_duckdb_version -- the DuckDB release this binary was built from
// ---------------------------------------------------------------------------
// The version of the engine linked into this binary, which is the release
// this file's layout assumptions hold for. The Rust entry point
// compares it with the host's `duckdb_library_version()` before calling any
// other function here, and refuses to load on a mismatch: the C_STRUCT ABI
// (see the Makefile) no longer has DuckDB make that check for us.
extern "C" const char *sv_compiled_duckdb_version() {
#ifdef SV_DUCKDB_2
    // The engine's header tree does not define DUCKDB_VERSION (its CMake build
    // passes it to the engine's own sources only). The statically linked
    // engine reports the version it was built as.
    return DuckDB::LibraryVersion();
#else
    return DUCKDB_VERSION;
#endif
}

// ---------------------------------------------------------------------------
// Rust FFI declarations (defined in src/parse.rs)
// ---------------------------------------------------------------------------
extern "C" {
    // Parser-override DDL rewrite. For recognized semantic-view DDL emits
    // native SQL (INSERT/DELETE/UPDATE on _definitions for write-side DDL,
    // or `SELECT * FROM <read_side_table_function>(...)` pass-through for
    // DESCRIBE/SHOW). Returns:
    //   0 = success: heap-owned (ptr, len) written to *sql_out_ptr/*sql_out_len.
    //                Caller takes ownership and MUST release via sv_free_buffer.
    //                Buffer is NOT NUL-terminated — read exactly *sql_out_len bytes.
    //   1 = error:   message written to error_out (NUL-terminated, capped at
    //                error_out_len-1 bytes); *sql_out_ptr left untouched.
    //                (Phase 62: unused — Err branches now return rc=2 and
    //                let parse_function render caret via DISPLAY_EXTENSION_ERROR.)
    //   2 = not ours: defer to default parser.
    //
    // AR-7: the opaque Box<OverrideContext>* context parameter was removed.
    // It was empty after Phase 65 Plan 06 moved catalog pre-checks into the
    // emitted SQL, so the hook no longer needs any per-DB state.
    uint8_t sv_parser_override_rust(
        const char *query_ptr, size_t query_len,
        char **sql_out_ptr, size_t *sql_out_len,
        char *error_out, size_t error_out_len);

    // Phase 62 Plan 03: parse_function callback. Called by DuckDB after the
    // default parser fails on an unrecognised prefix. Re-runs validation
    // and returns rc + error message + byte-offset position so
    // ParserException::SyntaxError can render `LINE 1: ... ^`.
    // Return codes:
    //   0 = success/unreachable (defensive)
    //   1 = ours-but-invalid (validation error or near-miss). error_buf
    //       gets the message, position_out gets the byte offset (or
    //       UINT32_MAX if no position is available). FF-16: a panic inside
    //       the Rust hook also lands here (message
    //       "internal error: panic inside sv_parse_function_rust",
    //       position UINT32_MAX) so an extension bug surfaces as our own
    //       error instead of DuckDB's generic prefix syntax error.
    //   2 = not ours; defer (DISPLAY_ORIGINAL_ERROR on the C++ side).
    //   3 = valid DDL but parser_override didn't fire (e.g. override
    //       setting reset by disable_peg_parser). error_buf gets an
    //       actionable hint; position_out gets 0.
    uint8_t sv_parse_function_rust(
        const char *query_ptr, size_t query_len,
        char *error_buf, size_t error_buf_len,
        uint32_t *position_out);

    // Grammar bridge (src/grammar/ffi.rs). The changes to register, as text
    // released with sv_free_buffer: 0 = ok, 1 = failed.
    uint8_t sv_grammar_changes_rust(char **out_ptr, size_t *out_len);
    // Lower one matched statement rule (its serialised parse subtree) to SQL:
    // 0 = ok (SQL in *sql_out_*, release with sv_free_buffer), 1 = invalid
    // statement (message in error_out, byte offset in *position_out or
    // UINT32_MAX), 2 = bridge failure (message in error_out).
    uint8_t sv_grammar_transform_rust(
        const uint8_t *tree_ptr, size_t tree_len,
        char **sql_out_ptr, size_t *sql_out_len,
        char *error_out, size_t error_out_len,
        uint32_t *position_out);

    // Releases a buffer previously produced by sv_parser_override_rust.
    // Safe to call with a null pointer (no-op). ptr/len must be the exact
    // pair the Rust side returned.
    void sv_free_buffer(char *ptr, size_t len);

    // Phase 65 Plan 04 (Task 2 Step C) — Rust FFI bridge for the
    // `__sv_compute_create_from_yaml` helper TF. The bind callback reads the
    // file content directly through DuckDB's FileSystem API (Phase 65.1
    // D-01..D-03 replaced the earlier read_text(?) SQL path) and this bridge
    // parses + enriches the YAML on the Rust side, returning the
    // metadata-less JSON definition as a heap-owned (ptr, len) buffer.
    //
    // The outer parser_override INSERT wraps `new_def` with json_merge_patch
    // to add now()/current_database()/current_schema() on the caller's
    // connection (matching the metadata-via-SQL pattern landed in Plan 03
    // for the inline CREATE path).
    //
    // Parameters:
    //   content_ptr/len — YAML bytes loaded by the C++ bind callback.
    //   name_ptr/len    — view name (bare identifier).
    //   comment_ptr/len — optional COMMENT='...' value; pass len=0 for
    //                     "no comment provided" (the helper leaves
    //                     `def.comment` untouched in that case so the
    //                     YAML's own `comment:` field, if any, survives).
    //   out_ptr/out_len — on rc=0, point to a heap-owned UTF-8 buffer
    //                     (NOT NUL-terminated) holding the JSON definition.
    //                     Caller MUST release via `sv_free_buffer` with
    //                     the exact (ptr, len) pair Rust returned.
    //   error_buf       — on rc!=0, gets a NUL-terminated error message
    //                     capped at `error_buf_len-1` bytes. Untouched
    //                     on success.
    //
    // Return codes:
    //   0 — success; (out_ptr, out_len) populated.
    //   1 — YAML parse / size-cap error; error_buf populated.
    //   2 — enrichment / validation error; error_buf populated.
    //   3 — internal error (panic across FFI, allocation failure, etc.);
    //       error_buf populated.
    // Phase 65.1 Plan 07 (IN-04 D-24): `kind` parameter removed — the outer
    // parser_override INSERT shape (OR IGNORE / OR REPLACE / plain) already
    // encodes ON CONFLICT behaviour, so the helper itself does not branch on
    // it. Three-arg helper TF signature matches: (file_path, view_name,
    // comment).
    uint8_t sv_compute_create_from_yaml_rust(
        const uint8_t *content_ptr, size_t content_len,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *comment_ptr, size_t comment_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 (Task 1 / Wave 0 bridge spike) — Rust dispatcher for
    // `list_semantic_views()`. The C++ bind callback opens a per-call
    // `Connection probe(*context.db)` and bridges by casting the stack
    // `Connection *` to `duckdb_connection` (cast confirmed by
    // duckdb.cpp:266432-266447 where `duckdb_connect` is literally
    // `reinterpret_cast<duckdb_connection>(new Connection(...))`, so the
    // C-API handle is a borrowed pointer to a C++ `Connection`).
    //
    // The bridge is a BORROW, not a transfer: the Rust dispatcher does NOT
    // call `duckdb_disconnect` (which would `delete` the stack Connection
    // and segfault). When the C++ bind scope ends, the `probe` local's
    // destructor runs and `~Connection()` does the correct teardown.
    //
    // The dispatcher serializes the result rows into a length-prefixed
    // binary buffer (`u32 row_count; for each row: for each of 6 cols: u32
    // byte_len; bytes...`) which the C++ bind parses into BindData. The
    // exec callback then emits rows from BindData. Using a flat binary
    // wire format avoids needing matched struct layouts across the FFI
    // boundary.
    //
    // Return codes:
    //   0 — success; (out_ptr, out_len) populated with the binary buffer.
    //        Caller MUST release via `sv_free_buffer`.
    //   1 — catalog read error; error_buf populated.
    //   2 — internal error (panic across FFI); error_buf populated.
    uint8_t sv_list_semantic_views_bind_rust(
        duckdb_connection conn,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 2 (Wave 1) — Rust dispatcher for the migrated
    // `list_terse_semantic_views()` table function. 5-column subset of
    // list_semantic_views (no `comment`). Same bridge mechanism and borrow
    // contract as the Wave 0 spike.
    uint8_t sv_list_terse_semantic_views_bind_rust(
        duckdb_connection conn,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 2 (Wave 1) — Rust dispatchers for the migrated
    // zero-arg "_all" TFs. All emit homogeneous VARCHAR rows; cell layout
    // matches the matching legacy duckdb-rs VTab. See per-dispatcher Rust
    // doc-headers for the exact column order.
    uint8_t sv_show_semantic_dimensions_all_bind_rust(
        duckdb_connection conn,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_metrics_all_bind_rust(
        duckdb_connection conn,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_facts_all_bind_rust(
        duckdb_connection conn,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_materializations_all_bind_rust(
        duckdb_connection conn,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 3 (Wave 2) — Rust dispatchers for the migrated
    // single-arg (view name) and two-arg (view name + metric name) TFs.
    // All take a per-call borrowed duckdb_connection plus name (ptr, len)
    // tuples. The single-view show_columns / describe / show_semantic_*
    // variants emit VARCHAR rows; show_semantic_dimensions_for_metric
    // emits VARCHAR+BOOL rows (3 VARCHAR + 1 trailing BOOL per row).
    uint8_t sv_show_columns_in_semantic_view_bind_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_describe_semantic_view_bind_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_dimensions_bind_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_metrics_bind_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_facts_bind_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_materializations_bind_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_dimensions_for_metric_bind_rust(
        duckdb_connection conn,
        const uint8_t *view_name_ptr, size_t view_name_len,
        const uint8_t *metric_name_ptr, size_t metric_name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 4 (Wave 3) — Rust dispatchers for the migrated
    // scalar functions (get_ddl, read_yaml_from_semantic_view). Per-row
    // dispatch: each invocation takes the borrowed per-call duckdb_connection
    // (opened from the C++ exec callback via Connection probe(*state.GetContext().db))
    // plus the input string args, and returns a heap-owned UTF-8 buffer for
    // the C++ side to copy into the result Vector via StringVector::AddString.
    //
    // Same wire convention as the bind dispatchers: rc=0 on success with
    // (out_ptr, out_len) populated (caller frees via sv_free_buffer), rc=1
    // on user-visible error (error_buf populated, raised as
    // InvalidInputException by the C++ side), rc=2 on internal panic.
    // `qualify` is the optional third GET_DDL argument (0 = bare name, non-zero
    // = schema-qualified). The two-argument arity passes 0.
    uint8_t sv_get_ddl_exec_rust(
        duckdb_connection conn,
        const uint8_t *type_ptr, size_t type_len,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        uint8_t qualify,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_read_yaml_from_semantic_view_exec_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 5 (Wave 5) — Rust dispatcher for the migrated
    // `explain_semantic_view(view_name, dimensions := [...], metrics := [...],
    // facts := [...])` table function. Same per-call Connection BORROW
    // contract as the 14 Batch-1 migrations. The three optional named
    // LIST(VARCHAR) parameters are flattened on the C++ side into the
    // standard length-prefixed wire format (`u32 count; for each entry:
    // u32 len + bytes`) and passed as (ptr, len) pairs. A null pointer
    // with len=0 means the named parameter was not supplied (treated as
    // an empty list).
    uint8_t sv_explain_semantic_view_bind_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        const uint8_t *dims_ptr, size_t dims_len,
        const uint8_t *metrics_ptr, size_t metrics_len,
        const uint8_t *facts_ptr, size_t facts_len,
        const uint8_t *where_ptr, size_t where_len,
        const uint8_t *order_ptr, size_t order_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Rust dispatcher for `semantic_view(view_name, dimensions := [...],
    // metrics := [...], ...)`. Same per-call BORROW contract as the other
    // read-side dispatchers. Reads the definition, expands the request and
    // returns the SQL to plan, UTF-8: one SELECT, with its columns named and
    // ordered as the request asks. `sv_semantic_view_bind_replace` parses it
    // into the subquery DuckDB binds in the call's place.
    uint8_t sv_semantic_view_bind_rust(
        duckdb_connection conn,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        const uint8_t *dims_ptr, size_t dims_len,
        const uint8_t *metrics_ptr, size_t metrics_len,
        const uint8_t *facts_ptr, size_t facts_len,
        const uint8_t *where_ptr, size_t where_len,
        const uint8_t *order_ptr, size_t order_len,
        uint8_t fence,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
}

// ---------------------------------------------------------------------------
// SemanticViewParseData — Phase 62 Plan 03
// ---------------------------------------------------------------------------
// Concrete subclass of ParserExtensionParseData required by the
// ParserExtensionParseResult(unique_ptr<...>) constructor. We never return
// PARSE_SUCCESSFUL from sv_parse_stub (its sole purpose is rendering errors
// via DISPLAY_EXTENSION_ERROR), so this type is structurally needed only
// for layout / type-system reasons. If we ever do produce a parse_data,
// sv_plan_unreachable below would fire.
struct SemanticViewParseData : public ParserExtensionParseData {
    string query;
    explicit SemanticViewParseData(string q) : query(std::move(q)) {}

    unique_ptr<ParserExtensionParseData> Copy() const override {
        return make_uniq<SemanticViewParseData>(query);
    }
    string ToString() const override {
        return query;
    }
};

// RAII guard for heap-owned buffers returned by the Rust FFI. Ensures the
// buffer is released even if a downstream call (Parser::ParseQuery) throws.
struct SvOwnedBuffer {
    char *ptr = nullptr;
    size_t len = 0;
    SvOwnedBuffer() = default;
    SvOwnedBuffer(const SvOwnedBuffer &) = delete;
    SvOwnedBuffer &operator=(const SvOwnedBuffer &) = delete;
    SvOwnedBuffer(SvOwnedBuffer &&other) noexcept
        : ptr(other.ptr), len(other.len) {
        other.ptr = nullptr;
        other.len = 0;
    }
    SvOwnedBuffer &operator=(SvOwnedBuffer &&other) noexcept {
        if (this != &other) {
            if (ptr) sv_free_buffer(ptr, len);
            ptr = other.ptr;
            len = other.len;
            other.ptr = nullptr;
            other.len = 0;
        }
        return *this;
    }
    ~SvOwnedBuffer() {
        if (ptr) sv_free_buffer(ptr, len);
    }
    string to_string() const {
        return ptr ? string(ptr, len) : string();
    }
};

// Per-extension-load marker attached to ParserExtension::parser_info. AR-7:
// this used to carry an opaque Box<OverrideContext>* (rust_state), but the
// OverrideContext was empty after Phase 65 Plan 06's H1 catalog_conn
// retirement, so it now holds no state. It is kept purely as the
// `dynamic_cast<SemanticViewsParserInfo *>` marker type that `sv_parser_override`
// uses to confirm the parser_info is ours. (`sv_parse_stub` no longer consults
// parser_info at all — validation needs no per-DB state — so it ignores the
// marker.)
struct SemanticViewsParserInfo : public ParserExtensionInfo {};

#ifdef SV_DUCKDB_2
// The query the last parser_override call on this thread saw. DuckDB 2.0's
// parse_function gets tokens without source offsets; when it runs, it is inside
// the same Parser::ParseQuery as the override call that recorded this, so the
// tokens can be located in it (sv_locate_tail). It is kept only while the stub
// may still need it: the override clears it when its rewrite of the query
// parses, and the stub whenever it runs, since its answer ends the parse.
// Otherwise a connection whose override is off, which records nothing, would
// quote an earlier query on this thread that ended with the same statement.
static thread_local std::string sv_override_query;
#endif

// ---------------------------------------------------------------------------
// Parser-override hook: sv_parser_override
// ---------------------------------------------------------------------------
// The sole DDL entry point. Runs *before* the default parser. Recognized
// semantic-view DDL is rewritten into native SQL by the Rust side and
// re-parsed via DuckDB's own Parser, producing SQLStatement ASTs that DuckDB
// then plans and executes on the caller's connection — so write-side DDL
// participates in the caller's transaction.
//
// For non-matching queries returns DISPLAY_ORIGINAL_ERROR so DuckDB falls
// through to the default parser.
static ParserOverrideResult sv_parser_override(
    ParserExtensionInfo *info, const string &query, ParserOptions &) {

    // Confirm this is our parser_info. info is the per-extension-load
    // SemanticViewsParserInfo attached at registration time; if it isn't ours
    // (dynamic_cast fails) defer to the default parser. AR-7: there is no
    // longer any per-DB Rust state to route through.
    auto *sv_info = dynamic_cast<SemanticViewsParserInfo *>(info);
    if (!sv_info) {
        return ParserOverrideResult();
    }
#ifdef SV_DUCKDB_2
    sv_override_query = query;
#endif

    SvOwnedBuffer sql_buf;
    char error_buf[1024];
    memset(error_buf, 0, sizeof(error_buf));

    uint8_t rc = sv_parser_override_rust(
        query.c_str(), query.size(),
        &sql_buf.ptr, &sql_buf.len,
        error_buf, sizeof(error_buf));

    if (rc == 2) {
        // Not our query — let DuckDB's default parser handle it.
        return ParserOverrideResult();
    }

    if (rc == 1) {
        // Phase 62 Plan 03: defer to default parser. parse_function
        // (registered as sv_parse_stub) re-runs validation and returns
        // DISPLAY_EXTENSION_ERROR with caret position. The Rust side
        // currently always returns rc=2 on the error path, so this
        // branch is defensive — kept to match the documented contract.
        return ParserOverrideResult();
    }

    // rc == 0: native SQL produced. Once it parses, no stub runs for this
    // query, so its recorded text is dropped. Re-parse via DuckDB's Parser. Use
    // default-constructed ParserOptions so parser_override doesn't recurse
    // (DEFAULT_OVERRIDE skips parser_override hooks entirely). The
    // rewritten SQL is read by exact length, so size is unbounded.
    string native_sql = sql_buf.to_string();
    try {
        Parser parser = sv_compat::builtin_parser();
        parser.ParseQuery(native_sql);
#ifdef SV_DUCKDB_2
        sv_override_query.clear();
#endif
        return ParserOverrideResult(std::move(parser.statements));
    } catch (std::exception &e) {
        return ParserOverrideResult(e);
    }
}

// ---------------------------------------------------------------------------
// Parse-function hook: sv_parse_stub  (Phase 62 Plan 03)
// ---------------------------------------------------------------------------
// Called by DuckDB's Parser::ParseQuery after the default parser fails on
// an unrecognised prefix. Sole purpose is rendering caret-aware errors via
// ParserException::SyntaxError(query, msg, error_location).
//
// parser_override remains the success path — it rewrites recognized DDL
// to native SQL, re-parses on the caller's connection, and returns
// PARSE_SUCCESSFUL (transactional behaviour preserved). For error cases
// parser_override now ALWAYS returns DISPLAY_ORIGINAL_ERROR (rc=2 from
// Rust), letting the default parser fail and DuckDB call this stub.
//
// We re-run validation through sv_parse_function_rust which returns:
//   0 — defensive internal error; render as DISPLAY_EXTENSION_ERROR
//   1 — ours-but-invalid: error_buf populated; position is byte offset.
//       FF-16: also covers a panic inside the Rust hook (position
//       UINT32_MAX) — an extension bug must not masquerade as the user's
//       syntax error, which is what rc=2 on panic used to produce.
//   2 — not ours: DISPLAY_ORIGINAL_ERROR (let the default parser's error stand)
//   3 — valid-but-override-disabled: actionable hint; position=0
//
// `query` is one statement: v1.5 hands parse_function each `;`-separated
// statement the default parser failed on (trimmed, `;` re-appended when it had
// one). The registered parse_function is the version-specific `sv_parse_stub`
// below, which feeds this.
static ParserExtensionParseResult sv_parse_statement(const string &query) {
    // AR-7: sv_parse_function_rust no longer takes a context pointer — the
    // validation path never needed the catalog — so the SemanticViewsParserInfo
    // marker is not consulted here.
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint32_t position = UINT32_MAX;

    uint8_t rc = sv_parse_function_rust(
        query.c_str(), query.size(),
        error_buf, sizeof(error_buf),
        &position);

    switch (rc) {
        case 2:
            // Not ours — DISPLAY_ORIGINAL_ERROR (default parser's error
            // text + caret stands).
            return ParserExtensionParseResult();

        case 1:
        case 3: {
            // ours-but-invalid OR valid-but-override-disabled. Both want
            // DISPLAY_EXTENSION_ERROR with caret position, just different
            // message text. Construct the std::string explicitly first to
            // dodge C++'s "most vexing parse" — `ParserExtensionParseResult
            // result(string(error_buf));` would otherwise be parsed as a
            // function declaration.
            string msg(error_buf);
            ParserExtensionParseResult result(std::move(msg));
            if (position != UINT32_MAX) {
                result.error_location = optional_idx(position);
            }
            return result;
        }

        case 0: {
            // Defensive — sv_parse_function_rust currently never returns 0.
            // Map to DISPLAY_EXTENSION_ERROR with an internal-error message
            // rather than letting a silent default-parser error escape.
            return ParserExtensionParseResult(string(
                "semantic_views: internal error — sv_parse_function_rust "
                "returned rc=0 (please report this bug)"));
        }

        default: {
            // Defensive — unknown rc. Same handling as rc=0.
            return ParserExtensionParseResult(string(
                "semantic_views: internal error — unknown rc from "
                "sv_parse_function_rust"));
        }
    }
}

#ifdef SV_DUCKDB_2
// DuckDB 2.0 changed parse_function_t from `(info, const string &query)` to
// `(info, const vector<SimpleToken> &tokens)`: when the PEG parser fails on a
// statement, Parser::TryParseExtensionStatement hands every parse_function the
// token stream from the start of the failed statement to the end of the query
// (one SimpleToken — raw source text + TokenType — per token; comments are not
// tokens; `;` is a TERMINATOR token; an END_OF_INPUT sentinel ends it), and
// reads the answer from `ParserExtensionParseResult::consumed_tokens` rather
// than `type`:
//   < 0  throw ParserException::SyntaxError(query, error, error_location) —
//        the same call v1.5 made for DISPLAY_EXTENSION_ERROR;
//   == 0 not claimed: the next extension is tried and then the PEG error is
//        rethrown — v1.5's DISPLAY_ORIGINAL_ERROR;
//   > 0  claimed (v1.5's PARSE_SUCCESSFUL), which sv_parse_statement never
//        returns.
//
// So the stub recovers the statement text v1.5 passed (from the first token up
// to and including the TERMINATOR, as v1.5's statement splitter did) and maps
// the result onto consumed_tokens.
//
// SimpleToken carries the raw source text of each token but no offset, so the
// statement is found in the query the override recorded (sv_locate_tail): the
// tokens run to the end of the query, which pins where they start. With the
// statement's own text, the Rust validator sees exactly what the user wrote,
// error positions are offsets into the real query, and the #85 multi-statement
// lookup finds the statement as a substring.
//
// The caret is rendered into the message here rather than left to
// `error_location`: 2.0 only renders a parser error's location on the
// ClientContext query path, so the Python client (and the sqllogictest runner
// built on it) would show none, for DuckDB's own syntax errors as for ours.
//
// When the tokens cannot be located (parser_override switched off, so nothing
// was recorded) the text is rebuilt from the tokens instead: one space between
// tokens, none around `.` or inside brackets, no comments. A position in that
// text is not one in the query, so no caret is shown then.
// `newline`, when given, is set if the skipped trivia held a line break, as
// DuckDB's tokenizer reports on the token after it (preceded_by_newline).
static bool sv_skip_trivia(const std::string &q, idx_t &pos, bool *newline = nullptr) {
    auto note_newlines = [&](idx_t from, idx_t to) {
        for (idx_t i = from; newline != nullptr && i < to; i++) {
            if (StringUtil::CharacterIsNewline(q[i])) {
                *newline = true;
            }
        }
    };
    while (pos < q.size()) {
        char c = q[pos];
        if (StringUtil::CharacterIsSpace(c)) {
            note_newlines(pos, pos + 1);
            pos++;
        } else if (c == '-' && pos + 1 < q.size() && q[pos + 1] == '-') {
            while (pos < q.size() && q[pos] != '\n') {
                pos++;
            }
        } else if (c == '/' && pos + 1 < q.size() && q[pos + 1] == '*') {
            auto close = q.find("*/", pos + 2);
            if (close == std::string::npos) {
                return false;
            }
            note_newlines(pos, close);
            pos = close + 2;
        } else {
            break;
        }
    }
    return true;
}

static bool sv_is_end_token(const SimpleToken &token) {
    return token.type == TokenType::END_OF_INPUT ||
           token.type == TokenType::END_OF_INPUT_AUTOCOMPLETE;
}

// Where `tokens` -- the tail of `query` from the failed statement to its end --
// starts in `query`, with the offset just past the statement's TERMINATOR (or
// the end of its last token when there is none). False when they do not match
// the query's suffix token for token.
static bool sv_locate_tail(const std::string &query, const vector<SimpleToken> &tokens,
                           idx_t &start, idx_t &statement_end) {
    if (tokens.empty() || sv_is_end_token(tokens[0]) || tokens[0].text.empty()) {
        return false;
    }
    const std::string &first = tokens[0].text;
    for (auto candidate = query.rfind(first); candidate != std::string::npos;
         candidate = candidate == 0 ? std::string::npos : query.rfind(first, candidate - 1)) {
        idx_t pos = candidate;
        idx_t end = std::string::npos;
        bool matched = true;
        for (idx_t i = 0; i < tokens.size(); i++) {
            const auto &token = tokens[i];
            if (sv_is_end_token(token) || token.text.empty()) {
                continue;
            }
            if (i > 0 && !sv_skip_trivia(query, pos)) {
                matched = false;
                break;
            }
            if (query.compare(pos, token.text.size(), token.text) != 0) {
                matched = false;
                break;
            }
            pos += token.text.size();
            if (end == std::string::npos && token.type == TokenType::TERMINATOR) {
                end = pos;
            }
        }
        if (matched && sv_skip_trivia(query, pos) && pos == query.size()) {
            start = candidate;
            statement_end = end == std::string::npos ? pos : end;
            return true;
        }
    }
    return false;
}

static std::string sv_statement_from_tokens(const vector<SimpleToken> &tokens) {
    auto glue_left = [](const std::string &t) {  // no space before these
        return t == "." || t == "," || t == ")" || t == "]";
    };
    auto glue_right = [](const std::string &t) {  // no space after these
        return t == "." || t == "(" || t == "[";
    };
    std::string text;
    const std::string *prev = nullptr;
    for (const auto &token : tokens) {
        if (sv_is_end_token(token)) {
            break;
        }
        if (token.type == TokenType::TERMINATOR) {
            text += ';';
            break;
        }
        if (token.type == TokenType::COMMENT) {
            continue;  // the tokenizer drops comments; defensive
        }
        if (prev != nullptr && !glue_right(*prev) && !glue_left(token.text)) {
            text += ' ';
        }
        text += token.text;
        prev = &token.text;
    }
    return text;
}

static ParserExtensionParseResult sv_parse_stub(
    ParserExtensionInfo * /*info*/, const vector<SimpleToken> &tokens) {
    idx_t start = 0;
    idx_t statement_end = 0;
    const std::string &query = sv_override_query;
    bool located = sv_locate_tail(query, tokens, start, statement_end);
    auto result = sv_parse_statement(located ? query.substr(start, statement_end - start)
                                             : sv_statement_from_tokens(tokens));
    // DISPLAY_EXTENSION_ERROR -> throw; DISPLAY_ORIGINAL_ERROR keeps the
    // default 0 (not claimed). PARSE_SUCCESSFUL is never produced.
    if (result.type == ParserExtensionResultType::DISPLAY_EXTENSION_ERROR) {
        result.consumed_tokens = -1;
        if (located && result.error_location.IsValid()) {
            result.error = QueryErrorContext::Format(query, result.error,
                                                     start + result.error_location.GetIndex());
        }
        result.error_location = optional_idx();
    }
    // Whatever the answer, this query's parse is over for us: an error is
    // thrown, and an unclaimed statement makes DuckDB rethrow its own error.
    // (Only another extension claiming the statement lets the parse go on;
    // a later failing statement then gets no caret, as with the override off.)
    sv_override_query.clear();
    return result;
}
#else
static ParserExtensionParseResult sv_parse_stub(
    ParserExtensionInfo * /*info*/, const string &query) {
    return sv_parse_statement(query);
}
#endif

// ---------------------------------------------------------------------------
// Plan-function hook: sv_plan_unreachable  (Phase 62 Plan 03)
// ---------------------------------------------------------------------------
// ParserExtension carries a sibling `plan_function` pointer alongside
// `parse_function`. The plan function only fires when parse_function returns
// PARSE_SUCCESSFUL, which sv_parse_stub never does — every code path goes
// through DISPLAY_EXTENSION_ERROR or DISPLAY_ORIGINAL_ERROR. Provide a
// hard-fail stub so a contract violation surfaces loudly rather than silently.
//
// Signature must match plan_function_t in parser_extension_compat.hpp:
//   ParserExtensionPlanResult (*)(ParserExtensionInfo *, ClientContext &,
//                                  unique_ptr<ParserExtensionParseData>);
static ParserExtensionPlanResult sv_plan_unreachable(
    ParserExtensionInfo * /*info*/,
    ClientContext & /*context*/,
    unique_ptr<ParserExtensionParseData> /*parse_data*/) {
    throw InternalException(
        "semantic_views: sv_plan_unreachable called — sv_parse_stub never "
        "returns PARSE_SUCCESSFUL (please report this bug)");
}

// ---------------------------------------------------------------------------
// duckdb_functions() documentation (GitHub issue #233)
// ---------------------------------------------------------------------------
// An agent connected over SQL learns what an extension offers only through
// `duckdb_functions()`, so every function registered below carries a
// `FunctionDescription`. The entries live in one table keyed by function name
// rather than at each call site, so the C entry points keep their signatures
// and the registration helpers attach the docs uniformly. A registration whose
// name has no entry fails (see `sv_function_doc`): a new function cannot ship
// undocumented. `test/sql/function_descriptions.test` pins the rendered result
// and runs the examples.
//
// `positional_names` names only the positional arguments. For a table
// function DuckDB lists positional arguments and then named parameters under
// one `parameters` column, and applies `parameter_names` by index across both
// — so the registration core appends the named-parameter names itself, in the
// order DuckDB will render them (see `sv_table_function_description`).
//
// Most table functions are what the SHOW / DESCRIBE statements lower to
// (`crate::parse::rewrite::read_function_name`). Their descriptions and
// examples point at the statement, which is the documented interface.
struct SvFunctionDoc {
    const char *name;
    vector<string> positional_names;
    const char *description;
    vector<string> examples;
    vector<string> categories;
};

static const std::vector<SvFunctionDoc> &sv_function_docs() {
    static const std::vector<SvFunctionDoc> docs = {
        // --- Query functions -------------------------------------------
        {"semantic_view", {"view_name"},
         "Queries a semantic view: returns the requested dimensions, metrics "
         "and/or facts, generating the joins and GROUP BY from the view "
         "definition. Pass at least one of dimensions, metrics or facts; "
         "an item may name its column ('region AS r'). where_clause filters "
         "rows before aggregation, and column_order (e.g. ['metrics', "
         "'dimensions']) sets the order of the columns. The SEMANTIC_VIEW(...) "
         "construct is the same query written as SQL.",
         {"SELECT * FROM semantic_view('sales', dimensions := ['region'], "
          "metrics := ['revenue']);"},
         {"semantic_views", "query"}},
        {"explain_semantic_view", {"view_name"},
         "Shows how semantic_view() would answer the same arguments: the "
         "generated SQL, the materialization routing decision and the DuckDB "
         "query plan, as rows of text, without running the data query.",
         {"SELECT * FROM explain_semantic_view('sales', dimensions := "
          "['region'], metrics := ['revenue']);"},
         {"semantic_views", "query"}},
        // --- Definition export -----------------------------------------
        {"get_ddl", {"object_type", "object_name", "use_fully_qualified_names"},
         "Returns the CREATE OR REPLACE SEMANTIC VIEW statement that recreates "
         "a stored semantic view. object_type must be 'SEMANTIC_VIEW'; pass "
         "true as the optional third argument (use_fully_qualified_names) to "
         "schema-qualify the view name in the output.",
         {"SELECT GET_DDL('SEMANTIC_VIEW', 'sales');",
          "SELECT GET_DDL('SEMANTIC_VIEW', 'sales', true);"},
         {"semantic_views", "metadata"}},
        {"read_yaml_from_semantic_view", {"view_name"},
         "Returns a stored semantic view's definition as YAML, suitable for "
         "re-import with CREATE SEMANTIC VIEW ... FROM YAML.",
         {"SELECT READ_YAML_FROM_SEMANTIC_VIEW('sales');"},
         {"semantic_views", "metadata"}},
        // --- Listing -----------------------------------------------------
        {"list_semantic_views", {},
         "Backs SHOW SEMANTIC VIEWS: lists every registered semantic view with "
         "its creation time, database, schema and comment. Use that statement "
         "to list views; call this directly only to use the listing as a FROM "
         "source.",
         {"SHOW SEMANTIC VIEWS;",
          // Qualified and quoted per part: a bare name is a unique-match
          // lookup, ambiguous once two schemas hold the same view name.
          "SELECT GET_DDL('SEMANTIC_VIEW', '\"' || replace(schema_name, '\"', "
          "'\"\"') || '\".\"' || replace(name, '\"', '\"\"') || '\"') FROM "
          "list_semantic_views();"},
         {"semantic_views", "metadata"}},
        {"list_terse_semantic_views", {},
         "Backs SHOW TERSE SEMANTIC VIEWS: lists every registered semantic "
         "view without the comment column. Use that statement rather than "
         "calling this directly.",
         {"SHOW TERSE SEMANTIC VIEWS;"},
         {"semantic_views", "metadata"}},
        // --- Per-view introspection (DDL-backing) --------------------------
        {"describe_semantic_view", {"view_name"},
         "Backs DESCRIBE SEMANTIC VIEW: returns a semantic view's definition "
         "as one row per property of each table, relationship, fact, "
         "dimension, metric and materialization. Use that statement rather "
         "than calling this directly.",
         {"DESCRIBE SEMANTIC VIEW sales;"},
         {"semantic_views", "metadata"}},
        {"show_columns_in_semantic_view", {"view_name"},
         "Backs SHOW COLUMNS IN SEMANTIC VIEW: lists a semantic view's "
         "queryable dimensions, facts and metrics with their kind and "
         "expression (data_type is the declared type, empty for views created "
         "since v0.10.0). Use that statement rather than calling this "
         "directly.",
         {"SHOW COLUMNS IN SEMANTIC VIEW sales;"},
         {"semantic_views", "metadata"}},
        {"show_semantic_dimensions", {"view_name"},
         "Backs SHOW SEMANTIC DIMENSIONS IN <view>: lists the dimensions of "
         "one semantic view. Use that statement rather than calling this "
         "directly.",
         {"SHOW SEMANTIC DIMENSIONS IN sales;"},
         {"semantic_views", "metadata"}},
        {"show_semantic_metrics", {"view_name"},
         "Backs SHOW SEMANTIC METRICS IN <view>: lists the metrics of one "
         "semantic view. Use that statement rather than calling this directly.",
         {"SHOW SEMANTIC METRICS IN sales;"},
         {"semantic_views", "metadata"}},
        {"show_semantic_facts", {"view_name"},
         "Backs SHOW SEMANTIC FACTS IN <view>: lists the facts of one semantic "
         "view. Use that statement rather than calling this directly.",
         {"SHOW SEMANTIC FACTS IN sales;"},
         {"semantic_views", "metadata"}},
        {"show_semantic_materializations", {"view_name"},
         "Backs SHOW SEMANTIC MATERIALIZATIONS IN <view>: lists the "
         "materializations declared in one semantic view. Use that statement "
         "rather than calling this directly.",
         {"SHOW SEMANTIC MATERIALIZATIONS IN sales;"},
         {"semantic_views", "metadata"}},
        {"show_semantic_dimensions_for_metric", {"view_name", "metric_name"},
         "Backs SHOW SEMANTIC DIMENSIONS IN <view> FOR METRIC <metric>: lists "
         "the dimensions that can be combined with a metric without a fan "
         "trap, and which ones a window metric requires. Use that statement "
         "rather than calling this directly.",
         {"SHOW SEMANTIC DIMENSIONS IN sales FOR METRIC revenue;"},
         {"semantic_views", "metadata"}},
        // --- Cross-view introspection (DDL-backing) ------------------------
        {"show_semantic_dimensions_all", {},
         "Backs SHOW SEMANTIC DIMENSIONS without IN: lists the dimensions of "
         "every semantic view. Use that statement rather than calling this "
         "directly.",
         {"SHOW SEMANTIC DIMENSIONS;"},
         {"semantic_views", "metadata"}},
        {"show_semantic_metrics_all", {},
         "Backs SHOW SEMANTIC METRICS without IN: lists the metrics of every "
         "semantic view. Use that statement rather than calling this directly.",
         {"SHOW SEMANTIC METRICS;"},
         {"semantic_views", "metadata"}},
        {"show_semantic_facts_all", {},
         "Backs SHOW SEMANTIC FACTS without IN: lists the facts of every "
         "semantic view. Use that statement rather than calling this directly.",
         {"SHOW SEMANTIC FACTS;"},
         {"semantic_views", "metadata"}},
        {"show_semantic_materializations_all", {},
         "Backs SHOW SEMANTIC MATERIALIZATIONS without IN: lists the "
         "materializations of every semantic view. Use that statement rather "
         "than calling this directly.",
         {"SHOW SEMANTIC MATERIALIZATIONS;"},
         {"semantic_views", "metadata"}},
        // --- Internal ----------------------------------------------------
        {"__sv_compute_create_from_yaml", {"file_path", "view_name", "comment"},
         "Internal helper behind CREATE SEMANTIC VIEW ... FROM YAML FILE. Use "
         "that statement instead; this function is not for direct use.",
         {},
         {"semantic_views", "internal"}},
    };
    return docs;
}

// Look up the documentation entry for `name`; nullptr when there is none,
// which the registration helpers turn into a registration failure.
static const SvFunctionDoc *sv_function_doc(const std::string &name) {
    for (const auto &doc : sv_function_docs()) {
        if (name == doc.name) {
            return &doc;
        }
    }
    return nullptr;
}

// Sentence appended to every table function that declares the `search_path`
// named parameter, so a caller reading duckdb_functions() leaves it alone.
// The binds read the caller's search path themselves; the parameter remains
// because views stored by releases before 0.16 carry it in their SQL text.
static const char *const SV_SEARCH_PATH_NOTE =
    " The search_path parameter is reserved (kept so views stored by earlier "
    "releases still bind); do not pass it.";

static FunctionDescription sv_base_description(const SvFunctionDoc &doc) {
    FunctionDescription desc;
    desc.parameter_names = doc.positional_names;
    desc.description = doc.description;
    desc.examples = doc.examples;
    desc.categories = doc.categories;
    return desc;
}

// Build the description for a table function. `named_parameter_names` must
// list the named parameters in the order duckdb_functions() renders them (see
// `sv_compat::rendered_named_parameters` and its call site), so the names are
// appended in the same order and stay aligned with `parameter_types`.
static FunctionDescription sv_table_function_description(
    const SvFunctionDoc &doc, const std::vector<std::string> &named_parameter_names) {
    auto desc = sv_base_description(doc);
    bool has_search_path = false;
    for (const auto &np_name : named_parameter_names) {
        desc.parameter_names.push_back(np_name);
        has_search_path = has_search_path || np_name == "search_path";
    }
    if (has_search_path) {
        desc.description += SV_SEARCH_PATH_NOTE;
    }
    return desc;
}

// ---------------------------------------------------------------------------
// Table-function registration: shared spec-based core (C-7, code-review
// 2026-07-11)
// ---------------------------------------------------------------------------
// Every table function this extension installs goes through the C++ Catalog
// API with identical boilerplate: unwrap the `DatabaseWrapper`, build a
// `TableFunction`, set `ALTER_ON_CONFLICT`, and register it on the
// system-catalog transaction — all inside a try/catch that funnels failures
// into `error_buf` (D-08/D-09, never stderr). That boilerplate used to live in
// three places: the generic `sv_register_table_function` wrapper plus two
// hand-rolled impls for the named-parameter TFs (`explain_semantic_view`,
// `semantic_view`) that the generic wrapper could not express. `SvTableFunctionSpec`
// + `sv_register_table_function_core` own it once; the wrapper and the two named
// registrations are now thin spec builders that differ only in data.
struct SvTableFunctionSpec {
    const char *name = nullptr;
    const LogicalType *arg_types = nullptr;
    size_t arg_count = 0;
    // Named parameters, applied in order (empty for positional-only TFs).
    std::vector<std::pair<std::string, LogicalType>> named_params;
    table_function_bind_t bind_cb = nullptr;
    table_function_t exec_cb = nullptr;
    // D-05 (generalized): exactly one per-execution state constructor. A TF
    // uses init_local (single-shot / streaming via local state) XOR init_global
    // (semantic_view's per-execution global). The core enforces the XOR.
    table_function_init_local_t init_local_cb = nullptr;
    table_function_init_global_t init_global_cb = nullptr;
    // A TF whose bind hands DuckDB a table reference to bind in its place
    // (semantic_view). It sets this alone: no bind, exec or init callback.
    table_function_bind_replace_t bind_replace_cb = nullptr;
};

// The named LIST(VARCHAR) parameters shared by explain_semantic_view and
// semantic_view. Defined once (C-7) so the two TFs cannot drift from each
// other: this triple must stay byte-for-byte identical to the legacy Rust VTab
// signature (`dimensions`, `metrics`, `facts`) so existing call sites keep
// parsing.
static std::vector<std::pair<std::string, LogicalType>> sv_semantic_named_params() {
    auto list_varchar = LogicalType::LIST(LogicalType::VARCHAR);
    return {
        {"dimensions", list_varchar},
        {"metrics", list_varchar},
        {"facts", list_varchar},
        // The pre-aggregation predicate — Snowflake's `SEMANTIC_VIEW( … WHERE
        // <predicate> )`. Named `where_clause` rather than `where` because
        // DuckDB's parser reserves that keyword in named-parameter position:
        // `where := '…'` is a syntax error before the binder ever sees it.
        {"where_clause", LogicalType::VARCHAR},
        // The order the item lists' columns come out in, e.g. ['metrics',
        // 'dimensions']: Snowflake's SEMANTIC_VIEW(...) sets column order by
        // the order its clauses are written in, and the construct lowers to
        // this function. Lists left out follow in the default order
        // (dimensions, metrics, facts).
        {"column_order", list_varchar},
        // An explicit schema resolution order. Without it the bind reads the
        // caller's search path off its ClientContext (`sv_caller_search_path`).
        // Kept, reserved, because views stored by earlier releases carry the
        // `search_path := …` argument the parser override used to splice in
        // (TECH-DEBT #19); an explicit value still decides.
        {"search_path", list_varchar},
    };
}

// Shared registration core. `caller` names the outer entry point so
// diagnostics keep naming the specific function (e.g. "sv_register_semantic_view
// failed: ..."). Every failure path writes a NUL-terminated diagnostic into
// `error_buf` (never stderr — D-09) and returns false.
static bool sv_register_table_function_core(
    duckdb_database db_handle,
    const SvTableFunctionSpec &spec,
    const char *caller,
    char *error_buf, size_t error_buf_len) {
    auto write_err = [error_buf, error_buf_len](const std::string &msg) {
        if (error_buf == nullptr || error_buf_len == 0) {
            return;
        }
        snprintf(error_buf, error_buf_len, "%s", msg.c_str());
    };
    // Diagnostics name both the registration entry point (`caller`) and the
    // specific table function (`spec.name`), reproducing the pre-C-7
    // `sv_register_table_function('<tf_name>') ...` format so no failure path
    // — including the wrapper's — loses the TF name.
    const std::string who =
        std::string(caller != nullptr ? caller : "sv_register_table_function_core") +
        "('" + (spec.name != nullptr ? spec.name : "(null)") + "')";
    try {
        const bool replaces = spec.bind_replace_cb != nullptr;
        if (db_handle == nullptr || spec.name == nullptr ||
            (!replaces && (spec.bind_cb == nullptr || spec.exec_cb == nullptr))) {
            write_err(who + ": null required argument");
            return false;
        }
        // D-05 generalized (CR-02): every TF must have exactly one
        // per-execution state constructor so the single-shot/streaming exec
        // cannot double-emit or loop. init_local XOR init_global — both-null
        // reopens that hazard; both-set mixes the two state models. A
        // bind_replace TF has no scan of its own, so it sets none of them.
        const bool has_local = spec.init_local_cb != nullptr;
        const bool has_global = spec.init_global_cb != nullptr;
        if (replaces) {
            if (spec.bind_cb != nullptr || spec.exec_cb != nullptr || has_local || has_global) {
                write_err(who + ": a bind_replace table function sets no bind, exec or init callback");
                return false;
            }
        } else if (has_local == has_global) {
            write_err(who +
                ": exactly one of init_local / init_global must be non-null "
                "(D-05)");
            return false;
        }
        // Symmetric null guard for arg_types when arg_count > 0 (WR-01): a
        // buggy (nullptr, > 0) caller would otherwise segfault in the copy loop.
        if (spec.arg_count > 0 && spec.arg_types == nullptr) {
            write_err(who + ": arg_count > 0 but arg_types is null");
            return false;
        }
        auto *wrapper = reinterpret_cast<duckdb::DatabaseWrapper *>(
            db_handle->internal_ptr);
        if (wrapper == nullptr) {
            write_err(who + ": null DatabaseWrapper");
            return false;
        }
        auto &db = *wrapper->database->instance;

        vector<LogicalType> args;
        args.reserve(spec.arg_count);
        for (size_t i = 0; i < spec.arg_count; ++i) {
            args.push_back(spec.arg_types[i]);
        }

        // Six-arg TableFunction ctor: (name, args, function, bind,
        // init_global, init_local). Exactly one init callback is non-null
        // (enforced above); the other is nullptr.
        TableFunction tf(
            sv_compat::function_name(spec.name),
            std::move(args),
            spec.exec_cb,
            spec.bind_cb,
            spec.init_global_cb,
            spec.init_local_cb);

        tf.bind_replace = spec.bind_replace_cb;
        for (const auto &np : spec.named_params) {
            sv_compat::add_named_parameter(tf, np.first, np.second);
        }

        const SvFunctionDoc *doc = sv_function_doc(spec.name);
        if (doc == nullptr) {
            write_err(who +
                ": no duckdb_functions() documentation entry (add one to "
                "sv_function_docs in cpp/src/shim.cpp)");
            return false;
        }
        // DuckDB 2.0 renders a table function's parameter names from its
        // signature (no-op on v1.5, which takes them from the description).
        sv_compat::name_positional_parameters(tf, doc->positional_names);

        CreateTableFunctionInfo info(tf);
        // ALTER_ON_CONFLICT: extension reload replaces the registration cleanly
        // instead of throwing on duplicate name.
        info.on_conflict = OnCreateConflict::ALTER_ON_CONFLICT;
        // The named-parameter names must follow the iteration order DuckDB
        // renders, and that order is not stable across copies: `named_parameters`
        // is an unordered_map, and libc++ (macOS) reverses its iteration order
        // on every copy while libstdc++ (Linux) preserves it. duckdb_functions()
        // renders both `parameters` and `parameter_types` from
        // `entry.functions.GetFunctionByOffset(offset)` -- a by-value copy of
        // the catalog entry's function, which the entry *moves* out of
        // `info.functions` (TableFunctionCatalogEntry ctor, DuckDB 1.5.5). So a
        // GetFunctionByOffset copy of the info's function iterates exactly like
        // DuckDB's rendering copy. Iterating `info.functions.functions[0]`
        // directly is one copy short, and misaligned names and types on macOS.
        // (`sv_compat::rendered_named_parameters` makes that copy on v1.5; on
        // DuckDB 2.0 named parameters are an ordered list, so there is no
        // copy-order question.)
        info.descriptions.push_back(sv_table_function_description(
            *doc, sv_compat::rendered_named_parameters(info)));

        auto &system_catalog = Catalog::GetSystemCatalog(db);
        auto txn = CatalogTransaction::GetSystemTransaction(db);
        system_catalog.CreateTableFunction(txn, info);
        return true;
    } catch (const std::exception &e) {
        write_err(who + " failed: " + e.what());
        return false;
    } catch (...) {
        write_err(who + " failed: unknown C++ exception");
        return false;
    }
}

// ---------------------------------------------------------------------------
// sv_register_table_function — Phase 65 Plan 04 (A2 resolution)
// ---------------------------------------------------------------------------
// Reusable C-callable wrapper around the C++ Catalog API table-function
// registration pattern proven by `65-READ-PATH-SPIKE.md`. Bind callbacks
// registered via this path receive a native `ClientContext &` (not the
// duckdb-rs `BindInfo` wrapper which marshals `ClientContext` away), so
// they can open per-call `Connection(*context.db)` for catalog reads and
// YAML parsing without needing a long-lived extension-owned connection.
//
// Plan 04 consumes this to register `__sv_compute_create_from_yaml`. Plan 05
// will consume it (and add a scalar sibling) to migrate the 17 read-side
// callbacks off `query_conn` (H2).
//
// Header declaration in cpp/src/shim.hpp.
// Phase 65.1 Plan 02a (WR-02 D-08/D-09 + CR-02 D-05) — registration
// failures surface via the `(error_buf, error_buf_len)` trailing pair,
// the same ABI-stable channel used by `sv_parser_override_rust`
// (shim.cpp:57-61). Per D-09 there is NO stderr write — ADBC/JDBC/Python
// callers may have redirected stderr; `error_buf` is the only reliable
// path. D-05: `init_cb == nullptr` is rejected at registration time
// (forces every TF onto the single-shot-via-local-state path so the
// double-emit / unbounded-loop hazard in CR-02 cannot recur).
extern "C" {
    bool sv_register_table_function(
        duckdb_database db_handle,
        const char *name,
        const LogicalType *arg_types,
        size_t arg_count,
        table_function_bind_t bind_cb,
        table_function_t exec_cb,
        table_function_init_local_t init_cb,
        char *error_buf, size_t error_buf_len) {
        // Thin compatibility wrapper (C-7): preserves this entry point's
        // narrow, documented contract (shim.hpp) — its call sites are all
        // single-shot local-state TFs, so init_cb (init_local) is MANDATORY
        // here and the exact legacy diagnostic is kept. Construction, the
        // generalized D-05 XOR check, named-parameter population, and the
        // catalog transaction all live in the shared core above.
        if (init_cb == nullptr) {
            if (error_buf != nullptr && error_buf_len > 0) {
                snprintf(error_buf, error_buf_len,
                    "sv_register_table_function('%s'): null required "
                    "argument (init_cb is mandatory)",
                    name ? name : "(null)");
            }
            return false;
        }
        SvTableFunctionSpec spec;
        spec.name = name;
        spec.arg_types = arg_types;
        spec.arg_count = arg_count;
        spec.bind_cb = bind_cb;
        spec.exec_cb = exec_cb;
        spec.init_local_cb = init_cb;
        spec.init_global_cb = nullptr;
        // Every read TF registered through this wrapper accepts an explicit
        // schema resolution order (see `sv_search_path_payload`). Declared
        // uniformly rather than per-TF: views stored by earlier releases carry
        // it on any read-TF call, and a TF that takes no view name simply
        // ignores it. `semantic_view` and `explain_semantic_view` declare it
        // via `sv_semantic_named_params`.
        spec.named_params.emplace_back("search_path",
                                       LogicalType::LIST(LogicalType::VARCHAR));
        return sv_register_table_function_core(
            db_handle, spec, "sv_register_table_function", error_buf,
            error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// sv_register_scalar_function — Phase 65 Plan 05 (Task 2 Step A)
// ---------------------------------------------------------------------------
// Sibling of `sv_register_table_function`. Registers a scalar function via
// the C++ Catalog API (`Catalog::CreateFunction` on the system catalog with
// a `CreateScalarFunctionInfo`). Consumed by Task 4 / Wave 4 to migrate
// `get_ddl` and `read_yaml_from_semantic_view` off
// `register_scalar_function_with_state`.
//
// Header declaration in cpp/src/shim.hpp.
// Phase 65.1 Plan 02a (WR-02 D-08/D-09) — see sv_register_table_function
// for the rationale. Scalar functions have no `init_local` concept, so
// D-05 (null-init refusal) does NOT apply here; the required-arg set
// stays db_handle / name / exec_cb.
//
// TECH-DEBT #25 (GET_DDL qualified names): the catalog write is factored into
// `sv_register_scalar_function_set` so a function carrying more than one arity
// — `get_ddl`, whose third argument is optional — registers through the same
// guards and error-buffer contract as the single-arity callers. The C entry
// point below keeps its signature and wraps its one overload into a set.
static bool sv_register_scalar_function_set(
    duckdb_database db_handle,
    const char *name,
    ScalarFunctionSet set,
    char *error_buf, size_t error_buf_len) {
    auto write_err = [error_buf, error_buf_len](const char *fmt,
                                                const char *arg1) {
        if (error_buf == nullptr || error_buf_len == 0) {
            return;
        }
        snprintf(error_buf, error_buf_len, fmt, arg1 ? arg1 : "(null)");
    };
    try {
        if (db_handle == nullptr || name == nullptr) {
            write_err(
                "sv_register_scalar_function('%s'): null required argument",
                name);
            return false;
        }
        auto *wrapper = reinterpret_cast<duckdb::DatabaseWrapper *>(
            db_handle->internal_ptr);
        if (wrapper == nullptr) {
            write_err(
                "sv_register_scalar_function('%s'): null DatabaseWrapper",
                name);
            return false;
        }
        auto &db = *wrapper->database->instance;

        const SvFunctionDoc *doc = sv_function_doc(name);
        if (doc == nullptr) {
            write_err(
                "sv_register_scalar_function('%s'): no duckdb_functions() "
                "documentation entry (add one to sv_function_docs in "
                "cpp/src/shim.cpp)",
                name);
            return false;
        }
        // DuckDB 2.0 renders a scalar function's parameter names from each
        // overload's signature (no-op on v1.5, which takes them from the
        // description).
        sv_compat::name_positional_parameters(set, doc->positional_names);
        // Every scalar this extension registers reports errors by throwing.
        sv_compat::mark_fallible(set);

        CreateScalarFunctionInfo info(std::move(set));
        info.on_conflict = OnCreateConflict::ALTER_ON_CONFLICT;
        // One description, no parameter_types: DuckDB applies a lone
        // description to every overload and takes as many names as each
        // overload has arguments, so get_ddl's 2- and 3-argument forms both
        // render real names.
        info.descriptions.push_back(sv_base_description(*doc));

        auto &system_catalog = Catalog::GetSystemCatalog(db);
        auto txn = CatalogTransaction::GetSystemTransaction(db);
        system_catalog.CreateFunction(txn, info);
        return true;
    } catch (const std::exception &e) {
        if (error_buf != nullptr && error_buf_len > 0) {
            snprintf(error_buf, error_buf_len,
                "sv_register_scalar_function('%s') failed: %s",
                name ? name : "(null)", e.what());
        }
        return false;
    } catch (...) {
        write_err(
            "sv_register_scalar_function('%s') failed: unknown C++ exception",
            name);
        return false;
    }
}

extern "C" {
    bool sv_register_scalar_function(
        duckdb_database db_handle,
        const char *name,
        const LogicalType *arg_types,
        size_t arg_count,
        LogicalType return_type,
        scalar_function_t exec_cb,
        char *error_buf, size_t error_buf_len) {
        auto write_err = [error_buf, error_buf_len](const char *fmt,
                                                    const char *arg1) {
            if (error_buf == nullptr || error_buf_len == 0) {
                return;
            }
            snprintf(error_buf, error_buf_len, fmt, arg1 ? arg1 : "(null)");
        };
        try {
            if (name == nullptr || exec_cb == nullptr) {
                write_err(
                    "sv_register_scalar_function('%s'): null required argument",
                    name);
                return false;
            }
            // Phase 65.1 WR-01: symmetric null guard for arg_types when the
            // caller declares a non-zero arg_count. Same contract as the
            // table-function sibling above.
            if (arg_count > 0 && arg_types == nullptr) {
                write_err(
                    "sv_register_scalar_function('%s'): arg_count > 0 but "
                    "arg_types is null",
                    name);
                return false;
            }

            vector<LogicalType> args;
            args.reserve(arg_count);
            for (size_t i = 0; i < arg_count; ++i) {
                args.push_back(arg_types[i]);
            }

            // Braces, not parens: `ScalarFunctionSet set(std::string(name));`
            // is a function declaration (most vexing parse), not a variable.
            ScalarFunctionSet set{sv_compat::function_name(name)};
            set.AddFunction(ScalarFunction(sv_compat::function_name(name), std::move(args),
                                           return_type, exec_cb));
            return sv_register_scalar_function_set(
                db_handle, name, std::move(set), error_buf, error_buf_len);
        } catch (const std::exception &e) {
            if (error_buf != nullptr && error_buf_len > 0) {
                snprintf(error_buf, error_buf_len,
                    "sv_register_scalar_function('%s') failed: %s",
                    name ? name : "(null)", e.what());
            }
            return false;
        } catch (...) {
            write_err(
                "sv_register_scalar_function('%s') failed: unknown C++ exception",
                name);
            return false;
        }
    }
}

// ---------------------------------------------------------------------------
// __sv_compute_create_from_yaml — Phase 65 Plan 04 (Task 2 Step B) +
//                                  Phase 65.1 Plan 07 (CR-01 D-01..D-03)
// ---------------------------------------------------------------------------
// Helper table function registered via `sv_register_table_function`. The bind
// callback reads the YAML file directly through DuckDB's `FileSystem` API
// (`FileSystem::GetFileSystem(context).OpenFile(path, FileFlags::FILE_FLAGS_READ)`
// → `GetFileSize` → `Read`). It then calls the Rust FFI helper
// `sv_compute_create_from_yaml_rust` to parse + enrich + serialize the YAML
// into a metadata-less JSON definition. The exec callback emits the JSON
// as a single VARCHAR row.
//
// Phase 65.1 D-01..D-03 (CR-01): the previous shape used
// `Connection probe(*context.db)` + `Query("SELECT content FROM
// read_text('<path>')")`, which required SQL-quote-doubling on the path. That
// surface is gone: the file is now read directly via the `FileSystem` API.
// `LocalFileSystem` continues to honour the global `enable_external_access`
// DBConfig option when no `FileOpener` is supplied, so the access gate is
// preserved by construction. The four `BinderException` paths below all
// retain the `"FROM YAML FILE failed: "` prefix pinned by
// `test/sql/phase53_yaml_file.test`.
//
// The outer parser_override INSERT (src/parse.rs::rewrite_yaml_file_create,
// landed in Task 4) wraps the helper TF's row via `json_merge_patch(new_def,
// json_object('created_on', strftime(now(), '%Y-%m-%dT%H:%M:%SZ'),
// 'database_name', current_database(), 'schema_name', current_schema()))`
// so metadata fields are populated on the caller's connection, preserving
// the D-21 transactional contract.

struct CreateFromYamlBindData : public TableFunctionData {
    std::string file_path;
    std::string view_name;
    std::string comment;
    std::string new_def;
};

struct CreateFromYamlLocalState : public LocalTableFunctionState {
    bool emitted = false;
};

static unique_ptr<FunctionData> sv_create_from_yaml_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<CreateFromYamlBindData>();
    // FF-15 (code-review 2026-08-08): guard every positional argument before
    // GetValue<string>(), the same up-front check FF-4 gave every other table
    // function (see sv_run_varchar_bind_with_name / sv_semantic_view_bind).
    // `Value::GetValue<string>()` on a NULL Value renders the literal text
    // "NULL", so without this a NULL argument silently became a file named
    // `NULL` / a view named `NULL` instead of an error.
    //
    // The rewriter (src/parse/native_sql.rs::rewrite_yaml_file_create) always
    // emits string literals here, so this is only reachable by calling the
    // helper TF directly — hence the function-name-prefixed FF-4 wording
    // rather than the `"FROM YAML FILE failed: "` prefix the file-read paths
    // below use (that prefix belongs to the user-facing DDL surface and is
    // pinned by `test/sql/phase53_yaml_file.test`).
    // Exactly 3, not "at least 3": the message promises a fixed signature, and
    // a 4-argument call silently ignoring the extra is the kind of quiet
    // tolerance that makes a helper's contract untrue. DuckDB's own arity check
    // should reject it first, so this is defence in depth either way.
    if (input.inputs.size() != 3) {
        throw BinderException(
            "__sv_compute_create_from_yaml: expected 3 arguments "
            "(file_path, view_name, comment)");
    }
    if (input.inputs[0].IsNull()) {
        throw BinderException(
            "__sv_compute_create_from_yaml: file path is required "
            "(positional arg 0)");
    }
    if (input.inputs[1].IsNull()) {
        throw BinderException(
            "__sv_compute_create_from_yaml: view name is required "
            "(positional arg 1)");
    }
    if (input.inputs[2].IsNull()) {
        throw BinderException(
            "__sv_compute_create_from_yaml: comment is required "
            "(positional arg 2; pass '' for no comment)");
    }
    bd->file_path = input.inputs[0].GetValue<string>();
    bd->view_name = input.inputs[1].GetValue<string>();
    bd->comment   = input.inputs[2].GetValue<string>();

    // Phase 65.1 D-01..D-03 (CR-01): read the YAML file directly via the
    // `FileSystem` API rather than `Connection::Query("SELECT content FROM
    // read_text('<path>')")`. The SQL surface — including its quote-doubling
    // escape — is gone; `LocalFileSystem` honours `enable_external_access`
    // natively when no `FileOpener` is supplied, so the access gate is
    // preserved by construction. Every throw below keeps the
    // `"FROM YAML FILE failed: "` prefix pinned by
    // `test/sql/phase53_yaml_file.test`.
    auto &fs = FileSystem::GetFileSystem(context);
    unique_ptr<FileHandle> handle;
    try {
        handle = fs.OpenFile(bd->file_path, FileFlags::FILE_FLAGS_READ);
    } catch (const std::exception &e) {
        throw BinderException(
            std::string("FROM YAML FILE failed: ") + e.what());
    }
    int64_t size = fs.GetFileSize(*handle);
    if (size < 0) {
        throw BinderException(
            "FROM YAML FILE failed: GetFileSize returned -1 for '" +
            bd->file_path + "'");
    }
    // Phase 65.1 IN-04: fail-fast on hostile file sizes BEFORE allocating
    // `std::string(size, '\0')`. The Rust side enforces a 1 MiB cap via
    // `from_yaml_with_size_cap`, but without a C++ pre-check a multi-GB
    // YAML path would allocate that many bytes of std::string storage,
    // run the read, hand the buffer to Rust, and only then get rejected
    // — practical outcome on a memory-constrained host is OOM kill. The
    // C++ cap is intentionally LOOSER than the Rust cap (16 MiB vs
    // 1 MiB) so the Rust limit remains the single source of truth for
    // legitimate sizing decisions; this cap exists purely as
    // defence-in-depth against a hostile or accidentally-pathological
    // file size.
    constexpr int64_t MAX_YAML_BYTES = 16 * 1024 * 1024;
    if (size > MAX_YAML_BYTES) {
        throw BinderException(
            "FROM YAML FILE failed: file '" + bd->file_path + "' is " +
            std::to_string(size) + " bytes (max " +
            std::to_string(MAX_YAML_BYTES) + ")");
    }
    std::string yaml_content(static_cast<size_t>(size), '\0');
    int64_t got = (size == 0) ? 0
                              : fs.Read(*handle, yaml_content.data(), size);
    if (got != size) {
        throw BinderException(
            "FROM YAML FILE failed: short read (" + std::to_string(got) +
            " of " + std::to_string(size) + ") for '" + bd->file_path + "'");
    }

    // Bridge into Rust to parse + enrich + serialize. The Rust side enforces
    // YAML_SIZE_CAP (1 MiB) inside from_yaml_with_size_cap, which remains the
    // single source of truth for the legitimate size limit. The 16 MiB
    // pre-check above is defence-in-depth against a hostile file size that
    // would balloon the std::string allocation before Rust gets a chance to
    // reject it.
    char *out_ptr = nullptr;
    size_t out_len = 0;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));

    uint8_t rc = sv_compute_create_from_yaml_rust(
        reinterpret_cast<const uint8_t *>(yaml_content.data()),
        yaml_content.size(),
        reinterpret_cast<const uint8_t *>(bd->view_name.data()),
        bd->view_name.size(),
        reinterpret_cast<const uint8_t *>(bd->comment.data()),
        bd->comment.size(),
        &out_ptr,
        &out_len,
        error_buf,
        sizeof(error_buf));

    if (rc != 0 || out_ptr == nullptr) {
        // Match "FROM YAML FILE failed:" wording for size/parse errors so
        // the legacy phase53 sqllogictest assertions stay green.
        throw BinderException(
            std::string("FROM YAML FILE failed: ") + error_buf);
    }

    // Move the heap-owned UTF-8 buffer into bd->new_def, then release the
    // Rust allocation via sv_free_buffer (the exact (ptr, len) pair Rust
    // returned). The string copy is unavoidable because std::string demands
    // its own allocator-owned storage; for a 1 MiB cap this is bounded.
    bd->new_def.assign(out_ptr, out_len);
    sv_free_buffer(out_ptr, out_len);

    return_types.push_back(LogicalType::VARCHAR);
    names.emplace_back("new_def");
    return std::move(bd);
}

static unique_ptr<LocalTableFunctionState> sv_create_from_yaml_init_local(
    ExecutionContext & /*context*/,
    TableFunctionInitInput & /*input*/,
    GlobalTableFunctionState * /*global_state*/) {
    return make_uniq<CreateFromYamlLocalState>();
}

// Phase 65.1 D-07 (IN-01 refresh): this TF's single-shot emission is enforced
// via `init_local` — `sv_create_from_yaml_init_local` (defined just above)
// constructs the `CreateFromYamlLocalState` flag that the exec callback below
// reads to decide whether to emit. `sv_register_table_function` refuses null
// `init_cb` at registration time per Phase 65.1 D-05 (registration helper
// surfaces the refusal text "init_cb is mandatory" via the error_buf), so
// reaching the `InternalException` branch in the exec callback is a
// corrupted-state diagnostic rather than a normal failure mode. The earlier
// recommendation that previously lived here (to rewire registration through
// the init_local callback) is retired — D-05 enforcement (Plan 02) and D-04
// fallback removal (Plan 08) closed it.
static void sv_create_from_yaml_function(
    ClientContext & /*context*/,
    TableFunctionInput &data_p,
    DataChunk &output) {
    auto &bd = data_p.bind_data->Cast<CreateFromYamlBindData>();
    auto *state_p = data_p.local_state.get();
    if (state_p == nullptr) {
        // Registration refuses null init_cb at registration time per
        // Phase 65.1 D-05 (see `sv_register_table_function` in this file).
        // Reaching this branch indicates a corrupted TableFunctionInput;
        // fail loud rather than risk an unbounded row stream (Phase 65.1
        // D-04, CR-02).
        throw InternalException(
            "sv_create_from_yaml_function: local_state missing despite init_local registration");
    }
    auto &state = state_p->Cast<CreateFromYamlLocalState>();
    if (state.emitted) {
        output.SetCardinality(0);
        return;
    }
    output.SetValue(0, 0, Value(bd.new_def));
    output.SetCardinality(1);
    state.emitted = true;
}

// ---------------------------------------------------------------------------
// Shared read-side FFI wire-format readers — Phase 65 Plan 05
// ---------------------------------------------------------------------------
// Foundation shared by every migrated read-side table function: the generic
// VARCHAR / VARCHAR+BOOL scaffolds below, the `list_semantic_views` family,
// and the `semantic_view` bind. Introduced with the Wave-0
// `list_semantic_views` spike (Task 1) — the first read-side TF moved off
// duckdb-rs `register_table_function_with_extra_info` (which marshals
// `ClientContext &` away — Plan 01 Spike A6) onto the C++ Catalog API path
// via `sv_register_table_function` (Plan 04). See `65-05-SPIKE-SUMMARY.md` for
// the bridge design; the per-call `Connection probe` borrow contract is
// documented on the FFI dispatcher declarations above and applied by the
// shared bind helpers (`sv_run_varchar_bind` et al.) below.
//
// Wire format from the Rust dispatcher (AR-3 self-describing): a flat binary
// buffer beginning with a schema header of column type tags, then:
//   u32 col_count (little-endian), then col_count u8 type tags
//   u32 row_count (little-endian)
//   for each row, for each column:
//     u32 byte_len (little-endian) + byte_len bytes (UTF-8, NOT NUL-terminated)
// (a trailing BOOLEAN column, where present, is a single u8). The buffer is
// heap-owned by Rust; C++ parses it into BindData and immediately releases it
// via `sv_free_buffer`.

// Helper: read a little-endian u32 from buf[offset..offset+4] and advance.
// Throws BinderException on out-of-bounds — defensive against an FFI buffer
// truncated by a panic or allocation failure on the Rust side.
static uint32_t sv_read_u32_le(const char *buf, size_t buf_len, size_t &offset,
                               const char *fn_name) {
    if (offset + 4 > buf_len) {
        throw BinderException(
            std::string(fn_name) + ": FFI buffer truncated (expected u32 at offset " +
            std::to_string(offset) + " of " + std::to_string(buf_len) + ")");
    }
    auto p = reinterpret_cast<const unsigned char *>(buf + offset);
    uint32_t v = static_cast<uint32_t>(p[0])
               | (static_cast<uint32_t>(p[1]) << 8)
               | (static_cast<uint32_t>(p[2]) << 16)
               | (static_cast<uint32_t>(p[3]) << 24);
    offset += 4;
    return v;
}

static std::string sv_read_string(const char *buf, size_t buf_len, size_t &offset,
                                  const char *fn_name) {
    uint32_t len = sv_read_u32_le(buf, buf_len, offset, fn_name);
    if (offset + len > buf_len) {
        throw BinderException(
            std::string(fn_name) + ": FFI buffer truncated (expected " +
            std::to_string(len) + " string bytes at offset " +
            std::to_string(offset) + " of " + std::to_string(buf_len) + ")");
    }
    std::string s(buf + offset, len);
    offset += len;
    return s;
}

// Self-describing wire-format schema tags (AR-3). Kept in sync with
// `WIRE_TAG_VARCHAR` / `WIRE_TAG_BOOL` in `src/ddl/read_ffi.rs` — the Rust
// serializers emit these, the parsers below assert them.
static constexpr uint8_t SV_WIRE_VARCHAR = 1;
static constexpr uint8_t SV_WIRE_BOOL = 2;

// Read + validate the self-describing schema header (AR-3): `u32 col_count`
// followed by one `u8` type tag per column. `expected_tags` is the column
// layout the bind callback declared; a mismatch means the Rust dispatcher and
// this C++ bind disagree on the result shape — fail loudly at the header
// rather than misparse cell bytes downstream.
//
// An empty result set is serialized with `col_count == 0` and no tags (there
// are no rows, hence no cells to misalign); the schema check is skipped in
// that case. Returns the `col_count` read from the header so the caller can
// enforce the empty-encoding invariant (`col_count == 0` ⟺ `row_count == 0`)
// against the `row_count` that follows.
static uint32_t sv_read_wire_schema(const char *buf, size_t buf_len, size_t &offset,
                                    const std::vector<uint8_t> &expected_tags,
                                    const char *fn_name) {
    uint32_t col_count = sv_read_u32_le(buf, buf_len, offset, fn_name);
    if (col_count == 0) {
        return 0;  // empty result set — no schema to validate
    }
    if (col_count != expected_tags.size()) {
        throw BinderException(
            std::string(fn_name) + ": FFI wire-format column count mismatch (payload declares " +
            std::to_string(col_count) + " columns, bind expects " +
            std::to_string(expected_tags.size()) + ")");
    }
    if (offset + col_count > buf_len) {
        throw BinderException(
            std::string(fn_name) + ": FFI buffer truncated (expected " +
            std::to_string(col_count) + " schema tags at offset " +
            std::to_string(offset) + " of " + std::to_string(buf_len) + ")");
    }
    for (uint32_t c = 0; c < col_count; ++c) {
        uint8_t tag = static_cast<uint8_t>(buf[offset + c]);
        if (tag != expected_tags[c]) {
            throw BinderException(
                std::string(fn_name) + ": FFI wire-format column-type mismatch at column " +
                std::to_string(c) + " (payload tag " + std::to_string(tag) +
                ", bind expects " + std::to_string(expected_tags[c]) + ")");
        }
    }
    offset += col_count;
    return col_count;
}

// The `list_semantic_views` / `list_terse_semantic_views` table functions are
// defined below as thin adapters over the generic VARCHAR-rows scaffold. C-1
// (code-review 2026-07-11) folded the Wave-0 spike's bespoke structs,
// hand-rolled 6-string parse loop, and duplicate chunked emitter onto
// `sv_run_varchar_bind` + `sv_parse_varchar_payload` + `sv_emit_varchar_rows`
// — which also picks up the trailing-bytes wire check the bespoke parser
// lacked.

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 (Tasks 2-5) — Generic VARCHAR-rows + VARCHAR/BOOL-rows
// helpers for the remaining read-side TF migrations.
// ---------------------------------------------------------------------------
//
// Most of the Phase 65 Plan 05 migrations emit homogeneous VARCHAR result
// columns (the various SHOW SEMANTIC ... / DESCRIBE SEMANTIC VIEW shapes).
// To keep the per-TF migration delta to a thin adapter, we factor the
// payload-parse + bind-data shape + exec emitter into generic helpers here.
//
// Each migrated TF still owns its own bind callback (to declare the right
// column names and call the matching `sv_<name>_bind_rust` dispatcher), but
// the bulk of the bind + the entire exec/init are reused.

struct SvVarcharBindData : public TableFunctionData {
    std::vector<std::vector<std::string>> rows;
    size_t expected_cols = 0;
};

struct SvVarcharLocalState : public LocalTableFunctionState {
    // Next row to emit. Bind-materialized row sets can exceed one DataChunk's
    // capacity (STANDARD_VECTOR_SIZE = 2048), so the exec callback emits in
    // chunks and resumes from this cursor — a single-shot `emitted` flag
    // overflowed the chunk for >2048 rows (writes past the vector's data
    // buffer; no bounds check in release builds). Shared by
    // sv_emit_varchar_rows and sv_emit_varchar_bool_rows.
    idx_t next_row = 0;
};

// Parse a length-prefixed VARCHAR wire-format payload into bd.rows. The
// expected column count is taken from bd.expected_cols (must be set by the
// caller before invocation). Throws BinderException on truncated payload.
static void sv_parse_varchar_payload(const char *buf, size_t buf_len,
                                     SvVarcharBindData &bd,
                                     const char *fn_name) {
    if (buf == nullptr) {
        return;  // rc=0 with null buffer == zero rows
    }
    size_t offset = 0;
    // Validate the self-describing schema header (AR-3): all VARCHAR columns.
    uint32_t schema_cols =
        sv_read_wire_schema(buf, buf_len, offset,
                            std::vector<uint8_t>(bd.expected_cols, SV_WIRE_VARCHAR),
                            fn_name);
    uint32_t row_count = sv_read_u32_le(buf, buf_len, offset, fn_name);
    if (schema_cols == 0 && row_count != 0) {
        throw BinderException(
            std::string(fn_name) +
            ": FFI wire-format empty-schema header (col_count == 0) with row_count == " +
            std::to_string(row_count) + " > 0");
    }
    bd.rows.reserve(row_count);
    for (uint32_t r = 0; r < row_count; ++r) {
        std::vector<std::string> row;
        row.reserve(bd.expected_cols);
        for (size_t c = 0; c < bd.expected_cols; ++c) {
            row.push_back(sv_read_string(buf, buf_len, offset, fn_name));
        }
        bd.rows.push_back(std::move(row));
    }
    if (offset != buf_len) {
        throw BinderException(
            std::string(fn_name) +
            ": FFI buffer has trailing bytes (consumed " +
            std::to_string(offset) + " of " + std::to_string(buf_len) + ")");
    }
}

static unique_ptr<LocalTableFunctionState> sv_varchar_init_local(
    ExecutionContext & /*context*/,
    TableFunctionInitInput & /*input*/,
    GlobalTableFunctionState * /*global_state*/) {
    return make_uniq<SvVarcharLocalState>();
}

static void sv_emit_varchar_rows(
    ClientContext & /*context*/,
    TableFunctionInput &data_p,
    DataChunk &output) {
    auto &bd = data_p.bind_data->Cast<SvVarcharBindData>();
    auto *state_p = data_p.local_state.get();
    if (state_p == nullptr) {
        // Registration refuses null init_cb at registration time per
        // Phase 65.1 D-05 (see `sv_register_table_function` in this file).
        // Reaching this branch indicates a corrupted TableFunctionInput;
        // fail loud rather than risk an unbounded row stream (Phase 65.1
        // D-04, CR-02).
        throw InternalException(
            "sv_emit_varchar_rows: local_state missing despite init_local registration");
    }
    auto &state = state_p->Cast<SvVarcharLocalState>();
    idx_t total = bd.rows.size();
    if (state.next_row >= total) {
        output.SetCardinality(0);
        return;
    }
    idx_t remaining = total - state.next_row;
    idx_t count = remaining < STANDARD_VECTOR_SIZE ? remaining : STANDARD_VECTOR_SIZE;
    for (idx_t i = 0; i < count; ++i) {
        const auto &row = bd.rows[state.next_row + i];
        for (size_t c = 0; c < row.size(); ++c) {
            output.SetValue(c, i, Value(row[c]));
        }
    }
    output.SetCardinality(count);
    state.next_row += count;
}

// VARCHAR-rows-with-trailing-BOOL shape (used by
// show_semantic_dimensions_for_metric, which returns 3 VARCHAR + 1 BOOLEAN).
struct SvVarcharBoolBindData : public TableFunctionData {
    std::vector<std::pair<std::vector<std::string>, bool>> rows;
    size_t expected_varchar_cols = 0;  // number of VARCHAR cells per row
};

static void sv_parse_varchar_bool_payload(const char *buf, size_t buf_len,
                                          SvVarcharBoolBindData &bd,
                                          const char *fn_name) {
    if (buf == nullptr) {
        return;
    }
    size_t offset = 0;
    // Validate the self-describing schema header (AR-3): N VARCHAR columns
    // followed by a single trailing BOOLEAN column.
    std::vector<uint8_t> expected_tags(bd.expected_varchar_cols, SV_WIRE_VARCHAR);
    expected_tags.push_back(SV_WIRE_BOOL);
    uint32_t schema_cols = sv_read_wire_schema(buf, buf_len, offset, expected_tags, fn_name);
    uint32_t row_count = sv_read_u32_le(buf, buf_len, offset, fn_name);
    if (schema_cols == 0 && row_count != 0) {
        throw BinderException(
            std::string(fn_name) +
            ": FFI wire-format empty-schema header (col_count == 0) with row_count == " +
            std::to_string(row_count) + " > 0");
    }
    bd.rows.reserve(row_count);
    for (uint32_t r = 0; r < row_count; ++r) {
        std::vector<std::string> strs;
        strs.reserve(bd.expected_varchar_cols);
        for (size_t c = 0; c < bd.expected_varchar_cols; ++c) {
            strs.push_back(sv_read_string(buf, buf_len, offset, fn_name));
        }
        if (offset + 1 > buf_len) {
            throw BinderException(
                std::string(fn_name) +
                ": FFI buffer truncated (expected u8 bool at offset " +
                std::to_string(offset) + " of " + std::to_string(buf_len) + ")");
        }
        bool b = buf[offset] != 0;
        offset += 1;
        bd.rows.emplace_back(std::move(strs), b);
    }
    if (offset != buf_len) {
        throw BinderException(
            std::string(fn_name) +
            ": FFI buffer has trailing bytes (consumed " +
            std::to_string(offset) + " of " + std::to_string(buf_len) + ")");
    }
}

static void sv_emit_varchar_bool_rows(
    ClientContext & /*context*/,
    TableFunctionInput &data_p,
    DataChunk &output) {
    auto &bd = data_p.bind_data->Cast<SvVarcharBoolBindData>();
    auto *state_p = data_p.local_state.get();
    if (state_p == nullptr) {
        // Registration refuses null init_cb at registration time per
        // Phase 65.1 D-05 (see `sv_register_table_function` in this file).
        // Reaching this branch indicates a corrupted TableFunctionInput;
        // fail loud rather than risk an unbounded row stream (Phase 65.1
        // D-04, CR-02).
        throw InternalException(
            "sv_emit_varchar_bool_rows: local_state missing despite init_local registration");
    }
    auto &state = state_p->Cast<SvVarcharLocalState>();
    idx_t total = bd.rows.size();
    if (state.next_row >= total) {
        output.SetCardinality(0);
        return;
    }
    idx_t remaining = total - state.next_row;
    idx_t count = remaining < STANDARD_VECTOR_SIZE ? remaining : STANDARD_VECTOR_SIZE;
    for (idx_t i = 0; i < count; ++i) {
        const auto &strs = bd.rows[state.next_row + i].first;
        for (size_t c = 0; c < strs.size(); ++c) {
            output.SetValue(c, i, Value(strs[c]));
        }
        // BOOLEAN trailing column at index strs.size().
        output.SetValue(strs.size(), i, Value::BOOLEAN(bd.rows[state.next_row + i].second));
    }
    output.SetCardinality(count);
    state.next_row += count;
}

// ---------------------------------------------------------------------------
// Cancellation of the bind dispatchers (PR #248 review, TECH-DEBT #51)
// ---------------------------------------------------------------------------
//
// Every read-side bind below runs its Rust dispatcher on a fresh
// `Connection(*context.db)`, which carries its OWN `ClientContext` and
// therefore its own interrupt flag. `duckdb_interrupt()` (ADBC's cancel, the
// CLI's Ctrl-C, framework timeouts) sets the flag on the CALLER's context only.
// Since SHOW / DESCRIBE started probing every member's type at bind time
// (one `LIMIT 0` statement per member kind per view, with per-member
// fallbacks), a cancelled cross-view SHOW kept probing view after view, and a
// probe whose source binds slowly ran to completion — the INTERRUPT surfaced
// only afterwards. A bind has no executor task loop to poll between (a
// `LIMIT 0` probe is planned to an empty result — its cost is all bind and
// optimisation), so the flag is carried across as follows:
//
//   1. `sv_bind_caller_interrupted()` exposes the caller's flag to Rust, which
//      consults it before every probe and stops typing (`member_types`), so a
//      cross-view listing ends at the next probe.
//   2. A watcher thread forwards the caller's flag to the probe connection for
//      as long as the dispatcher runs. It re-forwards on every poll because each
//      statement on `probe` clears its own flag when it starts. The engine polls
//      that flag inside the work a slow probe actually does — glob expansion
//      (`LazyMultiFileList`), between optimiser passes, in the executor — so a
//      probe already running stops too, and its INTERRUPT error is classified as
//      a cancellation on the Rust side rather than as an untypable member.
//   3. Once the dispatcher returns, a cancelled caller wins: a bare
//      `InterruptException` is thrown, never the `"<fn> failed: ..."` wrapper,
//      so the error is an INTERRUPT with the "Interrupted!" message: ADBC
//      maps a failure to ADBC_STATUS_CANCELLED only on an exact match against
//      `InterruptException::INTERRUPT_MESSAGE`. Thrown from a
//      bind, the binder appends the call's query location, exactly as it does
//      when one of DuckDB's own table functions (`read_csv` over a glob) is
//      interrupted while binding.
//
// The scope is per bind call and per thread; it saves and restores any outer
// scope so a bind nested inside a probe (a source that is itself a semantic
// view function) keeps its own caller. Cost: one short-lived thread per bind.
// If the thread cannot be started, (1) and (3) still hold.
static thread_local ClientContext *sv_bind_caller = nullptr;

extern "C" bool sv_bind_caller_interrupted() {
    return sv_bind_caller != nullptr &&
           sv_compat::is_cancelled(*sv_bind_caller, std::memory_order_relaxed);
}

class SvBindCancellationScope {
public:
    SvBindCancellationScope(ClientContext &outer, Connection &probe)
        : outer_(outer), probe_(probe), prev_(sv_bind_caller) {
        sv_bind_caller = &outer_;
        try {
            watcher_ = std::thread([this] { Watch(); });
        } catch (const std::system_error &) {
            // No watcher: a running probe is not interrupted, but the Rust
            // side still stops before the next one and the throw below holds.
        }
    }

    ~SvBindCancellationScope() {
        if (watcher_.joinable()) {
            {
                std::lock_guard<std::mutex> lock(mutex_);
                stop_ = true;
            }
            wake_.notify_one();
            watcher_.join();
        }
        sv_bind_caller = prev_;
    }

    SvBindCancellationScope(const SvBindCancellationScope &) = delete;
    SvBindCancellationScope &operator=(const SvBindCancellationScope &) = delete;

    // A cancelled caller is authoritative however the dispatcher ended.
    void ThrowIfCancelled() const {
        sv_compat::throw_if_cancelled(outer_, std::memory_order_relaxed);
    }

private:
    void Watch() {
        std::unique_lock<std::mutex> lock(mutex_);
        while (!stop_) {
            if (sv_compat::is_cancelled(outer_, std::memory_order_relaxed)) {
                probe_.Interrupt();
            }
            wake_.wait_for(lock, std::chrono::milliseconds(10));
        }
    }

    ClientContext &outer_;
    Connection &probe_;
    ClientContext *prev_;
    std::thread watcher_;
    std::mutex mutex_;
    std::condition_variable wake_;
    bool stop_ = false;
};

// Common idiom for the bind callback: open Connection, run Rust dispatcher
// for the zero-arg case, parse payload into bd. Used by all 5 zero-arg
// migrated TFs (Task 2 / Wave 1) and reused by Task 3 / Wave 2 with extra
// VARCHAR args supplied to the dispatcher.
//
// `dispatcher` returns rc==0 on success (payload populated), rc!=0 with
// error_buf NUL-terminated otherwise. Throws BinderException on rc!=0 with
// the "<fn_name> failed: <error_buf>" message — matches the Wave 0 spike.
template <typename DispatcherFn>
static void sv_run_varchar_bind(ClientContext &context,
                                SvVarcharBindData &bd,
                                size_t expected_cols,
                                const char *fn_name,
                                DispatcherFn &&dispatcher) {
    bd.expected_cols = expected_cols;
    Connection probe(*context.db);
    SvBindCancellationScope cancellation(context, probe);
    duckdb_connection borrowed = reinterpret_cast<duckdb_connection>(&probe);
    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = dispatcher(borrowed,
                            &payload.ptr, &payload.len,
                            error_buf, sizeof(error_buf));
    cancellation.ThrowIfCancelled();
    if (rc != 0) {
        throw BinderException(std::string(fn_name) + " failed: " + error_buf);
    }
    sv_parse_varchar_payload(payload.ptr, payload.len, bd, fn_name);
}

// Defined further down, next to the other named-parameter marshalling it
// shares its wire format with; forward-declared here because the search-path
// helpers below are the first callers.
static std::vector<uint8_t> sv_serialise_string_list(const Value &list_val,
                                                     const char *param_name);

// The caller's schema resolution order, most significant first, in the shared
// varchar-list wire format: the schemas `SET search_path` / `USE` put on the
// path, then the current schema. That is what
// `list_concat(current_schemas(false), [current_schema()])` evaluates to, the
// expression the parser override used to splice into every read call (#19 /
// #25). It is read off the caller's `ClientContext`, a plain field read that
// takes no lock, so it works from a bind or a scalar's execution alike, and
// for calls that never pass through the parser override.
//
// `current_schemas(true)` is deliberately not mirrored: it prepends the temp
// schema, itself named `main`, which would shadow a real schema of that name.
static std::vector<uint8_t> sv_caller_search_path(ClientContext &context) {
#ifdef SV_DUCKDB_2
    auto &search_path = ClientData::Get(context).catalog_search_path;
    vector<Value> schemas;
    for (const auto &entry : search_path->GetSetPaths()) {
        schemas.emplace_back(entry.GetSchema());
    }
    schemas.emplace_back(search_path->GetDefault().GetSchema());
    return sv_serialise_string_list(Value::LIST(LogicalType::VARCHAR, std::move(schemas)),
                                    "search_path");
#else
    // v1.5's amalgamation only forward-declares ClientData; the Rust side reads
    // an empty buffer as "no path" and falls back to the unique match.
    (void)context;
    return {};
#endif
}

// A table function's resolution order: an explicit `search_path` argument if
// the call carries one (a view stored by an earlier release has the spliced-in
// expression in its text, which evaluates to the same order), else the
// caller's search path.
static std::vector<uint8_t> sv_search_path_payload(ClientContext &context,
                                                   TableFunctionBindInput &input) {
    auto it = input.named_parameters.find("search_path");
    if (it == input.named_parameters.end() || it->second.IsNull()) {
        return sv_caller_search_path(context);
    }
    return sv_serialise_string_list(it->second, "search_path");
}

// Variant for TFs with one VARCHAR argument (the view name). Extracts the
// name from input.inputs[0] and forwards it as (ptr, len) to the dispatcher
// alongside the borrowed connection handle.
template <typename DispatcherFn>
static void sv_run_varchar_bind_with_name(ClientContext &context,
                                          TableFunctionBindInput &input,
                                          SvVarcharBindData &bd,
                                          size_t expected_cols,
                                          const char *fn_name,
                                          DispatcherFn &&dispatcher) {
    bd.expected_cols = expected_cols;
    // FF-4: guard the positional view-name argument. Without this a NULL
    // argument (e.g. describe_semantic_view(NULL)) reached GetValue<string>()
    // and surfaced as the confusing "view 'NULL' does not exist"; match the
    // semantic_view() binder's up-front BinderException instead.
    if (input.inputs.empty() || input.inputs[0].IsNull()) {
        throw BinderException(std::string(fn_name) +
                              ": view name is required (positional arg 0)");
    }
    std::string name = input.inputs[0].GetValue<std::string>();
    std::vector<uint8_t> sp = sv_search_path_payload(context, input);
    Connection probe(*context.db);
    SvBindCancellationScope cancellation(context, probe);
    duckdb_connection borrowed = reinterpret_cast<duckdb_connection>(&probe);
    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = dispatcher(borrowed,
                            reinterpret_cast<const uint8_t *>(name.data()), name.size(),
                            sp.empty() ? nullptr : sp.data(), sp.size(),
                            &payload.ptr, &payload.len,
                            error_buf, sizeof(error_buf));
    cancellation.ThrowIfCancelled();
    if (rc != 0) {
        throw BinderException(std::string(fn_name) + " failed: " + error_buf);
    }
    sv_parse_varchar_payload(payload.ptr, payload.len, bd, fn_name);
}

// Variant for TFs with two VARCHAR arguments (view_name, metric_name).
template <typename DispatcherFn>
static void sv_run_varchar_bool_bind_with_two_names(
    ClientContext &context,
    TableFunctionBindInput &input,
    SvVarcharBoolBindData &bd,
    size_t expected_varchar_cols,
    const char *fn_name,
    DispatcherFn &&dispatcher) {
    bd.expected_varchar_cols = expected_varchar_cols;
    // FF-4: guard both positional arguments (view_name, metric_name) before
    // GetValue<string>() so a NULL argument raises a clear BinderException
    // rather than resolving to a spurious "'NULL' does not exist".
    if (input.inputs.size() < 2 || input.inputs[0].IsNull() ||
        input.inputs[1].IsNull()) {
        throw BinderException(
            std::string(fn_name) +
            ": view name and metric name are required (positional args 0, 1)");
    }
    std::string name1 = input.inputs[0].GetValue<std::string>();
    std::string name2 = input.inputs[1].GetValue<std::string>();
    std::vector<uint8_t> sp = sv_search_path_payload(context, input);
    Connection probe(*context.db);
    SvBindCancellationScope cancellation(context, probe);
    duckdb_connection borrowed = reinterpret_cast<duckdb_connection>(&probe);
    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = dispatcher(borrowed,
                            reinterpret_cast<const uint8_t *>(name1.data()), name1.size(),
                            reinterpret_cast<const uint8_t *>(name2.data()), name2.size(),
                            sp.empty() ? nullptr : sp.data(), sp.size(),
                            &payload.ptr, &payload.len,
                            error_buf, sizeof(error_buf));
    cancellation.ThrowIfCancelled();
    if (rc != 0) {
        throw BinderException(std::string(fn_name) + " failed: " + error_buf);
    }
    sv_parse_varchar_bool_payload(payload.ptr, payload.len, bd, fn_name);
}

// ---------------------------------------------------------------------------
// list_semantic_views — Phase 65 Plan 05 Task 1 (Wave 0 spike), folded onto
// the generic scaffold (C-1, code-review 2026-07-11)
// ---------------------------------------------------------------------------
// 6-column VARCHAR: created_on, name, kind, database_name, schema_name,
// comment. Superset of list_terse_semantic_views (which drops `comment`);
// both share the Rust body `list_view_rows` and the same wire format, so the
// strict generic parser (`sv_parse_varchar_payload`, incl. the trailing-bytes
// check) applies uniformly.

static unique_ptr<FunctionData> sv_list_semantic_views_bind(
    ClientContext &context,
    TableFunctionBindInput & /*input*/,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    // Column layout must match the v0.9.0 Rust VTab exactly — SELECT * FROM
    // list_semantic_views() across the suite relies on byte-identical names
    // and order.
    static const char *const COL_NAMES[] = {
        "created_on", "name", "kind", "database_name", "schema_name", "comment",
    };
    for (auto cn : COL_NAMES) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind(
        context, *bd, /*expected_cols*/ 6, "list_semantic_views",
        [](duckdb_connection borrowed, char **out_ptr, size_t *out_len,
           char *error_buf, size_t error_buf_len) {
            return sv_list_semantic_views_bind_rust(
                borrowed, out_ptr, out_len, error_buf, error_buf_len);
        });
    return std::move(bd);
}

extern "C" {
    bool sv_register_list_semantic_views(duckdb_database db_handle,
                                         char *error_buf, size_t error_buf_len) {
        // Zero-argument table function — no arg_types array.
        return sv_register_table_function(
            db_handle,
            "list_semantic_views",
            /*arg_types*/ nullptr, /*arg_count*/ 0,
            sv_list_semantic_views_bind,
            sv_emit_varchar_rows,
            sv_varchar_init_local,
            error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// list_terse_semantic_views — Phase 65 Plan 05 Task 2 (Wave 1)
// ---------------------------------------------------------------------------
// 5-column subset of list_semantic_views — same bridge mechanism, no
// per-function BindData struct (uses generic SvVarcharBindData via the
// sv_run_varchar_bind helper).
//
// Columns: created_on, name, kind, database_name, schema_name.

static unique_ptr<FunctionData> sv_list_terse_semantic_views_bind(
    ClientContext &context,
    TableFunctionBindInput & /*input*/,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COL_NAMES[] = {
        "created_on", "name", "kind", "database_name", "schema_name",
    };
    for (auto cn : COL_NAMES) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind(
        context, *bd, /*expected_cols*/ 5, "list_terse_semantic_views",
        [](duckdb_connection borrowed, char **out_ptr, size_t *out_len,
           char *error_buf, size_t error_buf_len) {
            return sv_list_terse_semantic_views_bind_rust(
                borrowed, out_ptr, out_len, error_buf, error_buf_len);
        });
    return std::move(bd);
}

extern "C" {
    bool sv_register_list_terse_semantic_views(duckdb_database db_handle,
                                               char *error_buf, size_t error_buf_len) {
        return sv_register_table_function(
            db_handle,
            "list_terse_semantic_views",
            /*arg_types*/ nullptr, /*arg_count*/ 0,
            sv_list_terse_semantic_views_bind,
            sv_emit_varchar_rows,
            sv_varchar_init_local,
            error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// show_semantic_dimensions_all — Phase 65 Plan 05 Task 2 (Wave 1)
// ---------------------------------------------------------------------------
// 8-column VARCHAR: database_name, schema_name, semantic_view_name,
// table_name, name, data_type, synonyms, comment.

static unique_ptr<FunctionData> sv_show_semantic_dimensions_all_bind(
    ClientContext &context,
    TableFunctionBindInput & /*input*/,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name", "table_name",
        "name", "data_type", "synonyms", "comment",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind(
        context, *bd, /*expected_cols*/ 8, "show_semantic_dimensions_all",
        [](duckdb_connection borrowed, char **out_ptr, size_t *out_len,
           char *error_buf, size_t error_buf_len) {
            return sv_show_semantic_dimensions_all_bind_rust(
                borrowed, out_ptr, out_len, error_buf, error_buf_len);
        });
    return std::move(bd);
}

extern "C" {
    bool sv_register_show_semantic_dimensions_all(duckdb_database db_handle,
                                                  char *error_buf, size_t error_buf_len) {
        return sv_register_table_function(
            db_handle, "show_semantic_dimensions_all",
            nullptr, 0,
            sv_show_semantic_dimensions_all_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// show_semantic_metrics_all — Phase 65 Plan 05 Task 2 (Wave 1)
// ---------------------------------------------------------------------------
// 8-column VARCHAR (same schema as show_semantic_dimensions_all).

static unique_ptr<FunctionData> sv_show_semantic_metrics_all_bind(
    ClientContext &context,
    TableFunctionBindInput & /*input*/,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name", "table_name",
        "name", "data_type", "synonyms", "comment",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind(
        context, *bd, 8, "show_semantic_metrics_all",
        [](duckdb_connection borrowed, char **out_ptr, size_t *out_len,
           char *error_buf, size_t error_buf_len) {
            return sv_show_semantic_metrics_all_bind_rust(
                borrowed, out_ptr, out_len, error_buf, error_buf_len);
        });
    return std::move(bd);
}

extern "C" {
    bool sv_register_show_semantic_metrics_all(duckdb_database db_handle,
                                               char *error_buf, size_t error_buf_len) {
        return sv_register_table_function(
            db_handle, "show_semantic_metrics_all",
            nullptr, 0,
            sv_show_semantic_metrics_all_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// show_semantic_facts_all — Phase 65 Plan 05 Task 2 (Wave 1)
// ---------------------------------------------------------------------------
// 8-column VARCHAR (same schema as show_semantic_dimensions_all).

static unique_ptr<FunctionData> sv_show_semantic_facts_all_bind(
    ClientContext &context,
    TableFunctionBindInput & /*input*/,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name", "table_name",
        "name", "data_type", "synonyms", "comment",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind(
        context, *bd, 8, "show_semantic_facts_all",
        [](duckdb_connection borrowed, char **out_ptr, size_t *out_len,
           char *error_buf, size_t error_buf_len) {
            return sv_show_semantic_facts_all_bind_rust(
                borrowed, out_ptr, out_len, error_buf, error_buf_len);
        });
    return std::move(bd);
}

extern "C" {
    bool sv_register_show_semantic_facts_all(duckdb_database db_handle,
                                             char *error_buf, size_t error_buf_len) {
        return sv_register_table_function(
            db_handle, "show_semantic_facts_all",
            nullptr, 0,
            sv_show_semantic_facts_all_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// show_semantic_materializations_all — Phase 65 Plan 05 Task 2 (Wave 1)
// ---------------------------------------------------------------------------
// 7-column VARCHAR: database_name, schema_name, semantic_view_name, name,
// table, dimensions, metrics.

static unique_ptr<FunctionData> sv_show_semantic_materializations_all_bind(
    ClientContext &context,
    TableFunctionBindInput & /*input*/,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name",
        "name", "table", "dimensions", "metrics",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind(
        context, *bd, 7, "show_semantic_materializations_all",
        [](duckdb_connection borrowed, char **out_ptr, size_t *out_len,
           char *error_buf, size_t error_buf_len) {
            return sv_show_semantic_materializations_all_bind_rust(
                borrowed, out_ptr, out_len, error_buf, error_buf_len);
        });
    return std::move(bd);
}

extern "C" {
    bool sv_register_show_semantic_materializations_all(duckdb_database db_handle,
                                                        char *error_buf, size_t error_buf_len) {
        return sv_register_table_function(
            db_handle, "show_semantic_materializations_all",
            nullptr, 0,
            sv_show_semantic_materializations_all_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// Wave 2 single-arg TFs — Phase 65 Plan 05 Task 3
// ---------------------------------------------------------------------------
// show_columns_in_semantic_view, describe_semantic_view, and the 4
// single-view show_semantic_<entity> variants all take one VARCHAR arg
// (the view name) and emit homogeneous VARCHAR rows. The single
// show_semantic_dimensions_for_metric takes two VARCHAR args and emits
// VARCHAR + trailing BOOL rows (handled in the dedicated block further
// down).

static unique_ptr<FunctionData> sv_show_columns_in_semantic_view_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name", "column_name",
        "data_type", "kind", "expression", "comment",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind_with_name(
        context, input, *bd, 8, "show_columns_in_semantic_view",
        [](duckdb_connection borrowed,
           const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_columns_in_semantic_view_bind_rust(
                borrowed, np, nl, sp, spl, op, ol, eb, ebl);
        });
    return std::move(bd);
}

static unique_ptr<FunctionData> sv_describe_semantic_view_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "object_kind", "object_name", "parent_entity", "property", "property_value",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind_with_name(
        context, input, *bd, 5, "describe_semantic_view",
        [](duckdb_connection borrowed,
           const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_describe_semantic_view_bind_rust(
                borrowed, np, nl, sp, spl, op, ol, eb, ebl);
        });
    return std::move(bd);
}

static unique_ptr<FunctionData> sv_show_semantic_dimensions_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name", "table_name",
        "name", "data_type", "synonyms", "comment",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind_with_name(
        context, input, *bd, 8, "show_semantic_dimensions",
        [](duckdb_connection borrowed,
           const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_dimensions_bind_rust(
                borrowed, np, nl, sp, spl, op, ol, eb, ebl);
        });
    return std::move(bd);
}

static unique_ptr<FunctionData> sv_show_semantic_metrics_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name", "table_name",
        "name", "data_type", "synonyms", "comment",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind_with_name(
        context, input, *bd, 8, "show_semantic_metrics",
        [](duckdb_connection borrowed,
           const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_metrics_bind_rust(
                borrowed, np, nl, sp, spl, op, ol, eb, ebl);
        });
    return std::move(bd);
}

static unique_ptr<FunctionData> sv_show_semantic_facts_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name", "table_name",
        "name", "data_type", "synonyms", "comment",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind_with_name(
        context, input, *bd, 8, "show_semantic_facts",
        [](duckdb_connection borrowed,
           const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_facts_bind_rust(
                borrowed, np, nl, sp, spl, op, ol, eb, ebl);
        });
    return std::move(bd);
}

static unique_ptr<FunctionData> sv_show_semantic_materializations_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    static const char *const COLS[] = {
        "database_name", "schema_name", "semantic_view_name",
        "name", "table", "dimensions", "metrics",
    };
    for (auto cn : COLS) {
        return_types.push_back(LogicalType::VARCHAR);
        names.emplace_back(cn);
    }
    sv_run_varchar_bind_with_name(
        context, input, *bd, 7, "show_semantic_materializations",
        [](duckdb_connection borrowed,
           const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_materializations_bind_rust(
                borrowed, np, nl, sp, spl, op, ol, eb, ebl);
        });
    return std::move(bd);
}

// show_semantic_dimensions_for_metric: 3 VARCHAR + 1 BOOL output, 2 VARCHAR
// input args.
static unique_ptr<FunctionData> sv_show_semantic_dimensions_for_metric_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBoolBindData>();
    return_types.push_back(LogicalType::VARCHAR);
    names.emplace_back("table_name");
    return_types.push_back(LogicalType::VARCHAR);
    names.emplace_back("name");
    return_types.push_back(LogicalType::VARCHAR);
    names.emplace_back("data_type");
    return_types.push_back(LogicalType::BOOLEAN);
    names.emplace_back("required");
    sv_run_varchar_bool_bind_with_two_names(
        context, input, *bd, /*expected_varchar_cols*/ 3,
        "show_semantic_dimensions_for_metric",
        [](duckdb_connection borrowed,
           const uint8_t *vn, size_t vnl,
           const uint8_t *mn, size_t mnl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_dimensions_for_metric_bind_rust(
                borrowed, vn, vnl, mn, mnl, sp, spl, op, ol, eb, ebl);
        });
    return std::move(bd);
}

extern "C" {
    bool sv_register_show_columns_in_semantic_view(duckdb_database db_handle,
                                                   char *error_buf, size_t error_buf_len) {
        LogicalType args[] = {LogicalType::VARCHAR};
        return sv_register_table_function(
            db_handle, "show_columns_in_semantic_view",
            args, 1,
            sv_show_columns_in_semantic_view_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
    bool sv_register_describe_semantic_view(duckdb_database db_handle,
                                            char *error_buf, size_t error_buf_len) {
        LogicalType args[] = {LogicalType::VARCHAR};
        return sv_register_table_function(
            db_handle, "describe_semantic_view",
            args, 1,
            sv_describe_semantic_view_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
    bool sv_register_show_semantic_dimensions(duckdb_database db_handle,
                                              char *error_buf, size_t error_buf_len) {
        LogicalType args[] = {LogicalType::VARCHAR};
        return sv_register_table_function(
            db_handle, "show_semantic_dimensions",
            args, 1,
            sv_show_semantic_dimensions_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
    bool sv_register_show_semantic_metrics(duckdb_database db_handle,
                                           char *error_buf, size_t error_buf_len) {
        LogicalType args[] = {LogicalType::VARCHAR};
        return sv_register_table_function(
            db_handle, "show_semantic_metrics",
            args, 1,
            sv_show_semantic_metrics_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
    bool sv_register_show_semantic_facts(duckdb_database db_handle,
                                         char *error_buf, size_t error_buf_len) {
        LogicalType args[] = {LogicalType::VARCHAR};
        return sv_register_table_function(
            db_handle, "show_semantic_facts",
            args, 1,
            sv_show_semantic_facts_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
    bool sv_register_show_semantic_materializations(duckdb_database db_handle,
                                                    char *error_buf, size_t error_buf_len) {
        LogicalType args[] = {LogicalType::VARCHAR};
        return sv_register_table_function(
            db_handle, "show_semantic_materializations",
            args, 1,
            sv_show_semantic_materializations_bind,
            sv_emit_varchar_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
    bool sv_register_show_semantic_dimensions_for_metric(duckdb_database db_handle,
                                                         char *error_buf, size_t error_buf_len) {
        LogicalType args[] = {LogicalType::VARCHAR, LogicalType::VARCHAR};
        return sv_register_table_function(
            db_handle, "show_semantic_dimensions_for_metric",
            args, 2,
            sv_show_semantic_dimensions_for_metric_bind,
            sv_emit_varchar_bool_rows, sv_varchar_init_local,
            error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 Task 4 (Wave 3) — scalar function migrations
// ---------------------------------------------------------------------------
//
// Migrate the two read-side scalars (`get_ddl`, `read_yaml_from_semantic_view`)
// from duckdb-rs `register_scalar_function_with_state` to the C++ Catalog
// API path established by `sv_register_scalar_function` (Wave 1 prep). Each
// exec callback opens a per-call `Connection probe(*state.GetContext().db)`
// (the scalar analog of the bind-side `Connection(*context.db)` used by the
// 15 TF migrations) and bridges to a Rust dispatcher that performs the
// lookup + render on the borrowed connection. The borrow contract (Rust
// MUST NOT call `duckdb_disconnect`) is identical to the TF dispatchers —
// see `src/ddl/read_ffi.rs` module docs.
//
// Per-chunk Connection construction (one `Connection probe` per exec call,
// reused across every row in the chunk) keeps the dispatcher shape uniform
// with the TF migrations: each call into the C++ exec callback gets a
// fresh stack-owned Connection bound to the caller's Database, and every
// row in the chunk borrows that same handle through the reinterpret_cast
// bridge. Scalar usage in practice is one or two rows
// (`SELECT GET_DDL(...)`), so amortising the Connection ctor across the
// chunk is essentially free even at chunk size 1 — and the ctor itself is
// sub-millisecond per the READ-PATH-SPIKE evidence.

// Common helper: copy a Rust-owned UTF-8 buffer into the result Vector at
// `row_idx` via StringVector::AddString (which allocates inside the Vector's
// string heap). Throws InvalidInputException on rc!=0 with the dispatcher's
// error_buf as the message. The owned buffer is always freed via
// `sv_free_buffer` regardless of rc.
template <typename DispatcherFn>
static void sv_emit_scalar_row(Vector &result, idx_t row_idx,
                               const char *fn_name,
                               DispatcherFn &&dispatcher) {
    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = dispatcher(&payload.ptr, &payload.len,
                            error_buf, sizeof(error_buf));
    if (rc != 0) {
        throw InvalidInputException(std::string(fn_name) + ": " + error_buf);
    }
    // payload.ptr is non-null on rc==0 (publish_owned_buffer guarantees this
    // when out_ptr is non-null); len may be 0 for an empty string. The null
    // branch is purely defensive, but AddString(data, len) copies `len` bytes,
    // so the fallback length MUST be 0 too — otherwise a (nullptr, len>0)
    // payload would read `len` bytes past the 1-byte "" literal (C-9,
    // code-review 2026-07-11).
    const char *out_data = payload.ptr == nullptr ? "" : payload.ptr;
    idx_t out_len = payload.ptr == nullptr ? 0 : payload.len;
    string_t out_value = StringVector::AddString(result, out_data, out_len);
    sv_compat::flat_data_mut<string_t>(result)[row_idx] = out_value;
}

// get_ddl(object_type VARCHAR, name VARCHAR [, use_fully_qualified_names
// BOOLEAN]) -> VARCHAR
//
// Both arities land here; `args.ColumnCount()` distinguishes them. The third
// argument mirrors Snowflake's `use_fully_qualified_names`: when true the
// rendered CREATE name carries the view's own schema, so re-running the output
// restores the view where it came from rather than into the executing
// session's schema.
static void sv_get_ddl_exec(DataChunk &args, ExpressionState &state,
                            Vector &result) {
    const bool has_qualify_arg = args.ColumnCount() >= 3;
    auto &type_vec = args.data[0];
    auto &name_vec = args.data[1];
    type_vec.Flatten(args.size());
    name_vec.Flatten(args.size());
    auto type_data = FlatVector::GetData<string_t>(type_vec);
    auto name_data = FlatVector::GetData<string_t>(name_vec);
    auto &type_validity = FlatVector::Validity(type_vec);
    auto &name_validity = FlatVector::Validity(name_vec);

    // Only touched when the third arity was bound; `data[2]` does not exist on
    // the two-argument call.
    const bool *qualify_data = nullptr;
    if (has_qualify_arg) {
        auto &qualify_vec = args.data[2];
        qualify_vec.Flatten(args.size());
        qualify_data = FlatVector::GetData<bool>(qualify_vec);
    }

    auto &result_validity = sv_compat::validity_mut(result);

    Connection probe(*state.GetContext().db);
    duckdb_connection borrowed = reinterpret_cast<duckdb_connection>(&probe);
    // Read once per chunk: the path cannot change while a statement runs.
    const std::vector<uint8_t> sp = sv_caller_search_path(state.GetContext());

    for (idx_t i = 0; i < args.size(); ++i) {
        // A NULL in any argument yields a NULL row, the third included: asked
        // "qualified or not?" with NULL there is no answer, and defaulting to
        // either spelling would be a guess.
        if (!type_validity.RowIsValid(i) || !name_validity.RowIsValid(i) ||
            (has_qualify_arg && !FlatVector::Validity(args.data[2]).RowIsValid(i))) {
            result_validity.SetInvalid(i);
            continue;
        }
        const string_t &t = type_data[i];
        const string_t &n = name_data[i];
        const uint8_t qualify =
            (has_qualify_arg && qualify_data[i]) ? uint8_t(1) : uint8_t(0);
        sv_emit_scalar_row(
            result, i, "get_ddl",
            [&](char **op, size_t *ol, char *eb, size_t ebl) {
                return sv_get_ddl_exec_rust(
                    borrowed,
                    reinterpret_cast<const uint8_t *>(t.GetData()), t.GetSize(),
                    reinterpret_cast<const uint8_t *>(n.GetData()), n.GetSize(),
                    sp.empty() ? nullptr : sp.data(), sp.size(),
                    qualify,
                    op, ol, eb, ebl);
            });
    }
    if (args.AllConstant()) {
        result.SetVectorType(VectorType::CONSTANT_VECTOR);
    }
}

// read_yaml_from_semantic_view(name VARCHAR) -> VARCHAR
static void sv_read_yaml_from_semantic_view_exec(DataChunk &args,
                                                 ExpressionState &state,
                                                 Vector &result) {
    auto &name_vec = args.data[0];
    name_vec.Flatten(args.size());
    auto name_data = FlatVector::GetData<string_t>(name_vec);
    auto &name_validity = FlatVector::Validity(name_vec);
    auto &result_validity = sv_compat::validity_mut(result);

    Connection probe(*state.GetContext().db);
    duckdb_connection borrowed = reinterpret_cast<duckdb_connection>(&probe);
    const std::vector<uint8_t> sp = sv_caller_search_path(state.GetContext());

    for (idx_t i = 0; i < args.size(); ++i) {
        if (!name_validity.RowIsValid(i)) {
            result_validity.SetInvalid(i);
            continue;
        }
        const string_t &n = name_data[i];
        sv_emit_scalar_row(
            result, i, "read_yaml_from_semantic_view",
            [&](char **op, size_t *ol, char *eb, size_t ebl) {
                return sv_read_yaml_from_semantic_view_exec_rust(
                    borrowed,
                    reinterpret_cast<const uint8_t *>(n.GetData()), n.GetSize(),
                    sp.empty() ? nullptr : sp.data(), sp.size(),
                    op, ol, eb, ebl);
            });
    }
    if (args.AllConstant()) {
        result.SetVectorType(VectorType::CONSTANT_VECTOR);
    }
}

extern "C" {
    // Two arities on one name (TECH-DEBT #25): the third `BOOLEAN` argument is
    // Snowflake's `use_fully_qualified_names`. Registering a set rather than
    // two separate functions is what lets DuckDB's binder pick by argument
    // count; both overloads share `sv_get_ddl_exec`, which reads the third
    // column only when it was bound.
    bool sv_register_get_ddl(duckdb_database db_handle,
                             char *error_buf, size_t error_buf_len) {
        try {
            ScalarFunctionSet set("get_ddl");
            set.AddFunction(ScalarFunction(
                "get_ddl", {LogicalType::VARCHAR, LogicalType::VARCHAR},
                LogicalType::VARCHAR, sv_get_ddl_exec));
            set.AddFunction(ScalarFunction(
                "get_ddl",
                {LogicalType::VARCHAR, LogicalType::VARCHAR,
                 LogicalType::BOOLEAN},
                LogicalType::VARCHAR, sv_get_ddl_exec));
            return sv_register_scalar_function_set(
                db_handle, "get_ddl", std::move(set), error_buf, error_buf_len);
        } catch (const std::exception &e) {
            if (error_buf != nullptr && error_buf_len > 0) {
                snprintf(error_buf, error_buf_len,
                    "sv_register_get_ddl failed: %s", e.what());
            }
            return false;
        } catch (...) {
            if (error_buf != nullptr && error_buf_len > 0) {
                snprintf(error_buf, error_buf_len,
                    "sv_register_get_ddl failed: unknown C++ exception");
            }
            return false;
        }
    }
    bool sv_register_read_yaml_from_semantic_view(duckdb_database db_handle,
                                                  char *error_buf, size_t error_buf_len) {
        LogicalType args[] = {LogicalType::VARCHAR};
        return sv_register_scalar_function(
            db_handle, "read_yaml_from_semantic_view",
            args, 1,
            LogicalType::VARCHAR,
            sv_read_yaml_from_semantic_view_exec,
            error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 Task 5 (Wave 5) — explain_semantic_view migration
// ---------------------------------------------------------------------------
//
// Migrates `explain_semantic_view(view_name, dimensions := [...],
// metrics := [...], facts := [...])` off duckdb-rs's
// `register_table_function_with_extra_info` (which marshals
// `ClientContext &` away) to the C++ Catalog API path. The bind callback
// opens a per-call `Connection probe(*context.db)` and bridges to the
// Rust dispatcher `sv_explain_semantic_view_bind_rust` which runs the
// catalog lookup + expand + EXPLAIN on the per-call connection. Output is
// one VARCHAR row per explain-output line — reuses the Wave 1/2
// `SvVarcharBindData` shape + `sv_emit_varchar_rows` exec.
//
// Named LIST(VARCHAR) parameter handling: the three optional named
// parameters are flattened on the C++ side using
// `sv_serialise_string_list` (length-prefixed wire format) and passed as
// (ptr, len) pairs to the Rust dispatcher. Missing named parameters are
// passed as nullptr+0, which the Rust side treats as an empty list.

// Serialise a LIST(VARCHAR) Value into the standard length-prefixed wire
// format (`u32 count; for each: u32 byte_len + bytes`). The returned bytes
// can be handed directly to the Rust dispatcher as (ptr, len). Throws
// BinderException if the Value is not a LIST or contains non-VARCHAR
// children — defensive: DuckDB's named-parameter type-check already
// enforces the LIST(VARCHAR) declaration at registration time, so a
// mismatch here is a planner bug.
static std::vector<uint8_t> sv_serialise_string_list(
    const Value &list_val, const char *param_name) {
    std::vector<uint8_t> buf;
    const auto &children = ListValue::GetChildren(list_val);
    uint32_t count = static_cast<uint32_t>(children.size());
    buf.reserve(4 + children.size() * 16);
    buf.push_back(static_cast<uint8_t>(count & 0xff));
    buf.push_back(static_cast<uint8_t>((count >> 8) & 0xff));
    buf.push_back(static_cast<uint8_t>((count >> 16) & 0xff));
    buf.push_back(static_cast<uint8_t>((count >> 24) & 0xff));
    for (const auto &c : children) {
        if (c.IsNull()) {
            throw BinderException(
                std::string("explain_semantic_view: `") + param_name +
                "` contains a NULL element (only non-NULL VARCHARs accepted)");
        }
        // c.GetValue<std::string>() applies any necessary cast. The named-
        // parameter declaration is LIST(VARCHAR) so this should be a no-op.
        std::string s = c.GetValue<std::string>();
        uint32_t len = static_cast<uint32_t>(s.size());
        buf.push_back(static_cast<uint8_t>(len & 0xff));
        buf.push_back(static_cast<uint8_t>((len >> 8) & 0xff));
        buf.push_back(static_cast<uint8_t>((len >> 16) & 0xff));
        buf.push_back(static_cast<uint8_t>((len >> 24) & 0xff));
        buf.insert(buf.end(), s.begin(), s.end());
    }
    return buf;
}

static unique_ptr<FunctionData> sv_explain_semantic_view_bind(
    ClientContext &context,
    TableFunctionBindInput &input,
    vector<LogicalType> &return_types,
    sv_compat::bind_names_t &names) {
    auto bd = make_uniq<SvVarcharBindData>();
    bd->expected_cols = 1;
    return_types.push_back(LogicalType::VARCHAR);
    names.emplace_back("explain_output");

    if (input.inputs.empty() || input.inputs[0].IsNull()) {
        throw BinderException(
            "explain_semantic_view: view name is required (positional arg 0)");
    }
    std::string view_name = input.inputs[0].GetValue<std::string>();

    // Pull the three optional named LIST(VARCHAR) parameters. The
    // `input.named_parameters` map is case-insensitive (per
    // case_insensitive_map_t). A missing entry means the user did not
    // supply that named parameter — pass nullptr+0 to the Rust side.
    std::vector<uint8_t> dims_buf, metrics_buf, facts_buf;
    auto it_d = input.named_parameters.find("dimensions");
    if (it_d != input.named_parameters.end() && !it_d->second.IsNull()) {
        dims_buf = sv_serialise_string_list(it_d->second, "dimensions");
    }
    auto it_m = input.named_parameters.find("metrics");
    if (it_m != input.named_parameters.end() && !it_m->second.IsNull()) {
        metrics_buf = sv_serialise_string_list(it_m->second, "metrics");
    }
    auto it_f = input.named_parameters.find("facts");
    if (it_f != input.named_parameters.end() && !it_f->second.IsNull()) {
        facts_buf = sv_serialise_string_list(it_f->second, "facts");
    }
    std::vector<uint8_t> order_buf;
    auto it_o = input.named_parameters.find("column_order");
    if (it_o != input.named_parameters.end() && !it_o->second.IsNull()) {
        order_buf = sv_serialise_string_list(it_o->second, "column_order");
    }

    // Scalar VARCHAR, not a list — passed to Rust as a plain UTF-8 ptr/len.
    // Absent or NULL yields (nullptr, 0), which the Rust side reads as "no
    // predicate".
    std::string where_clause;
    auto it_w = input.named_parameters.find("where_clause");
    if (it_w != input.named_parameters.end() && !it_w->second.IsNull()) {
        where_clause = it_w->second.GetValue<std::string>();
    }
    std::vector<uint8_t> sp_buf = sv_search_path_payload(context, input);

    Connection probe(*context.db);
    // The EXPLAIN of the expanded query binds the view's tables on `probe`, so
    // a cancelled caller is forwarded and raised bare, as in semantic_view().
    SvBindCancellationScope cancellation(context, probe);
    duckdb_connection borrowed = reinterpret_cast<duckdb_connection>(&probe);

    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));

    uint8_t rc = sv_explain_semantic_view_bind_rust(
        borrowed,
        reinterpret_cast<const uint8_t *>(view_name.data()), view_name.size(),
        sp_buf.empty()      ? nullptr : sp_buf.data(),      sp_buf.size(),
        dims_buf.empty()    ? nullptr : dims_buf.data(),    dims_buf.size(),
        metrics_buf.empty() ? nullptr : metrics_buf.data(), metrics_buf.size(),
        facts_buf.empty()   ? nullptr : facts_buf.data(),   facts_buf.size(),
        where_clause.empty() ? nullptr : reinterpret_cast<const uint8_t *>(where_clause.data()),
        where_clause.size(),
        order_buf.empty()   ? nullptr : order_buf.data(),   order_buf.size(),
        &payload.ptr, &payload.len,
        error_buf, sizeof(error_buf));
    cancellation.ThrowIfCancelled();
    if (rc != 0) {
        throw BinderException(std::string("explain_semantic_view: ") + error_buf);
    }
    sv_parse_varchar_payload(payload.ptr, payload.len, *bd,
                             "explain_semantic_view");
    return std::move(bd);
}

// explain_semantic_view takes named LIST(VARCHAR) parameters (`dimensions`,
// `metrics`, `facts`) so DuckDB's binder type-checks `dimensions := [...]`.
// It builds a spec carrying those named params (from the single shared
// `sv_semantic_named_params`) plus an init_local state constructor, and
// registers via `sv_register_table_function_core` (C-7) — the hand-rolled
// TableFunction construction it used to duplicate now lives once, in the core.
// Borrow contract is unchanged: the bind receives `ClientContext &` and opens
// per-call Connections; no long-lived extension-owned connection. Failures
// surface through the forwarded `error_buf` (D-08/D-09).
static bool sv_register_explain_semantic_view_impl(duckdb_database db_handle,
                                                   char *error_buf,
                                                   size_t error_buf_len) {
    const LogicalType arg_types[] = {LogicalType::VARCHAR};
    SvTableFunctionSpec spec;
    spec.name = "explain_semantic_view";
    spec.arg_types = arg_types;
    spec.arg_count = 1;
    spec.named_params = sv_semantic_named_params();
    spec.bind_cb = sv_explain_semantic_view_bind;
    spec.exec_cb = sv_emit_varchar_rows;
    spec.init_local_cb = sv_varchar_init_local;
    spec.init_global_cb = nullptr;
    return sv_register_table_function_core(
        db_handle, spec, "sv_register_explain_semantic_view", error_buf,
        error_buf_len);
}

extern "C" {
    bool sv_register_explain_semantic_view(duckdb_database db_handle,
                                           char *error_buf, size_t error_buf_len) {
        return sv_register_explain_semantic_view_impl(
            db_handle, error_buf, error_buf_len);
    }
}

// ---------------------------------------------------------------------------
// semantic_view(): planned inside the caller's query
// ---------------------------------------------------------------------------
//
// `semantic_view(view_name, dimensions := [...], metrics := [...], ...)` is a
// `bind_replace` table function, as DuckDB's own `query()` is: its bind hands
// back a table reference, a subquery over the expanded SQL, and DuckDB binds
// and plans that subquery in place of the call. So the expansion is part of the
// caller's query:
//
//   - it runs in the caller's transaction and session, so it sees the
//     transaction's own uncommitted rows and the session's temporary tables;
//   - DuckDB optimises it together with the query around it (filter and
//     projection pushdown, parallel scans) and streams it, with no buffering;
//   - a cancelled caller stops it as it stops any query;
//   - its columns have the types DuckDB gives them (`sum()` over an INTEGER is a
//     HUGEINT), and the table alias and column aliases apply as to any table;
//   - it may refer to a column of an enclosing query (`LATERAL`).
//
// The bind still opens a per-call `Connection probe(*context.db)` for one
// thing: `sv_semantic_view_bind_rust` reads the definition from the catalog
// table on it (TECH-DEBT #19). That connection runs no part of the query.
//
// Borrow contract: identical to the other read-side binds. The Rust dispatcher
// MUST NOT call `duckdb_disconnect` on the borrowed handle — teardown is the
// C++ scope's responsibility.
// Point every expression and table reference in `node` at `location`. The
// expansion is parsed from SQL the user never wrote, so the positions the
// parser gave it are offsets into that text; left alone, an error in it (a
// column a metric names that no longer exists, a value that fails a cast)
// would put its caret on whatever the caller's query has at that offset. The
// call's own location is where the error belongs.
static void sv_relocate(QueryNode &node, QueryLocation location) {
    std::function<void(ParsedExpression &)> visit = [&](ParsedExpression &expr) {
        expr.SetQueryLocation(location);
        if (expr.GetExpressionClass() == ExpressionClass::SUBQUERY) {
            auto &subquery = expr.Cast<SubqueryExpression>().SubqueryMutable();
            if (subquery && subquery->node) {
                sv_relocate(*subquery->node, location);
            }
        }
        ParsedExpressionIterator::EnumerateChildren(expr, [&](ParsedExpression &child) { visit(child); });
    };
    ParsedExpressionIterator::EnumerateQueryNodeChildren(
        node,
        [&](unique_ptr<ParsedExpression> &expr) {
            if (expr) {
                visit(*expr);
            }
        },
        [&](TableRef &ref) { ref.query_location = location; });
}

// Pin each table the expansion names by a bare name to the catalog entry it
// resolves to, so DuckDB's bind of the expansion in the caller's query can't
// read a caller's CTE of that name in its place, or a table of that name in
// the caller's current schema. The `TABLES` entries come qualified with the
// database and schema stamped at CREATE (`expand::body_qualifier`); this
// covers the tables a member expression's subquery names, which the Rust side
// sees only as text. A bare name is looked up in that database and schema
// first, as a `TABLES` entry is, then as DuckDB's binder would look it up.
//
// Left as written: a CTE name the expansion has in scope, anything inside a
// hoisted where_clause subquery (`__sv_sqN`, the caller's own text, which
// resolves in the caller's query as written), and a name in no catalog (a
// replacement scan such as a file path). A name in no catalog that the
// caller's query has a CTE for is refused, since the CTE would be read.
struct SvTablePinner {
    ClientContext &context;
    optional_ptr<Binder> caller;
    Identifier database;
    Identifier schema;
    std::vector<Identifier> scope;

    static bool IsHoisted(const Identifier &name) {
        return StringUtil::StartsWith(name.GetIdentifierName(), "__sv_sq");
    }

    optional_ptr<CatalogEntry> Lookup(QualifiedName name) {
        EntryLookupInfo lookup(CatalogType::TABLE_ENTRY, std::move(name));
        return Catalog::GetEntry(context, lookup, OnEntryNotFound::RETURN_NULL);
    }

    void Pin(BaseTableRef &ref) {
        auto &name = ref.GetQualifiedName();
        if (name.Path().size() != 1) {
            return;
        }
        auto table = name.Name();
        for (auto &cte : scope) {
            if (cte == table) {
                return;
            }
        }
        optional_ptr<CatalogEntry> entry;
        if (!database.empty() && Catalog::GetCatalogEntry(context, database)) {
            entry = Lookup(QualifiedName(database, schema.empty() ? Identifier::DefaultSchema() : schema, table));
        }
        if (!entry) {
            entry = Lookup(QualifiedName(table));
        }
        if (entry) {
            ref.SetQualifiedName(entry->ParentCatalog().GetName(), entry->ParentSchema().name, table);
            return;
        }
        if (caller && caller->GetCTEBinding(BindingAlias(table))) {
            throw BinderException(
                "semantic_view: the view's definition names table \"%s\", which does not exist; the "
                "query's CTE of that name is not read in its place",
                table.GetIdentifierName());
        }
    }

    void Expr(ParsedExpression &expr) {
        if (expr.GetExpressionClass() == ExpressionClass::SUBQUERY) {
            auto &subquery = expr.Cast<SubqueryExpression>().SubqueryMutable();
            if (subquery && subquery->node) {
                Node(*subquery->node);
            }
        }
        ParsedExpressionIterator::EnumerateChildren(expr, [&](ParsedExpression &child) { Expr(child); });
    }

    void Expr(unique_ptr<ParsedExpression> &expr) {
        if (expr) {
            Expr(*expr);
        }
    }

    void Ref(TableRef &ref) {
        switch (ref.type) {
        case TableReferenceType::BASE_TABLE:
            Pin(ref.Cast<BaseTableRef>());
            break;
        case TableReferenceType::JOIN: {
            auto &join = ref.Cast<JoinRef>();
            Ref(*join.left);
            Ref(*join.right);
            Expr(join.condition);
            break;
        }
        case TableReferenceType::SUBQUERY: {
            auto &subquery = ref.Cast<SubqueryRef>();
            if (subquery.subquery && subquery.subquery->node) {
                Node(*subquery.subquery->node);
            }
            break;
        }
        case TableReferenceType::TABLE_FUNCTION:
            Expr(ref.Cast<TableFunctionRef>().function);
            break;
        case TableReferenceType::EXPRESSION_LIST:
            for (auto &row : ref.Cast<ExpressionListRef>().values) {
                for (auto &value : row) {
                    Expr(value);
                }
            }
            break;
        case TableReferenceType::PIVOT: {
            auto &pivot = ref.Cast<PivotRef>();
            Ref(*pivot.source);
            for (auto &aggregate : pivot.aggregates) {
                Expr(aggregate);
            }
            for (auto &column : pivot.pivots) {
                for (auto &pivot_expr : column.pivot_expressions) {
                    Expr(pivot_expr);
                }
                if (column.subquery) {
                    Node(*column.subquery);
                }
            }
            break;
        }
        default:
            break;
        }
    }

    void Node(QueryNode &node) {
        auto outer = scope.size();
        for (auto &cte : node.cte_map.map) {
            scope.push_back(cte.first);
        }
        for (auto &cte : node.cte_map.map) {
            if (!IsHoisted(cte.first) && cte.second->query_node) {
                Node(*cte.second->query_node);
            }
        }
        switch (node.type) {
        case QueryNodeType::SELECT_NODE: {
            auto &select = node.Cast<SelectNode>();
            for (auto &expr : select.select_list) {
                Expr(expr);
            }
            for (auto &expr : select.groups.group_expressions) {
                Expr(expr);
            }
            Expr(select.where_clause);
            Expr(select.having);
            Expr(select.qualify);
            if (select.from_table) {
                Ref(*select.from_table);
            }
            break;
        }
        case QueryNodeType::SET_OPERATION_NODE:
            for (auto &child : node.Cast<SetOperationNode>().children) {
                Node(*child);
            }
            break;
        case QueryNodeType::RECURSIVE_CTE_NODE: {
            auto &recursive = node.Cast<RecursiveCTENode>();
            scope.push_back(recursive.ctename);
            Node(*recursive.left);
            Node(*recursive.right);
            break;
        }
        default:
            break;
        }
        ParsedExpressionIterator::EnumerateQueryNodeModifiers(
            node, [&](unique_ptr<ParsedExpression> &expr) { Expr(expr); });
        scope.resize(outer);
    }
};

static unique_ptr<TableRef> sv_semantic_view_bind_replace(
    ClientContext &context,
    TableFunctionBindInput &input) {
    if (input.inputs.empty() || input.inputs[0].IsNull()) {
        throw BinderException(
            "semantic_view: view name is required (positional arg 0)");
    }
    std::string view_name = input.inputs[0].GetValue<std::string>();

    std::vector<uint8_t> dims_buf, metrics_buf, facts_buf;
    auto it_d = input.named_parameters.find("dimensions");
    if (it_d != input.named_parameters.end() && !it_d->second.IsNull()) {
        dims_buf = sv_serialise_string_list(it_d->second, "dimensions");
    }
    auto it_m = input.named_parameters.find("metrics");
    if (it_m != input.named_parameters.end() && !it_m->second.IsNull()) {
        metrics_buf = sv_serialise_string_list(it_m->second, "metrics");
    }
    auto it_f = input.named_parameters.find("facts");
    if (it_f != input.named_parameters.end() && !it_f->second.IsNull()) {
        facts_buf = sv_serialise_string_list(it_f->second, "facts");
    }
    std::vector<uint8_t> order_buf;
    auto it_o = input.named_parameters.find("column_order");
    if (it_o != input.named_parameters.end() && !it_o->second.IsNull()) {
        order_buf = sv_serialise_string_list(it_o->second, "column_order");
    }

    // Scalar VARCHAR, not a list — passed to Rust as a plain UTF-8 ptr/len.
    // Absent or NULL yields (nullptr, 0), which the Rust side reads as "no
    // predicate".
    std::string where_clause;
    auto it_w = input.named_parameters.find("where_clause");
    if (it_w != input.named_parameters.end() && !it_w->second.IsNull()) {
        where_clause = it_w->second.GetValue<std::string>();
    }
    std::vector<uint8_t> sp_buf = sv_search_path_payload(context, input);

    Connection probe(*context.db);
    // The catalog read runs on `probe`; a cancelled caller (interrupt or
    // deadline) is forwarded and raised bare, as in the metadata binds.
    SvBindCancellationScope cancellation(context, probe);
    duckdb_connection borrowed = reinterpret_cast<duckdb_connection>(&probe);

    // Expand the request into one SELECT whose parse positions are the call's.
    // With `fence`, the predicate is one that names only where_clause's
    // members (`expand::where_fence`), for the isolated bind below.
    auto expand = [&](bool fence) {
        SvOwnedBuffer payload;
        char error_buf[1024];
        std::memset(error_buf, 0, sizeof(error_buf));
        uint8_t rc = sv_semantic_view_bind_rust(
            borrowed,
            reinterpret_cast<const uint8_t *>(view_name.data()), view_name.size(),
            sp_buf.empty()      ? nullptr : sp_buf.data(),      sp_buf.size(),
            dims_buf.empty()    ? nullptr : dims_buf.data(),    dims_buf.size(),
            metrics_buf.empty() ? nullptr : metrics_buf.data(), metrics_buf.size(),
            facts_buf.empty()   ? nullptr : facts_buf.data(),   facts_buf.size(),
            where_clause.empty() ? nullptr
                                 : reinterpret_cast<const uint8_t *>(where_clause.data()),
            where_clause.size(),
            order_buf.empty()   ? nullptr : order_buf.data(),   order_buf.size(),
            fence ? 1 : 0,
            &payload.ptr, &payload.len,
            error_buf, sizeof(error_buf));
        cancellation.ThrowIfCancelled();
        if (rc != 0) {
            throw BinderException(std::string("semantic_view: ") + error_buf);
        }

        // The payload is the SQL to plan: one SELECT. It is parsed with the
        // built-in grammar, as DuckDB's `query()` parses its argument; it is
        // generated SQL and uses none of this extension's syntax.
        //
        // The expansion carries `where_clause` as written, so a malformed
        // predicate fails here. Its parse error is reported at the call, like
        // the binder's (see `sv_relocate`): the position the parser gives is an
        // offset into SQL the caller never wrote.
        // The payload frames the database and schema stamped at CREATE ahead of
        // the SQL (`query::wire::frame_bind_payload`).
        const char *framed = reinterpret_cast<const char *>(payload.ptr);
        size_t offset = 0;
        std::string database = sv_read_string(framed, payload.len, offset, "semantic_view");
        std::string schema = sv_read_string(framed, payload.len, offset, "semantic_view");
        std::string sql(framed + offset, payload.len - offset);
        Parser parser = sv_compat::builtin_parser();
        try {
            parser.ParseQuery(sql);
        } catch (ParserException &e) {
            ErrorData error(e);
            throw ParserException(Exception::InitializeExtraInfo(input.ref.query_location),
                                  "semantic_view: the query does not parse, so check "
                                  "where_clause: " + error.RawMessage());
        }
        // The Rust side refuses a `;` in where_clause, so the expansion is one
        // statement; this guards the shape all the same, as an input error,
        // since the expansion's text comes from the caller's arguments.
        if (parser.statements.size() != 1 ||
            parser.statements[0]->type != StatementType::SELECT_STATEMENT) {
            throw InvalidInputException(
                "semantic_view: the expansion is not one SELECT statement: %s", sql);
        }
        auto select = unique_ptr_cast<SQLStatement, SelectStatement>(
            std::move(parser.statements[0]));
        sv_relocate(*select->node, input.ref.query_location);
        SvTablePinner pinner {context, input.binder, Identifier(database), Identifier(schema), {}};
        pinner.Node(*select->node);
        return select;
    };
    auto select = expand(false);

    // DuckDB binds the returned subquery in the caller's scope, so a name the
    // expansion's own tables lack would bind to a column of an enclosing query
    // (a comma join's right side is bound as LATERAL) instead of failing: a
    // member expression's bare `amount` would read the outer row's `amount`.
    // Only `where_clause` may refer outward (TECH-DEBT #107). So the expansion
    // is bound first on a binder of its own, with no enclosing scope, where
    // such a name fails as it would in a query of its own; its error carries
    // the call's position, like the real bind's. Its predicate names only
    // where_clause's members, so the member expressions where_clause brings in
    // are checked too, and the expansion joins the same tables and takes the
    // same route (a filtered query is never answered from a materialization).
    //
    // DuckDB resolves a name outward only through the binder's enclosing
    // scopes, so with none (a top-level FROM, a join's left side, a CTE or a
    // subquery in FROM) the real bind is already isolated, and the second bind,
    // which costs as much as the first, is skipped.
    if (input.binder && input.binder->HasEnclosingScope()) {
        auto isolated = where_clause.empty()
                            ? unique_ptr_cast<SQLStatement, SelectStatement>(select->Copy())
                            : expand(true);
        auto isolated_binder = Binder::CreateBinder(context);
        isolated_binder->Bind(static_cast<SQLStatement &>(*isolated));
    }

    unique_ptr<TableRef> ref = make_uniq<SubqueryRef>(std::move(select));
    return ref;
}

// semantic_view takes the same named parameters as explain_semantic_view, from
// the single shared `sv_semantic_named_params` (C-7), so the two cannot drift.
// It has only a `bind_replace`: DuckDB binds the subquery it returns, so there
// is no bind data, state or scan of its own. Same error_buf convention
// (D-08/D-09); no stderr.
static bool sv_register_semantic_view_impl(duckdb_database db_handle,
                                           char *error_buf,
                                           size_t error_buf_len) {
    const LogicalType arg_types[] = {LogicalType::VARCHAR};
    SvTableFunctionSpec spec;
    spec.name = "semantic_view";
    spec.arg_types = arg_types;
    spec.arg_count = 1;
    spec.named_params = sv_semantic_named_params();
    spec.bind_replace_cb = sv_semantic_view_bind_replace;
    return sv_register_table_function_core(
        db_handle, spec, "sv_register_semantic_view", error_buf,
        error_buf_len);
}

extern "C" {
    bool sv_register_semantic_view(duckdb_database db_handle,
                                   char *error_buf, size_t error_buf_len) {
        return sv_register_semantic_view_impl(
            db_handle, error_buf, error_buf_len);
    }
}

#ifdef SV_DUCKDB_2
// ---------------------------------------------------------------------------
// Grammar productions (DuckDB 2.0 PEG parser)
// ---------------------------------------------------------------------------
// The parser override above sees a whole statement's text, so it can only
// rewrite statements that *start* with our syntax. A grammar extension adds
// rules to DuckDB's own PEG grammar instead, so our syntax can sit anywhere
// the grammar allows: `SELECT ... FROM (SHOW SEMANTIC VIEWS)`, a CTE, a view
// body (TECH-DEBT #77).
//
// The rules and their transforms live in Rust (src/grammar/). This side only
// bridges: it registers the rules Rust lists, and when a statement rule
// matches it serialises the matched subtree with the statement's tokens
// (wire format in src/grammar/tree.rs), has Rust lower it to SQL, and parses
// that SQL with the builtin parser. The override still runs first and owns
// every statement that starts with our syntax; with overrides off
// (allow_parser_override_extension = 'DEFAULT') every statement comes here.

static const char *SV_GRAMMAR_NAME = "semantic_views";

static void sv_wire_u32(string &out, uint32_t v) {
    char b[4] = {char(v & 0xff), char((v >> 8) & 0xff), char((v >> 16) & 0xff), char((v >> 24) & 0xff)};
    out.append(b, 4);
}

static void sv_wire_str(string &out, const string &s) {
    sv_wire_u32(out, static_cast<uint32_t>(s.size()));
    out += s;
}

static void sv_wire_node(string &out, ParseResult &pr) {
    out.push_back(static_cast<char>(pr.type));
    sv_wire_str(out, pr.Name());
    sv_wire_u32(out, pr.offset.IsValid() ? static_cast<uint32_t>(pr.offset.GetIndex()) : UINT32_MAX);
    sv_wire_u32(out, pr.length.IsValid() ? static_cast<uint32_t>(pr.length.GetIndex()) : UINT32_MAX);
    string value;
    vector<reference<ParseResult>> children;
    switch (pr.type) {
    case ParseResultType::LIST:
        for (auto &c : pr.Cast<ListParseResult>().GetChildren()) {
            children.push_back(c);
        }
        break;
    case ParseResultType::REPEAT:
        for (auto &c : pr.Cast<RepeatParseResult>().GetChildren()) {
            children.push_back(c);
        }
        break;
    case ParseResultType::OPTIONAL: {
        auto &opt = pr.Cast<OptionalParseResult>();
        if (opt.HasResult()) {
            children.push_back(opt.GetResult());
        }
        break;
    }
    case ParseResultType::CHOICE:
        children.push_back(pr.Cast<ChoiceParseResult>().GetResult());
        break;
    case ParseResultType::IDENTIFIER:
        value = pr.Cast<IdentifierParseResult>().identifier.GetIdentifierName();
        break;
    case ParseResultType::KEYWORD:
        value = pr.Cast<KeywordParseResult>().keyword;
        break;
    case ParseResultType::OPERATOR:
        value = pr.Cast<OperatorParseResult>().operator_token;
        break;
    case ParseResultType::NUMBER:
        value = pr.Cast<NumberParseResult>().number;
        break;
    case ParseResultType::STRING:
        value = pr.Cast<StringLiteralParseResult>().result;
        break;
    case ParseResultType::TOKEN:
        value = pr.Cast<TokenParseResult>().text;
        break;
    default:
        break;
    }
    sv_wire_str(out, value);
    sv_wire_u32(out, static_cast<uint32_t>(children.size()));
    for (auto &c : children) {
        sv_wire_node(out, c.get());
    }
}

// Whether the query the parser override recorded on this thread
// (sv_override_query) is the one being parsed: every token is that query's
// text at the token's offset, and everything else in it (before the first
// token, between tokens, after the last) is whitespace or comments. The
// iterator holds the whole query's tokens, so together that is the query, up
// to the trivia the tokenizer drops. Each gap must also hold a line break
// exactly when the tokenizer says this query's gap did (preceded_by_newline),
// so a record never lends its line structure to an error's caret or to the
// stored body. A transform gets only the tokens, so the record is how it
// learns the source text: sv_grammar_span_has_source below checks the
// statement's own span the same way, and this whole-query check guards the
// caret, which quotes the query around the statement. With the override on,
// the override saw this query and declined it just before the grammar ran.
// With it off nothing was recorded, and an earlier query on this thread is
// left over: matching tokens alone would take its text where the offsets line
// up, so a body with spaces where the earlier one had `+o.b` would store the
// earlier body. What still passes is a gap of the same length with the same
// line breaks, such as a tab for a space (TECH-DEBT #100).
static bool sv_grammar_has_source(TokenIterator &tokens) {
    const std::string &query = sv_override_query;
    if (query.empty()) {
        return false;
    }
    idx_t pos = 0;
    bool first = true;
    for (idx_t i = 0; i < tokens.Size(); i++) {
        auto &tok = tokens.GetToken(i);
        if (tok.text.empty()) {
            continue;
        }
        bool newline = false;
        if (!sv_skip_trivia(query, pos, &newline) || pos != tok.offset ||
            query.compare(tok.offset, tok.text.size(), tok.text) != 0) {
            return false;
        }
        // The tokenizer leaves the flag unset on the first token.
        if (!first && newline != tok.preceded_by_newline) {
            return false;
        }
        first = false;
        pos = tok.offset + tok.text.size();
    }
    return sv_skip_trivia(query, pos) && pos == query.size();
}

// The matched statement's own part of the query: its byte span [start, end)
// and the tokens wholly inside it, [first, last). The iterator holds the whole
// query's tokens, so a transform that got all of them, and the record with
// them, would cost every statement of a K-statement string the length of the
// whole string (TECH-DEBT #85). Without a span the statement is the query.
struct SvGrammarSpan {
    idx_t start = 0;
    idx_t end = 0;
    idx_t first = 0;
    idx_t last = 0;
};

static SvGrammarSpan sv_grammar_span(TokenIterator &tokens, ParseResult &pr) {
    SvGrammarSpan span;
    span.last = tokens.Size();
    if (!pr.offset.IsValid() || !pr.length.IsValid()) {
        span.end = tokens.Size() == 0 ? 0 : tokens.GetToken(tokens.Size() - 1).offset +
                                                tokens.GetToken(tokens.Size() - 1).text.size();
        return span;
    }
    span.start = pr.offset.GetIndex();
    span.end = span.start + pr.length.GetIndex();
    // The tokens are in query order: binary-search the first one at or after
    // `start`, then take them while they end inside the span.
    idx_t lo = 0, hi = tokens.Size();
    while (lo < hi) {
        idx_t mid = lo + (hi - lo) / 2;
        if (tokens.GetToken(mid).offset < span.start) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    span.first = lo;
    span.last = lo;
    while (span.last < tokens.Size()) {
        auto &tok = tokens.GetToken(span.last);
        if (tok.offset + tok.text.size() > span.end) {
            break;
        }
        span.last++;
    }
    return span;
}

// Whether the record holds this statement: what sv_grammar_has_source checks
// for the whole query, for the statement's span alone. Each token is the
// record's text at its offset, the gaps between them are whitespace or
// comments holding a line break exactly where this query's do, and so is
// whatever lies between the last token and the end of the span. The gap
// before the first token is outside the span and is not checked.
static bool sv_grammar_span_has_source(TokenIterator &tokens, const SvGrammarSpan &span) {
    const std::string &query = sv_override_query;
    if (query.empty() || span.end > query.size() || span.start > span.end) {
        return false;
    }
    idx_t pos = span.start;
    bool first = true;
    for (idx_t i = span.first; i < span.last; i++) {
        auto &tok = tokens.GetToken(i);
        if (tok.text.empty()) {
            continue;
        }
        bool newline = false;
        if (!sv_skip_trivia(query, pos, &newline) || pos != tok.offset ||
            query.compare(tok.offset, tok.text.size(), tok.text) != 0) {
            return false;
        }
        if (!first && newline != tok.preceded_by_newline) {
            return false;
        }
        first = false;
        pos = tok.offset + tok.text.size();
    }
    return pos >= span.end || (sv_skip_trivia(query, pos) && pos >= span.end);
}

// The statement's tree, its own tokens, and its part of the record when the
// record holds it.
static string sv_grammar_serialise(PEGTransformer &transformer, ParseResult &pr, const SvGrammarSpan &span,
                                   bool has_source) {
    string out;
    sv_wire_node(out, pr);
    auto &tokens = transformer.token_iterator;
    sv_wire_u32(out, static_cast<uint32_t>(span.last - span.first));
    for (idx_t i = span.first; i < span.last; i++) {
        auto &tok = tokens.GetToken(i);
        sv_wire_u32(out, static_cast<uint32_t>(tok.offset));
        sv_wire_u32(out, static_cast<uint32_t>(tok.length));
        out.push_back(tok.preceded_by_newline ? 1 : 0);
        sv_wire_str(out, tok.text);
    }
    sv_wire_u32(out, static_cast<uint32_t>(span.start));
    sv_wire_str(out, has_source ? sv_override_query.substr(span.start, span.end - span.start) : string());
    return out;
}

// Have Rust lower a matched statement rule to SQL, and parse the SQL. With the
// source text, an error's caret is rendered into its message, as the parse stub
// renders it, since 2.0 shows a parser error's location only on the
// ClientContext path (TECH-DEBT #99). The caret quotes the whole query, so it
// needs the record to be the whole query, which is checked only then: an error
// ends the parse, so the check runs at most once per query.
static vector<unique_ptr<SQLStatement>> sv_grammar_lower(PEGTransformer &transformer, ParseResult &pr) {
    auto span = sv_grammar_span(transformer.token_iterator, pr);
    bool has_source = sv_grammar_span_has_source(transformer.token_iterator, span);
    auto wire = sv_grammar_serialise(transformer, pr, span, has_source);
    SvOwnedBuffer sql_buf;
    char error_buf[1024];
    memset(error_buf, 0, sizeof(error_buf));
    uint32_t position = UINT32_MAX;
    uint8_t rc = sv_grammar_transform_rust(reinterpret_cast<const uint8_t *>(wire.data()), wire.size(),
                                           &sql_buf.ptr, &sql_buf.len, error_buf, sizeof(error_buf), &position);
    if (rc == 1) {
        if (position != UINT32_MAX && has_source && sv_grammar_has_source(transformer.token_iterator)) {
            throw ParserException("%s", QueryErrorContext::Format(sv_override_query, string(error_buf), position));
        }
        if (position != UINT32_MAX) {
            throw ParserException(QueryLocation(position, 0), "%s", string(error_buf));
        }
        throw ParserException("%s", string(error_buf));
    }
    if (rc != 0) {
        throw InternalException("semantic_views grammar bridge: %s", string(error_buf));
    }
    Parser parser = sv_compat::builtin_parser();
    parser.ParseQuery(sql_buf.to_string());
    return std::move(parser.statements);
}

static arena_ptr<TransformResultValue> sv_grammar_finalize_query(PEGTransformer &transformer, ParseResult &pr) {
    auto statements = sv_grammar_lower(transformer, pr);
    if (statements.size() != 1 || statements[0]->type != StatementType::SELECT_STATEMENT) {
        throw InternalException("semantic_views grammar: rule %s did not lower to one query", pr.Name());
    }
    auto node = std::move(statements[0]->Cast<SelectStatement>().node);
    return transformer.MakeResult<unique_ptr<QueryNode>>(std::move(node));
}

static arena_ptr<TransformProcess> sv_grammar_start_query(PEGTransformer &transformer, ParseResult &pr) {
    return transformer.MakeProcess<FinalizeTransformProcess>(transformer, pr, sv_grammar_finalize_query);
}

// A table reference (`SEMANTIC_VIEW(...)`) lowers to `SELECT * FROM <ref>`,
// whose FROM is the table reference, alias included.
static arena_ptr<TransformResultValue> sv_grammar_finalize_tableref(PEGTransformer &transformer,
                                                                    ParseResult &pr) {
    auto statements = sv_grammar_lower(transformer, pr);
    if (statements.size() != 1 || statements[0]->type != StatementType::SELECT_STATEMENT) {
        throw InternalException("semantic_views grammar: rule %s did not lower to one query", pr.Name());
    }
    auto &node = statements[0]->Cast<SelectStatement>().node;
    if (!node || node->type != QueryNodeType::SELECT_NODE || !node->Cast<SelectNode>().from_table) {
        throw InternalException("semantic_views grammar: rule %s did not lower to a table reference", pr.Name());
    }
    auto table = std::move(node->Cast<SelectNode>().from_table);
    return transformer.MakeResult<unique_ptr<TableRef>>(std::move(table));
}

static arena_ptr<TransformProcess> sv_grammar_start_tableref(PEGTransformer &transformer, ParseResult &pr) {
    return transformer.MakeProcess<FinalizeTransformProcess>(transformer, pr, sv_grammar_finalize_tableref);
}

// A write statement lowers to several statements (the guards, then the DML).
// They go back as one MultiStatement, which the client unpacks and runs in
// order, as it does for a PIVOT's generated statements.
static arena_ptr<TransformResultValue> sv_grammar_finalize_statement(PEGTransformer &transformer,
                                                                     ParseResult &pr) {
    auto statements = sv_grammar_lower(transformer, pr);
    if (statements.empty()) {
        throw InternalException("semantic_views grammar: rule %s lowered to no statement", pr.Name());
    }
    if (statements.size() == 1) {
        return transformer.MakeResult<unique_ptr<SQLStatement>>(std::move(statements[0]));
    }
    auto multi = make_uniq<MultiStatement>();
    for (auto &statement : statements) {
        multi->statements.push_back(std::move(statement));
    }
    return transformer.MakeResult<unique_ptr<SQLStatement>>(std::move(multi));
}

static arena_ptr<TransformProcess> sv_grammar_start_statement(PEGTransformer &transformer, ParseResult &pr) {
    return transformer.MakeProcess<FinalizeTransformProcess>(transformer, pr, sv_grammar_finalize_statement);
}

class SemanticViewsGrammar final : public GrammarExtension {
public:
    explicit SemanticViewsGrammar(vector<GrammarChange> changes_p)
        : GrammarExtension(SV_GRAMMAR_NAME,
                           "CREATE / ALTER / DROP / SHOW / DESCRIBE SEMANTIC VIEW and SEMANTIC_VIEW(...) syntax"),
          changes(std::move(changes_p)) {
    }
    vector<GrammarChange> GetChanges() const override {
        return changes;
    }

private:
    vector<GrammarChange> changes;
};

// The grammar changes Rust lists (src/grammar/mod.rs, `changes`), one per line.
static vector<GrammarChange> sv_grammar_changes() {
    SvOwnedBuffer buf;
    if (sv_grammar_changes_rust(&buf.ptr, &buf.len) != 0) {
        throw InternalException("semantic_views grammar: listing the grammar rules failed");
    }
    vector<GrammarChange> changes;
    for (auto &line : StringUtil::Split(buf.to_string(), '\n')) {
        if (line.empty()) {
            continue;
        }
        auto fields = StringUtil::Split(line, '\t');
        if (fields.size() >= 2 && fields[0] == "RULE") {
            auto shape = line.substr(line.rfind('\t') + 1);
            if (shape == "query") {
                changes.push_back(GrammarChange::AddRule(fields[1], sv_grammar_start_query));
            } else if (shape == "statement") {
                changes.push_back(GrammarChange::AddRule(fields[1], sv_grammar_start_statement));
            } else if (shape == "tableref") {
                changes.push_back(GrammarChange::AddRule(fields[1], sv_grammar_start_tableref));
            } else if (shape.empty()) {
                changes.push_back(GrammarChange::AddRule(fields[1]));
            } else {
                throw InternalException("semantic_views grammar: unknown rule shape '%s'", shape);
            }
        } else if (fields.size() == 3 && fields[0] == "PREPEND") {
            changes.push_back(GrammarChange::PrependChoice(fields[1], fields[2]));
        } else if (fields.size() == 2 && fields[0] == "ANY_TOKEN") {
            // DuckDB's own matcher for a token it does not interpret (the
            // passthrough dialect's): any one token short of the statement's end.
            changes.push_back(GrammarChange::AddTerminalRuleOverride(
                fields[1], [](const PEGKeywordHelper &) -> unique_ptr<Matcher> {
                    return make_uniq<PassthroughTokenMatcher>();
                }));
        } else {
            throw InternalException("semantic_views grammar: malformed change '%s'", line);
        }
    }
    return changes;
}

// Turn the grammar on for one connection. `active_grammar_extensions` is a
// per-connection setting, so a LOAD reaches the connections that exist now
// (sv_grammar_activate_all) and an ExtensionCallback reaches the ones opened
// later.
static void sv_grammar_activate(ClientContext &context) {
    auto &config = ClientConfig::GetConfig(context);
    vector<Value> names;
    for (auto &existing : config.active_grammar_extensions) {
        if (StringUtil::CIEquals(existing, SV_GRAMMAR_NAME)) {
            return;
        }
        names.emplace_back(existing);
    }
    names.emplace_back(SV_GRAMMAR_NAME);
    ActiveGrammarExtensionsSetting::SetLocal(context, Value::LIST(LogicalType::VARCHAR, std::move(names)));
}

class SemanticViewsGrammarActivation final : public ExtensionCallback {
public:
    void OnConnectionOpened(ClientContext &context) override {
        sv_grammar_activate(context);
    }
};

// Register the grammar once per database and activate it on every connection.
static void sv_register_grammar(DatabaseInstance &db) {
    auto &config = DBConfig::GetConfig(db);
    if (!config.GetCallbackManager().FindGrammarExtension(SV_GRAMMAR_NAME)) {
        GrammarExtension::Register(db, make_shared_ptr<SemanticViewsGrammar>(sv_grammar_changes()));
        ExtensionCallback::Register(config, make_shared_ptr<SemanticViewsGrammarActivation>());
    }
    for (auto &context : db.GetConnectionManager().GetConnectionList()) {
        sv_grammar_activate(*context);
    }
}
#endif

// ---------------------------------------------------------------------------
// sv_register_parser_hooks -- called from Rust after C API init
// ---------------------------------------------------------------------------
// Extracts DatabaseInstance& from the C API handle and registers the
// parser_override hook on DBConfig. Phase 65 Plan 06: signature slimmed
// to `(db_handle)` after H1 catalog_conn retirement. AR-7: the
// SemanticViewsParserInfo carries no Rust state anymore — it is just the
// dynamic_cast marker used by sv_parser_override / sv_parse_stub to confirm
// the parser_info is ours.
//
// Phase 65.1 Plan 10 (WR-06 D-12/D-13): the signature now carries the
// trailing `(char *error_buf, size_t error_buf_len)` pair — matching the
// 17 read-side dispatchers — so registration failures and BORROW-contract
// probe failures surface through the ABI-stable channel into the Rust
// caller's diagnostic. A runtime probe right after the wrapper unwrap
// catches representation drift the file-scope `static_assert` cannot.
extern "C" {
    bool sv_register_parser_hooks(
        duckdb_database db_handle,
        char *error_buf, size_t error_buf_len) {
        try {
            // duckdb_database -> internal_ptr -> DatabaseWrapper ->
            //   shared_ptr<DuckDB> -> shared_ptr<DatabaseInstance>
            auto *wrapper = reinterpret_cast<duckdb::DatabaseWrapper *>(
                db_handle->internal_ptr);
            auto &db = *wrapper->database->instance;

            // Phase 65.1 Plan 10 (WR-06 D-12 runtime probe). The file-scope
            // `static_assert(sizeof(duckdb_connection) == sizeof(void*))`
            // catches size drift; this probe catches representation drift
            // that an equal-size change would miss (e.g. wrapping the
            // pointer in a struct with the same layout but different
            // semantics). Failure refuses the LOAD entirely so no
            // semantic_views state survives a broken bridge — better than
            // discovering the breakage on the first user query.
            //
            // The `Connection probe` is stack-allocated; its dtor runs at
            // scope exit. The bridged `handle` becomes invalid but is not
            // touched after the SELECT 1 round-trip.
            {
                Connection probe(db);
                auto handle = reinterpret_cast<duckdb_connection>(&probe);
                duckdb_result r;
                auto rc = duckdb_query(handle, "SELECT 1", &r);
                if (rc != DuckDBSuccess) {
                    // Phase 65.1 IN-01: surface the underlying DuckDB error
                    // text and clarify that the probe failure can stem from
                    // either ABI drift (the original BORROW-contract concern)
                    // OR a transient DB error at LOAD time (shutdown in
                    // progress, OOM, etc.). The diagnostic must list both
                    // possible causes so an operator hitting a non-bridge
                    // failure isn't misled into chasing an ABI regression.
                    const char *err_text = duckdb_result_error(&r);
                    if (error_buf != nullptr && error_buf_len > 0) {
                        snprintf(error_buf, error_buf_len,
                            "bridge contract probe failed (duckdb_query "
                            "SELECT 1 returned %d): %s. Possible causes: "
                            "duckdb_connection ABI drift OR transient DB "
                            "error at LOAD time; refusing to load "
                            "semantic_views extension.",
                            (int)rc, err_text ? err_text : "(no detail)");
                    }
                    duckdb_destroy_result(&r);
                    return false;
                }
                duckdb_destroy_result(&r);
            }

            // Phase 65 Plan 04 (Task 2 Step B): register the
            // `__sv_compute_create_from_yaml` helper TF via the C++ Catalog
            // API so its bind callback receives a native `ClientContext &`
            // and can open per-call `Connection(*context.db)` to read the
            // YAML file. The outer parser_override INSERT in
            // src/parse.rs::rewrite_yaml_file_create wraps the helper TF's
            // `new_def` row with json_merge_patch + json_object to add
            // now()/current_database()/current_schema() on the caller's
            // connection, preserving D-21 transactional contract.
            //
            // LIFE-2 (code-review 2026-08-08): this block runs BEFORE the
            // parser-extension registration below, for the same reason
            // `REGISTRATIONS` in src/lib.rs puts the whole hook last — the
            // dependency is one-way. The helper TF is a pure Catalog-API
            // registration that never needs the hook, while the hook's own
            // rewrite of `CREATE SEMANTIC VIEW ... FROM YAML FILE` emits a
            // call to this TF. Registering the hook first meant a failure
            // here returned false with `parser_override` already installed
            // and `allow_parser_override_extension` already flipped to
            // FALLBACK, so the YAML-file DDL form parsed and then failed on
            // an unresolved function name.
            {
                // Phase 65.1 Plan 07 (IN-04 D-24): helper TF slimmed to
                // 3 args — (file_path, view_name, comment). The previous
                // `kind` INTEGER parameter was redundant with the outer
                // parser_override INSERT shape (OR IGNORE / OR REPLACE /
                // plain) and the Rust helper ignored it.
                LogicalType arg_types[] = {
                    LogicalType::VARCHAR,  // file_path
                    LogicalType::VARCHAR,  // view_name
                    LogicalType::VARCHAR,  // comment (empty = none)
                };
                // Phase 65.1 Plan 02a: sv_register_table_function requires a
                // `(error_buf, error_buf_len)` trailing pair.
                //
                // Phase 65.1 Plan 10 (WR-06 D-13): sv_register_parser_hooks
                // now carries its own error_buf; forward the helper TF's
                // diagnostic into the parent buffer so the Rust caller
                // surfaces it through `decode_register_err_buf`. The local
                // buffer is needed because the helper writes via snprintf
                // and may truncate independently of our outer buffer's size.
                char helper_err[1024];
                std::memset(helper_err, 0, sizeof(helper_err));
                if (!sv_register_table_function(
                        db_handle,
                        "__sv_compute_create_from_yaml",
                        arg_types, 3,
                        sv_create_from_yaml_bind,
                        sv_create_from_yaml_function,
                        sv_create_from_yaml_init_local,
                        helper_err, sizeof(helper_err))) {
                    if (error_buf != nullptr && error_buf_len > 0) {
                        snprintf(error_buf, error_buf_len,
                            "sv_register_parser_hooks: failed to register "
                            "__sv_compute_create_from_yaml helper TF: %s",
                            helper_err);
                    }
                    return false;
                }
            }

            auto &config = DBConfig::GetConfig(db);

            // Phase 65.1 Plan 12 (WR-09 D-21): idempotence check across
            // same-process repeat LOAD semantic_views. `ParserExtension::Register`
            // unconditionally appends to `DBConfig::parser_extensions` (no dedup
            // API), so without this guard every LOAD grows the list by one entry
            // pointing at the same `sv_parser_override` — an unbounded "soft leak"
            // surfaced by 65-REVIEW.md WR-09.
            //
            // We iterate the public read-side iterator
            // `DBConfig::GetCallbackManager().ParserExtensions()`
            // (cpp/include/duckdb.cpp:281157) and compare each existing entry's
            // `parser_override` function pointer against the file-static
            // `sv_parser_override` symbol. Function-pointer equality is portable
            // ISO C++ here because `sv_parser_override` is a file-static C++
            // function defined in this same TU — both sides of the `==` see the
            // identical pointer value.
            //
            // CRITICAL: the entire `ParserExtension ext; ...; Register(...)` block
            // — including the `SemanticViewsParserInfo` allocation — must be
            // guarded. `Register` unconditionally appends to
            // `DBConfig::parser_extensions`; on the skip path re-registering
            // would grow the list unboundedly across repeated re-LOAD cycles
            // (the precise leak shape WR-09 / CR-01 (65.1) was meant to fix).
            //
            // The target is same-process repeat LOAD (e.g. a long-lived host
            // re-invoking `LOAD semantic_views` across sessions, or partial-failure
            // retries during `init_extension`). The theoretical concurrent-thread
            // LOAD race between the iterator check and `Register` is documented
            // as out of scope for v0.10.0 — see WR-09 Pitfall 3 / RESEARCH.md
            // lines 668-714. Hitting it would require user code to LOAD the
            // extension from two threads simultaneously, which is atypical for
            // DuckDB's extension-load API.
            //
            // The helper-TF registration above uses
            // `OnCreateConflict::ALTER_ON_CONFLICT` and is naturally
            // idempotent — no analog guard needed there.
            auto &cbmgr = config.GetCallbackManager();
            bool already_registered = false;
            for (auto &existing : cbmgr.ParserExtensions()) {
                if (existing.parser_override == sv_parser_override) {
                    already_registered = true;
                    break;
                }
            }

            if (!already_registered) {
                // Phase 65.1 CR-01 / WR-09: the `ParserExtension ext; ...;
                // Register(...)` block — including the `SemanticViewsParserInfo`
                // allocation — must stay INSIDE the dedup guard.
                // `ParserExtension::Register` unconditionally appends to
                // `DBConfig::parser_extensions` (no dedup API), so on the
                // `already_registered` skip path we must not Register (or
                // allocate) again — otherwise every redundant LOAD grows the
                // list by one entry, an unbounded soft leak over a long-lived
                // host process. AR-7: there is no longer a separate
                // `sv_make_override_context()` allocation to guard here.
                ParserExtension ext;
                ext.parser_override = sv_parser_override;
                // Phase 62 Plan 03: parse_function is the error-reporting layer.
                // parser_override owns the success path (rewrite + re-parse on the
                // caller's connection — transactional). When parser_override
                // defers (rc=2), the default parser fails on the unrecognised
                // prefix and DuckDB calls sv_parse_stub, which re-runs validation
                // and returns DISPLAY_EXTENSION_ERROR with `error_location` so
                // ParserException::SyntaxError renders `LINE 1: … ^` (caret).
                // sv_plan_unreachable is the required sibling — sv_parse_stub
                // never returns PARSE_SUCCESSFUL so it should never fire.
                ext.parse_function  = sv_parse_stub;
                ext.plan_function   = sv_plan_unreachable;
                // duckdb::shared_ptr<T> doesn't have an upcast constructor, so we
                // build the std::shared_ptr<ParserExtensionInfo> first (allocates
                // the SemanticViewsParserInfo and immediately upcasts) then hand
                // it to duckdb::shared_ptr's std-interop constructor.
                std::shared_ptr<ParserExtensionInfo> info_std(
                    new SemanticViewsParserInfo());
                ext.parser_info = duckdb::shared_ptr<ParserExtensionInfo>(info_std);
                ParserExtension::Register(config, ext);

                // FALLBACK_OVERRIDE: hook runs before default parser; misses
                // fall through cleanly. Caret regression (TECH-DEBT 22) is
                // resolved by parse_function above — sv_parse_stub renders
                // `LINE 1: ... ^` for every CREATE/DROP/ALTER validation error.
                config.SetOption("allow_parser_override_extension", Value("FALLBACK"));
            }

#ifdef SV_DUCKDB_2
            sv_register_grammar(db);
#endif

            return true;
        } catch (const std::exception &e) {
            if (error_buf != nullptr && error_buf_len > 0) {
                snprintf(error_buf, error_buf_len,
                    "sv_register_parser_hooks failed: %s", e.what());
            }
            return false;
        }
    }
}

// ---------------------------------------------------------------------------
// sv_count_parser_extensions -- Phase 65.1 Plan 12 Task 3 (D-21 + B-07
// plan-checker fix) structural verification helper.
// ---------------------------------------------------------------------------
// Counts entries in `DBConfig::GetCallbackManager().ParserExtensions()`
// whose `parser_override` function pointer equals the file-static
// `sv_parser_override` symbol. Used by
// `tests/parser_hook_idempotent.rs` as a stable, named symbol the
// structural test can reference (the cargo-test FFI path cannot
// exercise the extension binary directly under the
// `duckdb/loadable-extension` stubs; see that test's docstring).
//
// Option (a) per the B-07 plan-checker fix in 65.1-12-PLAN.md: PUBLIC
// helper (no cfg(test) gating). Read-only iteration is harmless in
// production binaries and gating would require conditional-compilation
// plumbing in the C++ TU for marginal benefit.
//
// Returns >= 0 on success (the count); -1 on failure with the message
// in `error_buf`. Same error_buf convention as the WR-02 ABI.
extern "C" {
    int32_t sv_count_parser_extensions(
        duckdb_database db_handle,
        char *error_buf, size_t error_buf_len) {
        try {
            if (db_handle == nullptr) {
                if (error_buf != nullptr && error_buf_len > 0) {
                    snprintf(error_buf, error_buf_len,
                        "sv_count_parser_extensions: null db_handle");
                }
                return -1;
            }
            auto *wrapper = reinterpret_cast<duckdb::DatabaseWrapper *>(
                db_handle->internal_ptr);
            if (wrapper == nullptr) {
                if (error_buf != nullptr && error_buf_len > 0) {
                    snprintf(error_buf, error_buf_len,
                        "sv_count_parser_extensions: null DatabaseWrapper");
                }
                return -1;
            }
            auto &db = *wrapper->database->instance;
            auto &config = DBConfig::GetConfig(db);
            auto &cbmgr = config.GetCallbackManager();
            // Phase 65.1 IN-02: saturate the counter at INT32_MAX rather
            // than wrapping. The documented contract is "returns >= 0 on
            // success; -1 on failure" — silent wrap to a negative value
            // would be misinterpreted as a registration failure by the
            // structural test. Reaching INT32_MAX is only plausible if
            // WR-09's dedup guard fails badly and the list grows
            // unbounded across re-LOAD cycles, but the saturation is
            // cheap defence-in-depth.
            int32_t count = 0;
            for (auto &existing : cbmgr.ParserExtensions()) {
                if (existing.parser_override == sv_parser_override) {
                    if (count < INT32_MAX) {
                        ++count;
                    }
                }
            }
            return count;
        } catch (const std::exception &e) {
            if (error_buf != nullptr && error_buf_len > 0) {
                snprintf(error_buf, error_buf_len,
                    "sv_count_parser_extensions failed: %s", e.what());
            }
            return -1;
        } catch (...) {
            if (error_buf != nullptr && error_buf_len > 0) {
                snprintf(error_buf, error_buf_len,
                    "sv_count_parser_extensions failed: unknown C++ exception");
            }
            return -1;
        }
    }
}
