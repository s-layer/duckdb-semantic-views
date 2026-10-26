// C++ helper for the DuckDB semantic_views extension.
//
// The Rust entry point (semantic_views_init_c_api, C_STRUCT ABI) owns the
// DuckDB handshake and function registration. After init, it calls
// sv_register_grammar_hooks() here, which registers the extension's grammar
// productions (DuckDB's PEG grammar extension) and the helper table function
// CREATE ... FROM YAML FILE lowers to. Every semantic-view statement is parsed
// by DuckDB's own parser through those productions; there is no parser
// override and no parse_function.
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
#include "duckdb/parser/expression/columnref_expression.hpp"
#include "duckdb/parser/parsed_data/create_table_info.hpp"
#include "duckdb/parser/parsed_data/create_view_info.hpp"
#include "duckdb/parser/query_node/delete_query_node.hpp"
#include "duckdb/parser/query_node/insert_query_node.hpp"
#include "duckdb/parser/query_node/update_query_node.hpp"
#include "duckdb/parser/result_modifier.hpp"
#include "duckdb/parser/statement/copy_statement.hpp"
#include "duckdb/parser/statement/create_statement.hpp"
#include "duckdb/parser/statement/delete_statement.hpp"
#include "duckdb/parser/statement/explain_statement.hpp"
#include "duckdb/parser/statement/insert_statement.hpp"
#include "duckdb/parser/statement/prepare_statement.hpp"
#include "duckdb/parser/statement/set_statement.hpp"
#include "duckdb/parser/statement/update_statement.hpp"
#include "duckdb/parser/tableref/showref.hpp"
#include "duckdb/planner/operator_extension.hpp"
#include "duckdb/function/replacement_scan.hpp"
#include <chrono>
#include <cstdint>
#include <cstring>
#include <memory>

// Destructors here release things while a DuckDB exception unwinds a bind:
// SvCallerScope restores the thread's previous caller, owned buffers are
// freed. MSVC emits those unwinding calls only under /EHsc (`_CPPUNWIND`),
// which the cc crate does not pass on its own (build.rs does). Without it a
// bind that threw would leave this thread pointing at a dead caller context.
// (A watcher thread the binds used to start crashed windows_amd64 this way
// from its introduction in #248 until the flag.)
#if defined(_MSC_VER) && !defined(_CPPUNWIND)
#error "shim.cpp must be compiled with C++ exception unwinding (/EHsc)"
#endif

using namespace duckdb;

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
// Rust FFI declarations (defined under src/)
// ---------------------------------------------------------------------------
extern "C" {
    // Grammar bridge (src/grammar/ffi.rs). The changes to register, as text
    // released with sv_free_buffer: 0 = ok, 1 = failed.
    uint8_t sv_grammar_changes_rust(char **out_ptr, size_t *out_len);
    // Lower one matched statement rule (its serialised parse subtree) to SQL:
    // 0 = ok (SQL in *sql_out_*, release with sv_free_buffer), 1 = invalid
    // statement (the whole message in *sql_out_* when the statement itself is
    // invalid, a capped copy in error_out either way; byte offset in
    // *position_out or UINT32_MAX), 2 = bridge failure (message in error_out).
    uint8_t sv_grammar_transform_rust(
        const uint8_t *tree_ptr, size_t tree_len,
        char **sql_out_ptr, size_t *sql_out_len,
        char *error_out, size_t error_out_len,
        uint32_t *position_out);

    // Releases a buffer the Rust side returned (SQL, rule text, read results).
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
    // The outer INSERT the CREATE lowers to wraps `new_def` with json_merge_patch
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
    // INSERT shape the CREATE lowers to (OR IGNORE / OR REPLACE / plain) already
    // encodes ON CONFLICT behaviour, so the helper itself does not branch on
    // it. Three-arg helper TF signature matches: (file_path, view_name,
    // comment).
    uint8_t sv_compute_create_from_yaml_rust(
        const uint8_t *content_ptr, size_t content_len,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *comment_ptr, size_t comment_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Rust FFI behind every CREATE: refuse a definition with a member DuckDB
    // cannot bind (src/ddl/alter_helpers_ffi.rs). Runs in an SvCallerScope.
    // Return codes: 0 every member binds; 1 a member does not bind, or the
    // caller was cancelled (the whole message in out_ptr/out_len); 3 internal
    // error (error_buf).
    uint8_t sv_check_create_members_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *def_ptr, size_t def_len,
        const uint8_t *database_ptr, size_t database_len,
        const uint8_t *schema_ptr, size_t schema_len,
        const uint8_t *target_schema_ptr, size_t target_schema_len,
        uint8_t skip_if_present,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Rust dispatcher for `list_semantic_views()`. The C++ bind callback runs
    // it inside an `SvCallerScope`, and it reads the catalog in the caller's
    // transaction through `sv_caller_definitions` (TECH-DEBT #19); no
    // connection crosses the boundary.
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
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 2 (Wave 1) — Rust dispatcher for the migrated
    // `list_terse_semantic_views()` table function. 5-column subset of
    // list_semantic_views (no `comment`). Runs in the caller's scope like
    // list_semantic_views.
    uint8_t sv_list_terse_semantic_views_bind_rust(
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 2 (Wave 1) — Rust dispatchers for the migrated
    // zero-arg "_all" TFs. All emit homogeneous VARCHAR rows; cell layout
    // matches the matching legacy duckdb-rs VTab. See per-dispatcher Rust
    // doc-headers for the exact column order.
    uint8_t sv_show_semantic_dimensions_all_bind_rust(
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_metrics_all_bind_rust(
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_facts_all_bind_rust(
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_materializations_all_bind_rust(
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 3 (Wave 2) — Rust dispatchers for the migrated
    // single-arg (view name) and two-arg (view name + metric name) TFs.
    // All take name (ptr, len) tuples and the caller's search path. The single-view show_columns / describe / show_semantic_*
    // variants emit VARCHAR rows; show_semantic_dimensions_for_metric
    // emits VARCHAR+BOOL rows (3 VARCHAR + 1 trailing BOOL per row).
    uint8_t sv_show_columns_in_semantic_view_bind_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_describe_semantic_view_bind_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_dimensions_bind_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_metrics_bind_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_facts_bind_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_materializations_bind_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_show_semantic_dimensions_for_metric_bind_rust(
        const uint8_t *view_name_ptr, size_t view_name_len,
        const uint8_t *metric_name_ptr, size_t metric_name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 4 (Wave 3) — Rust dispatchers for the migrated
    // scalar functions (get_ddl, read_yaml_from_semantic_view). Per-row
    // dispatch: each invocation runs inside the exec callback's `SvCallerScope`,
    // takes the input string args, and returns a heap-owned UTF-8 buffer for
    // the C++ side to copy into the result Vector via StringVector::AddString.
    //
    // Same wire convention as the bind dispatchers: rc=0 on success with
    // (out_ptr, out_len) populated (caller frees via sv_free_buffer), rc=1
    // on user-visible error (error_buf populated, raised as
    // InvalidInputException by the C++ side), rc=2 on internal panic.
    // `qualify` is the optional third GET_DDL argument (0 = bare name, non-zero
    // = schema-qualified). The two-argument arity passes 0.
    uint8_t sv_get_ddl_exec_rust(
        const uint8_t *type_ptr, size_t type_len,
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        uint8_t qualify,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
    uint8_t sv_read_yaml_from_semantic_view_exec_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Phase 65 Plan 05 Task 5 (Wave 5) — Rust dispatcher for the migrated
    // `explain_semantic_view(view_name, dimensions := [...], metrics := [...],
    // facts := [...])` table function, run in the caller's scope like the
    // metadata binds. The three optional named
    // LIST(VARCHAR) parameters are flattened on the C++ side into the
    // standard length-prefixed wire format (`u32 count; for each entry:
    // u32 len + bytes`) and passed as (ptr, len) pairs. A null pointer
    // with len=0 means the named parameter was not supplied (treated as
    // an empty list).
    uint8_t sv_explain_semantic_view_bind_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        const uint8_t *dims_ptr, size_t dims_len,
        const uint8_t *metrics_ptr, size_t metrics_len,
        const uint8_t *facts_ptr, size_t facts_len,
        const uint8_t *where_ptr, size_t where_len,
        size_t param_count,
        const uint8_t *order_ptr, size_t order_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Rust dispatcher for `semantic_view(view_name, dimensions := [...],
    // metrics := [...], ...)`, run in the caller's scope like the other
    // read-side dispatchers. Reads the definition, expands the request and
    // returns the SQL to plan, UTF-8: one SELECT, with its columns named and
    // ordered as the request asks. `sv_semantic_view_bind_replace` parses it
    // into the subquery DuckDB binds in the call's place.
    uint8_t sv_semantic_view_bind_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        const uint8_t *dims_ptr, size_t dims_len,
        const uint8_t *metrics_ptr, size_t metrics_len,
        const uint8_t *facts_ptr, size_t facts_len,
        const uint8_t *where_ptr, size_t where_len,
        size_t param_count,
        const uint8_t *order_ptr, size_t order_len,
        uint8_t fence,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);

    // Rust dispatcher for a standard-SQL query over a semantic view
    // (`sv_standard_query_bind`), run in the caller's scope like the other read-side
    // dispatchers. Empty payload: the name is no semantic view the caller can
    // see. Otherwise `view\n`, then one `<kind>\t<table>\t<name>\n` line per
    // member (kind D, M or F; table empty for a derived metric).
    uint8_t sv_standard_query_view_rust(
        const uint8_t *name_ptr, size_t name_len,
        const uint8_t *sp_ptr, size_t sp_len,
        char **out_ptr, size_t *out_len,
        char *error_buf, size_t error_buf_len);
}

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
         "rows before aggregation, and refers to the values of params "
         "(e.g. row('EU')) as $1, $2, ...; column_order (e.g. ['metrics', "
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
         "rather than calling this directly; it works as a subquery too.",
         {"SHOW SEMANTIC VIEWS;",
          // Qualified and quoted per part: a bare name is a unique-match
          // lookup, ambiguous once two schemas hold the same view name.
          "SELECT GET_DDL('SEMANTIC_VIEW', '\"' || replace(schema_name, '\"', "
          "'\"\"') || '\".\"' || replace(name, '\"', '\"\"') || '\"') FROM "
          "(SHOW SEMANTIC VIEWS);"},
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
        {"__sv_compute_create_from_yaml", {"file_path", "view_name", "comment", "schema", "skip_if_present"},
         "Internal helper behind CREATE SEMANTIC VIEW ... FROM YAML FILE. Use "
         "that statement instead; this function is not for direct use.",
         {},
         {"semantic_views", "internal"}},
        {"__sv_check_create", {"definition", "view_name", "schema", "skip_if_present"},
         "Internal helper behind CREATE SEMANTIC VIEW: checks that every member "
         "of the definition binds. Use that statement instead; this function is "
         "not for direct use.",
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
        // The values where_clause refers to as $1, $2, ...: a row, so each
        // keeps its own type (TECH-DEBT #105). The SEMANTIC_VIEW(...) construct
        // passes a prepared statement's parameters here.
        {"params", LogicalType::ANY},
        // The order the item lists' columns come out in, e.g. ['metrics',
        // 'dimensions']: Snowflake's SEMANTIC_VIEW(...) sets column order by
        // the order its clauses are written in, and the construct lowers to
        // this function. Lists left out follow in the default order
        // (dimensions, metrics, facts).
        {"column_order", list_varchar},
        // An explicit schema resolution order. Without it the bind reads the
        // caller's search path off its ClientContext (`sv_caller_search_path`).
        // Kept, reserved, because views stored by earlier releases carry the
        // `search_path := …` argument the extension's parser hook used to splice in
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
// they can read the catalog in the caller's transaction (`sv_caller_*`) and
// parse YAML without needing a long-lived extension-owned connection.
//
// Plan 04 consumes this to register `__sv_compute_create_from_yaml`. Plan 05
// will consume it (and add a scalar sibling) to migrate the 17 read-side
// callbacks off `query_conn` (H2).
//
// Header declaration in cpp/src/shim.hpp.
// Phase 65.1 Plan 02a (WR-02 D-08/D-09 + CR-02 D-05) — registration
// failures surface via the `(error_buf, error_buf_len)` trailing pair,
// the same ABI-stable channel the read-side dispatchers use. Per D-09 there is NO stderr write — ADBC/JDBC/Python
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
// The outer INSERT the CREATE lowers to (src/parse/native_sql.rs,
// landed in Task 4) wraps the helper TF's row via `json_merge_patch(new_def,
// json_object('created_on', strftime(now(), '%Y-%m-%dT%H:%M:%SZ'),
// 'database_name', current_database(), 'schema_name', current_schema()))`
// so metadata fields are populated on the caller's connection, preserving
// the D-21 transactional contract.

// Refuse a CREATE whose definition has a member DuckDB cannot bind (defined
// after SvCallerScope, below).
static void sv_check_create_members(ClientContext &context, const std::string &view_name,
                                    const std::string &definition, const std::string &schema,
                                    bool skip_if_present);

struct CreateFromYamlBindData : public TableFunctionData {
    std::string file_path;
    std::string view_name;
    std::string comment;
    std::string new_def;
    // Where the CREATE lands (the qualifier as written, empty for the
    // caller's current schema), and whether a view already there skips the
    // member check: true unless the CREATE replaces.
    std::string schema;
    bool skip_if_present = false;
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
    // Exactly 5, not "at least 5": the message promises a fixed signature, and
    // a 6-argument call silently ignoring the extra is the kind of quiet
    // tolerance that makes a helper's contract untrue. DuckDB's own arity check
    // should reject it first, so this is defence in depth either way.
    if (input.inputs.size() != 5) {
        throw BinderException(
            "__sv_compute_create_from_yaml: expected 5 arguments "
            "(file_path, view_name, comment, schema, skip_if_present)");
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
    if (input.inputs[3].IsNull() || input.inputs[4].IsNull()) {
        throw BinderException(
            "__sv_compute_create_from_yaml: schema and skip_if_present are required "
            "(positional args 3, 4; pass '' for the current schema)");
    }
    bd->file_path = input.inputs[0].GetValue<string>();
    bd->view_name = input.inputs[1].GetValue<string>();
    bd->comment   = input.inputs[2].GetValue<string>();
    bd->schema    = input.inputs[3].GetValue<string>();
    bd->skip_if_present = input.inputs[4].GetValue<bool>();

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
    sv_check_create_members(context, bd->view_name, bd->new_def, bd->schema, bd->skip_if_present);

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
// the bridge design. The shared bind helpers below (`sv_run_varchar_bind` et
// al.) run each dispatcher in an `SvCallerScope`, so it reads in the caller's
// transaction (TECH-DEBT #19).
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
// The caller of a read dispatcher (TECH-DEBT #19, #51)
// ---------------------------------------------------------------------------
//
// Every read-side bind below, and the two read scalars, runs its Rust
// dispatcher with `sv_bind_caller` naming the caller's `ClientContext`. The
// dispatcher reads the catalog, types members and plans EXPLAIN through the
// `sv_caller_*` functions further down, which work in that context and its
// transaction: a read sees what the statement around it sees, the
// transaction's own uncommitted CREATE / DROP / RENAME / ALTER TABLE and the
// session's temporary tables included. (Until TECH-DEBT #19 each read ran on a
// fresh `Connection(*context.db)`, which saw committed state only.)
//
// Cancellation follows from the same choice. A bind has no executor task loop
// to poll between, but the binds and plans `sv_caller_*` run are the caller's
// own, so DuckDB's interrupt checks inside them see the caller's flag, as they
// do in any other bind. Between them:
//
//   1. `sv_bind_caller_interrupted()` exposes the caller's flag to Rust, which
//      consults it before every member-type probe and stops typing
//      (`member_types`), so a cancelled cross-view SHOW ends at the next probe.
//   2. Once the dispatcher returns, a cancelled caller wins: a bare
//      `InterruptException` is thrown, never the `"<fn> failed: ..."` wrapper,
//      so the error is an INTERRUPT with the "Interrupted!" message: ADBC
//      maps a failure to ADBC_STATUS_CANCELLED only on an exact match against
//      `InterruptException::INTERRUPT_MESSAGE`. Thrown from a bind, the binder
//      appends the call's query location, exactly as it does when one of
//      DuckDB's own table functions (`read_csv` over a glob) is interrupted
//      while binding.
//
// The scope is per call and per thread; it saves and restores any outer scope
// so a bind nested inside a probe (a source that is itself a semantic view
// function) keeps its own caller.
static thread_local ClientContext *sv_bind_caller = nullptr;

extern "C" bool sv_bind_caller_interrupted() {
    return sv_bind_caller != nullptr &&
           sv_compat::is_cancelled(*sv_bind_caller, std::memory_order_relaxed);
}

// The values of the caller's `params` (TECH-DEBT #105), for the expansion an
// explain_semantic_view() bind plans through `sv_caller_explain`.
static thread_local const vector<Value> *sv_bind_params_values = nullptr;

class SvCallerScope {
public:
    explicit SvCallerScope(ClientContext &caller, const vector<Value> *params = nullptr)
        : caller_(caller), prev_(sv_bind_caller), prev_params_(sv_bind_params_values) {
        sv_bind_caller = &caller_;
        sv_bind_params_values = params;
    }

    ~SvCallerScope() {
        sv_bind_caller = prev_;
        sv_bind_params_values = prev_params_;
    }

    SvCallerScope(const SvCallerScope &) = delete;
    SvCallerScope &operator=(const SvCallerScope &) = delete;

    // A cancelled caller is authoritative however the dispatcher ended.
    void ThrowIfCancelled() const {
        sv_compat::throw_if_cancelled(caller_, std::memory_order_relaxed);
    }

private:
    ClientContext &caller_;
    ClientContext *prev_;
    const vector<Value> *prev_params_;
};

// CREATE binds every member, as Snowflake compiles each member expression at
// CREATE: a member DuckDB cannot bind (an unknown column or function, an
// invalid date part) refuses the view, naming the member, instead of being
// stored and failing only when queried. The probes run in the caller's
// context, against the database and schema the CREATE stamps (the caller's
// current ones), so they see the caller's transaction and temporary tables.
//
// Unless `skip_if_present` is false (OR REPLACE), a view already in the
// target schema skips the check: IF NOT EXISTS then writes nothing, and a
// plain CREATE reports "already exists", neither of which reads the body.
static void sv_check_create_members(ClientContext &context, const std::string &view_name,
                                    const std::string &definition, const std::string &target_schema,
                                    bool skip_if_present) {
    auto database = DatabaseManager::GetDefaultDatabase(context).GetIdentifierName();
    auto schema = ClientData::Get(context).catalog_search_path->GetDefault().GetSchema().GetIdentifierName();
    SvCallerScope caller(context);
    SvOwnedBuffer message;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = sv_check_create_members_rust(
        reinterpret_cast<const uint8_t *>(view_name.data()), view_name.size(),
        reinterpret_cast<const uint8_t *>(definition.data()), definition.size(),
        reinterpret_cast<const uint8_t *>(database.data()), database.size(),
        reinterpret_cast<const uint8_t *>(schema.data()), schema.size(),
        reinterpret_cast<const uint8_t *>(target_schema.data()), target_schema.size(),
        skip_if_present ? 1 : 0, &message.ptr, &message.len, error_buf, sizeof(error_buf));
    caller.ThrowIfCancelled();
    if (rc == 1) {
        throw BinderException("%s", message.to_string());
    }
    if (rc != 0) {
        throw InternalException("semantic_views: checking the CREATE's members failed: %s", string(error_buf));
    }
}

// __sv_check_create(definition, view_name, schema, skip_if_present): the definition an AS-body or
// FROM YAML CREATE lowers to (src/parse/native_sql.rs), passed through as one
// `new_def` row once every member binds. The outer INSERT reads it the way it
// reads __sv_compute_create_from_yaml's row, so both CREATE forms are checked.
static unique_ptr<FunctionData> sv_check_create_bind(ClientContext &context, TableFunctionBindInput &input,
                                                     vector<LogicalType> &return_types,
                                                     sv_compat::bind_names_t &names) {
    if (input.inputs.size() != 4) {
        throw BinderException("__sv_check_create: expected 4 arguments (definition, view_name, schema, "
                              "skip_if_present)");
    }
    for (auto &arg : input.inputs) {
        if (arg.IsNull()) {
            throw BinderException("__sv_check_create: definition, view name, schema and skip_if_present are "
                                  "required (pass '' for the current schema)");
        }
    }
    auto bd = make_uniq<CreateFromYamlBindData>();
    bd->new_def = input.inputs[0].GetValue<std::string>();
    bd->view_name = input.inputs[1].GetValue<std::string>();
    bd->schema = input.inputs[2].GetValue<std::string>();
    bd->skip_if_present = input.inputs[3].GetValue<bool>();
    sv_check_create_members(context, bd->view_name, bd->new_def, bd->schema, bd->skip_if_present);
    return_types.push_back(LogicalType::VARCHAR);
    names.emplace_back("new_def");
    return std::move(bd);
}

// The values of `params`: a row (`params := row('EU', 2024)`, or a struct),
// whose fields where_clause refers to as $1, $2, ... in order. Absent or NULL
// is no values.
static vector<Value> sv_read_params(TableFunctionBindInput &input, const char *fn_name) {
    auto it = input.named_parameters.find("params");
    if (it == input.named_parameters.end() || it->second.IsNull()) {
        return {};
    }
    if (!StructType::IsStruct(it->second.type())) {
        throw BinderException("%s: params must be a row of values, e.g. params := row('EU', 2024), not %s",
                              fn_name, it->second.type().ToString());
    }
    return StructValue::GetChildren(it->second);
}

// Put each value of `params` in place of the predicate's parameter that
// refers to it in the parsed expansion, as a constant of the value's own type:
// a value is never SQL text. The Rust side lets only `$1`, `$2`, ... through
// where_clause, checks each has a value (`expand::check_params_supplied`) and
// writes each as a `__sv_param(<n>)` call (`expand::where_clause::
// PARAM_FUNCTION`). A parameter left in the expansion comes from a stored
// member expression (TECH-DEBT #95): it has no value to take, and must not be
// given one of the predicate's or left to bind to one of the caller's
// statement, so it is refused.
static void sv_bind_params(QueryNode &node, const vector<Value> &values) {
    std::function<void(unique_ptr<ParsedExpression> &)> visit = [&](unique_ptr<ParsedExpression> &expr) {
        if (!expr) {
            return;
        }
        if (expr->GetExpressionClass() == ExpressionClass::PARAMETER) {
            throw BinderException(expr->GetQueryLocation(),
                                  "semantic_views: a member of this semantic view holds the prepared "
                                  "statement parameter $%s, which has no value in a query; re-create "
                                  "the view without it",
                                  expr->Cast<ParameterExpression>().Identifier().GetIdentifierName());
        }
        if (expr->GetExpressionClass() == ExpressionClass::FUNCTION) {
            auto &call = expr->Cast<FunctionExpression>();
            if (call.GetQualifiedName().Path().size() == 1 &&
                call.GetQualifiedName().Name().GetIdentifierName() == "__sv_param") {
                int64_t slot = 0;
                idx_t arguments = 0;
                for (auto &argument : call.Children()) {
                    if (++arguments == 1 && argument.GetExpressionClass() == ExpressionClass::CONSTANT &&
                        !argument.Cast<ConstantExpression>().GetLiteral().TryGetInt64(slot)) {
                        slot = 0;
                    }
                }
                if (arguments != 1) {
                    slot = 0;
                }
                if (slot < 1 || idx_t(slot) > values.size()) {
                    throw BinderException("semantic_views: where_clause refers to $%lld, but params has %llu values",
                                          static_cast<long long>(slot),
                                          static_cast<unsigned long long>(values.size()));
                }
                auto constant = ConstantExpression::FromValue(values[idx_t(slot) - 1]);
                constant->SetQueryLocation(expr->GetQueryLocation());
                constant->SetAlias(expr->GetAlias());
                expr = std::move(constant);
                return;
            }
        }
        if (expr->GetExpressionClass() == ExpressionClass::SUBQUERY) {
            auto &subquery = expr->Cast<SubqueryExpression>().SubqueryMutable();
            if (subquery && subquery->node) {
                sv_bind_params(*subquery->node, values);
            }
        }
        ParsedExpressionIterator::EnumerateChildren(*expr, visit);
    };
    ParsedExpressionIterator::EnumerateQueryNodeChildren(node, visit);
}

// Common idiom for the bind callback: run the Rust dispatcher
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
    SvCallerScope caller(context);
    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = dispatcher(&payload.ptr, &payload.len,
                            error_buf, sizeof(error_buf));
    caller.ThrowIfCancelled();
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
// expression the extension's parser hook used to splice into every read call (#19 /
// #25). It is read off the caller's `ClientContext`, a plain field read that
// takes no lock, so it works from a bind or a scalar's execution alike.
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
// with the caller's search path.
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
    SvCallerScope caller(context);
    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = dispatcher(reinterpret_cast<const uint8_t *>(name.data()), name.size(),
                            sp.empty() ? nullptr : sp.data(), sp.size(),
                            &payload.ptr, &payload.len,
                            error_buf, sizeof(error_buf));
    caller.ThrowIfCancelled();
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
    SvCallerScope caller(context);
    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = dispatcher(reinterpret_cast<const uint8_t *>(name1.data()), name1.size(),
                            reinterpret_cast<const uint8_t *>(name2.data()), name2.size(),
                            sp.empty() ? nullptr : sp.data(), sp.size(),
                            &payload.ptr, &payload.len,
                            error_buf, sizeof(error_buf));
    caller.ThrowIfCancelled();
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
        [](char **out_ptr, size_t *out_len, char *error_buf, size_t error_buf_len) {
            return sv_list_semantic_views_bind_rust(
                out_ptr, out_len, error_buf, error_buf_len);
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
        [](char **out_ptr, size_t *out_len, char *error_buf, size_t error_buf_len) {
            return sv_list_terse_semantic_views_bind_rust(
                out_ptr, out_len, error_buf, error_buf_len);
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
        [](char **out_ptr, size_t *out_len, char *error_buf, size_t error_buf_len) {
            return sv_show_semantic_dimensions_all_bind_rust(
                out_ptr, out_len, error_buf, error_buf_len);
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
        [](char **out_ptr, size_t *out_len, char *error_buf, size_t error_buf_len) {
            return sv_show_semantic_metrics_all_bind_rust(
                out_ptr, out_len, error_buf, error_buf_len);
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
        [](char **out_ptr, size_t *out_len, char *error_buf, size_t error_buf_len) {
            return sv_show_semantic_facts_all_bind_rust(
                out_ptr, out_len, error_buf, error_buf_len);
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
        [](char **out_ptr, size_t *out_len, char *error_buf, size_t error_buf_len) {
            return sv_show_semantic_materializations_all_bind_rust(
                out_ptr, out_len, error_buf, error_buf_len);
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
        [](const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_columns_in_semantic_view_bind_rust(
                np, nl, sp, spl, op, ol, eb, ebl);
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
        [](const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_describe_semantic_view_bind_rust(
                np, nl, sp, spl, op, ol, eb, ebl);
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
        [](const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_dimensions_bind_rust(
                np, nl, sp, spl, op, ol, eb, ebl);
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
        [](const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_metrics_bind_rust(
                np, nl, sp, spl, op, ol, eb, ebl);
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
        [](const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_facts_bind_rust(
                np, nl, sp, spl, op, ol, eb, ebl);
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
        [](const uint8_t *np, size_t nl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_materializations_bind_rust(
                np, nl, sp, spl, op, ol, eb, ebl);
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
        [](const uint8_t *vn, size_t vnl,
           const uint8_t *mn, size_t mnl,
           const uint8_t *sp, size_t spl,
           char **op, size_t *ol, char *eb, size_t ebl) {
            return sv_show_semantic_dimensions_for_metric_bind_rust(
                vn, vnl, mn, mnl, sp, spl, op, ol, eb, ebl);
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
// exec callback names the executing query's `ClientContext` as the caller
// (`SvCallerScope`) and calls a Rust dispatcher per row that performs the
// lookup + render, reading the catalog in that query's transaction
// (TECH-DEBT #19), as the table-function binds do.

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

    SvCallerScope caller(state.GetContext());
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

    SvCallerScope caller(state.GetContext());
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
// runs the Rust dispatcher `sv_explain_semantic_view_bind_rust` in an
// `SvCallerScope`; it reads the definition, expands the request and plans
// it in the caller's transaction (`sv_caller_explain`). Output is
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
    auto params = sv_read_params(input, "explain_semantic_view");
    std::vector<uint8_t> sp_buf = sv_search_path_payload(context, input);

    // The EXPLAIN of the expanded query plans the view's tables in the caller's
    // context, so a cancelled caller is raised bare, as in semantic_view(). The
    // plan is of the expansion with the values of `params` in place.
    SvCallerScope caller(context, &params);

    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));

    uint8_t rc = sv_explain_semantic_view_bind_rust(
        reinterpret_cast<const uint8_t *>(view_name.data()), view_name.size(),
        sp_buf.empty()      ? nullptr : sp_buf.data(),      sp_buf.size(),
        dims_buf.empty()    ? nullptr : dims_buf.data(),    dims_buf.size(),
        metrics_buf.empty() ? nullptr : metrics_buf.data(), metrics_buf.size(),
        facts_buf.empty()   ? nullptr : facts_buf.data(),   facts_buf.size(),
        where_clause.empty() ? nullptr : reinterpret_cast<const uint8_t *>(where_clause.data()),
        where_clause.size(),
        params.size(),
        order_buf.empty()   ? nullptr : order_buf.data(),   order_buf.size(),
        &payload.ptr, &payload.len,
        error_buf, sizeof(error_buf));
    caller.ThrowIfCancelled();
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
// The bind receives `ClientContext &` and plans in it (`sv_caller_explain`,
// TECH-DEBT #19); no connection is opened. Failures
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
// `sv_semantic_view_bind_rust` reads the definition in the caller's
// transaction too (`sv_caller_definitions`, TECH-DEBT #19), so a view created,
// dropped or renamed earlier in the transaction is seen that way.
//
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

// Resolve the names the expansion's member expressions use as DuckDB resolves
// them in a view stored in the schema stamped at CREATE, and pin each to what it
// finds, so DuckDB's bind of the expansion inside the caller's query can't pick
// up the caller's CTE of that name, or an object of that name in the caller's
// current schema. A view looks in the temporary schema, then its own schema,
// then its database's default schema, ahead of the session's search path
// (`Binder::GetSearchPath`); so does this, for a table (a member subquery's
// `FROM`), a function or macro, a table function, and a type a cast names. The
// `TABLES` entries arrive qualified already (`expand::qualify_and_quote_table_ref`,
// with a table that was temporary at CREATE as `temp.main`); a two-part name is
// looked up in the stamp's database first, as `TABLES` entries written that way
// were at CREATE.
//
// Left as written: a CTE name in scope, anything inside a hoisted where_clause
// subquery (`__sv_sqN`, the caller's own text, which resolves in the caller's
// query), an operator, a name already qualified with its catalog, and a name in
// no catalog (a replacement scan such as a file path). A table name in no
// catalog that the caller's query has a CTE for is refused, since the CTE would
// be read. The rest of where_clause is spliced into the expansion, so a function
// or type it names is resolved here like the definition's.
struct SvTablePinner {
    ClientContext &context;
    optional_ptr<Binder> caller;
    Identifier database;
    Identifier schema;
    std::vector<Identifier> scope;

    static bool IsHoisted(const Identifier &name) {
        return StringUtil::StartsWith(name.GetIdentifierName(), "__sv_sq");
    }

    optional_ptr<CatalogEntry> Lookup(CatalogType type, QualifiedName name) {
        EntryLookupInfo lookup(type, std::move(name));
        return Catalog::GetEntry(context, lookup, OnEntryNotFound::RETURN_NULL);
    }

    bool HasDatabase() {
        return !database.empty() && Catalog::GetCatalogEntry(context, database);
    }

    // The first of the places a view in the stamp's schema looks for `name`
    // that has it.
    optional_ptr<CatalogEntry> Anchored(CatalogType type, const Identifier &name) {
        if (auto entry = Lookup(type, QualifiedName(Identifier::TempCatalog(), Identifier::DefaultSchema(), name))) {
            return entry;
        }
        if (HasDatabase()) {
            auto own = schema.empty() ? Identifier::DefaultSchema() : schema;
            if (auto entry = Lookup(type, QualifiedName(database, own, name))) {
                return entry;
            }
            if (own != Identifier::DefaultSchema()) {
                return Lookup(type, QualifiedName(database, Identifier::DefaultSchema(), name));
            }
            return nullptr;
        }
        if (!schema.empty()) {
            return Lookup(type, QualifiedName(vector<Identifier> {schema}, name));
        }
        return nullptr;
    }

    // A function, macro or type found where the view looks first is named by
    // where it was found. One in the temporary schema needs no qualifier: the
    // caller's search path starts there too.
    template <class T>
    void PinObject(T &expr, CatalogType type) {
        auto &name = expr.GetQualifiedName();
        if (name.Path().size() != 1) {
            return;
        }
        auto entry = Anchored(type, name.Name());
        if (entry && entry->ParentCatalog().GetName() != Identifier::TempCatalog()) {
            expr.SetQualifiedName(entry->ParentCatalog().GetName(), entry->ParentSchema().name, name.Name());
        }
    }

    void Pin(BaseTableRef &ref) {
        auto &path = ref.GetQualifiedName().Path();
        if (path.size() == 2) {
            if (HasDatabase() && Lookup(CatalogType::TABLE_ENTRY, QualifiedName(database, path[0], path[1]))) {
                ref.SetQualifiedName(database, path[0], path[1]);
            }
            return;
        }
        if (path.size() != 1) {
            return;
        }
        auto table = path[0];
        for (auto &cte : scope) {
            if (cte == table) {
                return;
            }
        }
        auto entry = Anchored(CatalogType::TABLE_ENTRY, table);
        if (!entry) {
            entry = Lookup(CatalogType::TABLE_ENTRY, QualifiedName(table));
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
        switch (expr.GetExpressionClass()) {
        case ExpressionClass::SUBQUERY: {
            auto &subquery = expr.Cast<SubqueryExpression>().SubqueryMutable();
            if (subquery && subquery->node) {
                Node(*subquery->node);
            }
            break;
        }
        case ExpressionClass::FUNCTION: {
            auto &function = expr.Cast<FunctionExpression>();
            if (!function.IsOperator()) {
                PinObject(function, CatalogType::SCALAR_FUNCTION_ENTRY);
            }
            break;
        }
        case ExpressionClass::WINDOW:
            PinObject(expr.Cast<WindowExpression>(), CatalogType::SCALAR_FUNCTION_ENTRY);
            break;
        case ExpressionClass::CAST:
            Expr(expr.Cast<CastExpression>().TargetTypeMutable());
            break;
        case ExpressionClass::TYPE:
            PinObject(expr.Cast<TypeExpression>(), CatalogType::TYPE_ENTRY);
            break;
        default:
            break;
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
        case TableReferenceType::TABLE_FUNCTION: {
            auto &function = ref.Cast<TableFunctionRef>().function;
            if (function && function->GetExpressionClass() == ExpressionClass::FUNCTION) {
                PinObject(function->Cast<FunctionExpression>(), CatalogType::TABLE_FUNCTION_ENTRY);
                ParsedExpressionIterator::EnumerateChildren(*function, [&](ParsedExpression &child) { Expr(child); });
            } else {
                Expr(function);
            }
            break;
        }
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
        // A CTE is in scope from the next one on: its own body, unless it is
        // recursive, reads the table of its name.
        for (auto &cte : node.cte_map.map) {
            if (!IsHoisted(cte.first) && cte.second->query_node) {
                Node(*cte.second->query_node);
            }
            scope.push_back(cte.first);
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

// ---------------------------------------------------------------------------
// Reads in the caller's transaction (TECH-DEBT #19)
// ---------------------------------------------------------------------------
//
// The three functions a read dispatcher reads through. Each works in the
// `ClientContext` an `SvCallerScope` named on this thread, and in its
// transaction, so it sees what the statement around it sees. None of them runs
// a statement: `context.Query` from inside the caller's own bind or execution
// would wait on the context lock that statement holds.
//
//   * `sv_caller_definitions` scans `semantic_layer._definitions` in the
//     database new connections start in (the managed catalog CREATE writes to,
//     whatever the caller has `USE`-d) through the storage layer, with the
//     caller's transaction, so its uncommitted inserts and deletes count.
//   * `sv_caller_describe` binds a member-typing probe and reports its output
//     columns' names and types, as `DESCRIBE` would.
//   * `sv_caller_explain` plans a query to the physical plan `EXPLAIN` shows.
//
// The probe and explain SQL are pinned to the view's stamped database and
// schema with `SvTablePinner`, like semantic_view()'s expansion, so they bind
// the objects semantic_view() would.
//
// Results come back in malloc'd buffers the Rust side copies and releases with
// `sv_caller_free`. rc 0: the payload; 1: failed, the buffer holds the error;
// 2: the caller was interrupted, ditto.

static void sv_caller_put_u32(string &out, uint32_t v) {
    char b[4] = {char(v & 0xff), char((v >> 8) & 0xff), char((v >> 16) & 0xff), char((v >> 24) & 0xff)};
    out.append(b, 4);
}

static void sv_caller_put_str(string &out, const string &s) {
    if (s.size() > UINT32_MAX) {
        throw InvalidInputException("value too large for the read bridge (%llu bytes)", s.size());
    }
    sv_caller_put_u32(out, static_cast<uint32_t>(s.size()));
    out += s;
}

// A nullable cell: u8 1 for NULL, else u8 0 and the string.
static void sv_caller_put_value(string &out, const Value &value) {
    if (value.IsNull()) {
        out.push_back(1);
        return;
    }
    out.push_back(0);
    sv_caller_put_str(out, value.GetValue<string>());
}

template <typename Body>
static uint8_t sv_caller_run(char **out_ptr, size_t *out_len, Body &&body) {
    *out_ptr = nullptr;
    *out_len = 0;
    string out;
    uint8_t rc = 0;
    try {
        if (!sv_bind_caller) {
            throw InternalException("semantic_views: a catalog read ran outside a caller scope");
        }
        body(*sv_bind_caller, out);
    } catch (std::exception &ex) {
        ErrorData error(ex);
        rc = error.Type() == ExceptionType::INTERRUPT ? 2 : 1;
        out = error.Message();
    } catch (...) {
        rc = 1;
        out = "unknown error";
    }
    // At least one byte, so an empty payload is not a failed allocation.
    auto buffer = static_cast<char *>(malloc(out.empty() ? 1 : out.size()));
    if (!buffer) {
        return 1;
    }
    std::memcpy(buffer, out.data(), out.size());
    *out_ptr = buffer;
    *out_len = out.size();
    return rc;
}

// Parse `sql` (one SELECT, built-in grammar: it is generated SQL) and pin its
// names to the view's stamped database and schema.
static unique_ptr<SelectStatement> sv_caller_parse_pinned(ClientContext &context, const string &database,
                                                          const string &schema, const string &sql) {
    Parser parser = sv_compat::builtin_parser();
    parser.ParseQuery(sql);
    if (parser.statements.size() != 1 || parser.statements[0]->type != StatementType::SELECT_STATEMENT) {
        throw InvalidInputException("semantic_views: expected one SELECT statement: %s", sql);
    }
    auto select = unique_ptr_cast<SQLStatement, SelectStatement>(std::move(parser.statements[0]));
    SvTablePinner pinner {context, nullptr, Identifier(database), Identifier(schema), {}};
    pinner.Node(*select->node);
    if (sv_bind_params_values) {
        sv_bind_params(*select->node, *sv_bind_params_values);
    }
    return select;
}

extern "C" {
    // Payload: u8 present (whether the catalog table exists); the database
    // name; u32 row count; per row the nullable cells schema_name, name,
    // definition. With `only_name` set, only the rows whose name is
    // `name` byte for byte, so a per-row lookup copies one view, not all.
    uint8_t sv_caller_definitions(uint8_t only_name, const uint8_t *name_ptr, size_t name_len,
                                  char **out_ptr, size_t *out_len) {
        return sv_caller_run(out_ptr, out_len, [&](ClientContext &context, string &out) {
            const string wanted(reinterpret_cast<const char *>(name_ptr), name_ptr ? name_len : 0);
            auto database = DatabaseManager::Get(context).GetDefaultDatabase();
            optional_ptr<CatalogEntry> entry;
            if (!database.empty()) {
                EntryLookupInfo lookup(CatalogType::TABLE_ENTRY,
                                       QualifiedName(database, Identifier("semantic_layer"),
                                                     Identifier("_definitions")));
                entry = Catalog::GetEntry(context, lookup, OnEntryNotFound::RETURN_NULL);
            }
            const bool present = entry && entry->type == CatalogType::TABLE_ENTRY;
            out.push_back(present ? 1 : 0);
            sv_caller_put_str(out, database.GetIdentifierName());
            if (!present) {
                return;
            }
            auto &table = entry->Cast<TableCatalogEntry>();
            if (!table.IsDuckTable()) {
                throw InvalidInputException(
                    "semantic_views: semantic_layer._definitions in database \"%s\" is not a DuckDB table",
                    database.GetIdentifierName());
            }
            static const char *const COLUMNS[] = {"schema_name", "name", "definition"};
            vector<StorageIndex> column_ids;
            vector<LogicalType> types;
            for (auto column : COLUMNS) {
                Identifier name {string(column)};
                if (!table.ColumnExists(name)) {
                    throw InvalidInputException("semantic_views: semantic_layer._definitions has no column \"%s\"",
                                                column);
                }
                auto &definition = table.GetColumn(name);
                column_ids.emplace_back(definition.StorageOid());
                types.push_back(definition.Type());
            }
            auto &storage = table.GetStorage();
            auto &transaction = DuckTransaction::Get(context, table.ParentCatalog());
            TableScanState state;
            storage.InitializeScan(context, transaction, state, column_ids);
            DataChunk chunk;
            chunk.Initialize(context, types);
            string rows;
            uint32_t count = 0;
            while (true) {
                chunk.Reset();
                storage.Scan(transaction, chunk, state);
                if (chunk.size() == 0) {
                    break;
                }
                for (idx_t row = 0; row < chunk.size(); ++row) {
                    if (only_name) {
                        auto name = chunk.GetValue(1, row);
                        if (name.IsNull() || StringValue::Get(name) != wanted) {
                            continue;
                        }
                    }
                    for (idx_t column = 0; column < chunk.ColumnCount(); ++column) {
                        sv_caller_put_value(rows, chunk.GetValue(column, row));
                    }
                    ++count;
                }
            }
            sv_caller_put_u32(out, count);
            out += rows;
        });
    }

    // Payload: u32 column count; per column its name and its type as
    // `DESCRIBE` spells it.
    uint8_t sv_caller_describe(const uint8_t *database_ptr, size_t database_len,
                               const uint8_t *schema_ptr, size_t schema_len,
                               const uint8_t *sql_ptr, size_t sql_len,
                               char **out_ptr, size_t *out_len) {
        return sv_caller_run(out_ptr, out_len, [&](ClientContext &context, string &out) {
            auto select = sv_caller_parse_pinned(
                context, string(reinterpret_cast<const char *>(database_ptr), database_len),
                string(reinterpret_cast<const char *>(schema_ptr), schema_len),
                string(reinterpret_cast<const char *>(sql_ptr), sql_len));
            auto binder = Binder::CreateBinder(context);
            auto bound = binder->Bind(static_cast<SQLStatement &>(*select));
            sv_caller_put_u32(out, static_cast<uint32_t>(bound.types.size()));
            for (idx_t i = 0; i < bound.types.size(); ++i) {
                sv_caller_put_str(out, bound.names[i].GetIdentifierName());
                sv_caller_put_str(out, bound.types[i].ToString());
            }
        });
    }

    // Payload: the physical plan as `EXPLAIN` renders it.
    uint8_t sv_caller_explain(const uint8_t *database_ptr, size_t database_len,
                              const uint8_t *schema_ptr, size_t schema_len,
                              const uint8_t *sql_ptr, size_t sql_len,
                              char **out_ptr, size_t *out_len) {
        return sv_caller_run(out_ptr, out_len, [&](ClientContext &context, string &out) {
            auto select = sv_caller_parse_pinned(
                context, string(reinterpret_cast<const char *>(database_ptr), database_len),
                string(reinterpret_cast<const char *>(schema_ptr), schema_len),
                string(reinterpret_cast<const char *>(sql_ptr), sql_len));
            Planner planner(context);
            planner.CreatePlan(std::move(select));
            planner.Optimize();
            PhysicalPlanGenerator generator(context);
            auto plan = generator.Plan(std::move(planner.plan));
            out = plan->Root().ToString(context);
        });
    }

    void sv_caller_free(char *ptr) {
        free(ptr);
    }
}

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
    auto params = sv_read_params(input, "semantic_view");
    std::vector<uint8_t> sp_buf = sv_search_path_payload(context, input);

    // The catalog read runs in the caller's transaction; a cancelled caller
    // (interrupt or deadline) is raised bare, as in the metadata binds.
    SvCallerScope caller(context);

    // Expand the request into one SELECT whose parse positions are the call's.
    // With `fence`, the predicate is one that names only where_clause's
    // members (`expand::where_fence`), for the isolated bind below.
    auto expand = [&](bool fence) {
        SvOwnedBuffer payload;
        char error_buf[1024];
        std::memset(error_buf, 0, sizeof(error_buf));
        uint8_t rc = sv_semantic_view_bind_rust(
            reinterpret_cast<const uint8_t *>(view_name.data()), view_name.size(),
            sp_buf.empty()      ? nullptr : sp_buf.data(),      sp_buf.size(),
            dims_buf.empty()    ? nullptr : dims_buf.data(),    dims_buf.size(),
            metrics_buf.empty() ? nullptr : metrics_buf.data(), metrics_buf.size(),
            facts_buf.empty()   ? nullptr : facts_buf.data(),   facts_buf.size(),
            where_clause.empty() ? nullptr
                                 : reinterpret_cast<const uint8_t *>(where_clause.data()),
            where_clause.size(),
            params.size(),
            order_buf.empty()   ? nullptr : order_buf.data(),   order_buf.size(),
            fence ? 1 : 0,
            &payload.ptr, &payload.len,
            error_buf, sizeof(error_buf));
        caller.ThrowIfCancelled();
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
        // After pinning, so a value's type or constructor is the caller's own.
        sv_bind_params(*select->node, params);
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

// ---------------------------------------------------------------------------
// Grammar productions (DuckDB 2.0 PEG parser)
// ---------------------------------------------------------------------------
// A grammar extension adds rules to DuckDB's own PEG grammar, so our syntax
// can sit anywhere the grammar allows: `SELECT ... FROM (SHOW SEMANTIC VIEWS)`, a CTE, a view
// body (TECH-DEBT #77).
//
// The rules and their transforms live in Rust (src/grammar/). This side only
// bridges: it registers the rules Rust lists, and when a statement rule
// matches it serialises the matched subtree with the statement's tokens
// (wire format in src/grammar/tree.rs), has Rust lower it to SQL, and parses
// that SQL with the builtin parser.

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

// The matched statement's own part of the query: its byte span [start, end)
// and the tokens wholly inside it, [first, last). The iterator holds the whole
// query's tokens, so a transform that got all of them would cost every
// statement of a K-statement string the length of the whole string. Without a
// span the statement is the query.
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

// The statement's tree and its own tokens (the wire format is in src/grammar/tree.rs).
static string sv_grammar_serialise(PEGTransformer &transformer, ParseResult &pr, const SvGrammarSpan &span) {
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
    return out;
}

// Have Rust lower a matched statement rule to SQL, and parse the SQL. An
// invalid statement is a parser error at the offset Rust reports, which DuckDB
// renders as `LINE n: ... ^` like its own syntax errors.
static vector<unique_ptr<SQLStatement>> sv_grammar_lower(PEGTransformer &transformer, ParseResult &pr) {
    auto span = sv_grammar_span(transformer.token_iterator, pr);
    auto wire = sv_grammar_serialise(transformer, pr, span);
    SvOwnedBuffer sql_buf;
    char error_buf[1024];
    memset(error_buf, 0, sizeof(error_buf));
    uint32_t position = UINT32_MAX;
    uint8_t rc = sv_grammar_transform_rust(reinterpret_cast<const uint8_t *>(wire.data()), wire.size(),
                                           &sql_buf.ptr, &sql_buf.len, error_buf, sizeof(error_buf), &position);
    if (rc == 1) {
        // The heap copy is the whole message; error_buf would cut a long one
        // (a duplicate 600-character name, say) off mid-name.
        string message = sql_buf.len > 0 ? sql_buf.to_string() : string(error_buf);
        if (position != UINT32_MAX) {
            throw ParserException(QueryLocation(position, 0), "%s", message);
        }
        throw ParserException("%s", message);
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

// The `Parameter` matches under `pr`, in the order they are written.
static void sv_grammar_parameters(ParseResult &pr, vector<reference<ParseResult>> &out) {
    if (StringUtil::CIEquals(pr.Name(), "Parameter")) {
        out.push_back(pr);
        return;
    }
    switch (pr.type) {
    case ParseResultType::LIST:
        for (auto &c : pr.Cast<ListParseResult>().GetChildren()) {
            sv_grammar_parameters(c, out);
        }
        break;
    case ParseResultType::REPEAT:
        for (auto &c : pr.Cast<RepeatParseResult>().GetChildren()) {
            sv_grammar_parameters(c, out);
        }
        break;
    case ParseResultType::OPTIONAL: {
        auto &opt = pr.Cast<OptionalParseResult>();
        if (opt.HasResult()) {
            sv_grammar_parameters(opt.GetResult(), out);
        }
        break;
    }
    case ParseResultType::CHOICE:
        sv_grammar_parameters(pr.Cast<ChoiceParseResult>().GetResult(), out);
        break;
    default:
        break;
    }
}

// A table reference (`SEMANTIC_VIEW(...)`) lowers to `SELECT * FROM <ref>`,
// whose FROM is the table reference, alias included.
//
// A prepared statement's parameter in the construct is one of the statement's
// (TECH-DEBT #105). The lowering writes each as a placeholder in `params :=
// row(...)` (`grammar::query::PARAMETER_PLACEHOLDER`), in the order they are
// written, and each is put in its place here as DuckDB transforms it: so it is
// numbered, named and counted in the statement as it would be anywhere else.
static arena_ptr<TransformResultValue> sv_grammar_finalize_tableref(PEGTransformer &transformer,
                                                                    ParseResult &pr) {
    vector<reference<ParseResult>> parameters;
    sv_grammar_parameters(pr, parameters);
    vector<unique_ptr<ParsedExpression>> transformed;
    for (auto &parameter : parameters) {
        // `Parameter` is a choice of the four spellings; each has its transform.
        auto &spelling = parameter.get().type == ParseResultType::CHOICE
                             ? parameter.get().Cast<ChoiceParseResult>().GetResult()
                             : parameter.get();
        transformed.push_back(transformer.Transform<unique_ptr<ParsedExpression>>(spelling));
    }
    auto statements = sv_grammar_lower(transformer, pr);
    if (statements.size() != 1 || statements[0]->type != StatementType::SELECT_STATEMENT) {
        throw InternalException("semantic_views grammar: rule %s did not lower to one query", pr.Name());
    }
    auto &node = statements[0]->Cast<SelectStatement>().node;
    if (!node || node->type != QueryNodeType::SELECT_NODE || !node->Cast<SelectNode>().from_table) {
        throw InternalException("semantic_views grammar: rule %s did not lower to a table reference", pr.Name());
    }
    auto table = std::move(node->Cast<SelectNode>().from_table);
    idx_t next = 0;
    std::function<void(unique_ptr<ParsedExpression> &)> place = [&](unique_ptr<ParsedExpression> &expr) {
        if (!expr) {
            return;
        }
        if (expr->GetExpressionClass() == ExpressionClass::PARAMETER &&
            expr->Cast<ParameterExpression>().Identifier().GetIdentifierName() == "__sv_parameter") {
            if (next >= transformed.size()) {
                throw InternalException("semantic_views grammar: more parameter placeholders than parameters");
            }
            expr = std::move(transformed[next++]);
            return;
        }
        ParsedExpressionIterator::EnumerateChildren(*expr, place);
    };
    ParsedExpressionIterator::EnumerateTableRefChildren(*table, place);
    if (next != transformed.size()) {
        throw InternalException("semantic_views grammar: %llu parameters, %llu placeholders",
                                static_cast<unsigned long long>(transformed.size()),
                                static_cast<unsigned long long>(next));
    }
    return transformer.MakeResult<unique_ptr<TableRef>>(std::move(table));
}

static arena_ptr<TransformProcess> sv_grammar_start_tableref(PEGTransformer &transformer, ParseResult &pr) {
    return transformer.MakeProcess<FinalizeTransformProcess>(transformer, pr, sv_grammar_finalize_tableref);
}

// The variable a guard's value goes into (sv_grammar_finalize_statement).
static const char *SV_GRAMMAR_GUARD_VARIABLE = "__semantic_views_guard";

// A write statement lowers to several statements (the guards, then the DML).
// They go back as one MultiStatement, which the client unpacks and runs in
// order, as it does for a PIVOT's generated statements. DuckDB 2.0.0 serves
// such a statement's result from its one row-returning statement, which must
// come last, so each guard `SELECT CASE ... error(...) ... END` before the DML
// runs as `SET VARIABLE` of its value, which returns nothing and raises the
// guard's error the same way (PIVOT stages its values the same way). A RESET
// then drops the variable before the DML, whose RETURNING row is the result.
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
    bool staged_guard = false;
    for (idx_t i = 0; i + 1 < statements.size(); i++) {
        auto &statement = statements[i];
        if (statement->type != StatementType::SELECT_STATEMENT) {
            multi->statements.push_back(std::move(statement));
            continue;
        }
        auto guard = make_uniq<SubqueryExpression>();
        guard->GetSubqueryTypeMutable() = SubqueryType::SCALAR;
        guard->SubqueryMutable() = unique_ptr_cast<SQLStatement, SelectStatement>(std::move(statement));
        multi->statements.push_back(
            make_uniq<SetVariableStatement>(SV_GRAMMAR_GUARD_VARIABLE, std::move(guard), SetScope::VARIABLE));
        staged_guard = true;
    }
    if (staged_guard) {
        multi->statements.push_back(make_uniq<ResetVariableStatement>(SV_GRAMMAR_GUARD_VARIABLE, SetScope::VARIABLE));
    }
    multi->statements.push_back(std::move(statements.back()));
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
// per-connection setting, and `SetLocal` replaces the connection's compiled
// grammar unsynchronised, so this only ever runs on the connection's own thread:
// from an ExtensionCallback for a connection opened after LOAD, and from
// SemanticViewsGrammarPending for one that already existed.
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

// ---------------------------------------------------------------------------
// Standard-SQL queries over a semantic view by name (Phase 5)
// ---------------------------------------------------------------------------
// A semantic view can be queried with an ordinary SELECT, as in Snowflake:
//
//     SELECT region, AGG(revenue) FROM shop WHERE channel = 'web' GROUP BY region
//
// `shop` names no table, so the statement fails to bind. The planner then
// offers the statement to each operator extension's Bind (planner.cpp, the
// fallback after a bind error), and SvStandardQuery's re-parses its text,
// rewrites each query block that reads a semantic view into the
// SEMANTIC_VIEW(...) construct, and binds that:
//
//     SELECT __svd0 AS region, __svm0 AS "agg(revenue)"
//     FROM SEMANTIC_VIEW(shop DIMENSIONS region AS __svd0 METRICS revenue AS __svm0
//                        WHERE channel = 'web')
//
// The block's GROUP BY expressions become DIMENSIONS items, each AGG(m) and
// each other aggregate a METRICS item, and each SELECT item that is neither
// (an expression over facts) a FACTS item; WHERE moves into the construct and
// HAVING becomes the outer WHERE. Everything from member resolution on is the
// construct's, with its checks. The rewrite enforces only the rules that are
// the query form's own (Snowflake's): a metric is passed to AGG, a dimension
// in SELECT is in GROUP BY, HAVING refers only to aggregated metrics, and the
// block that reads the view has no join, window function, QUALIFY, PIVOT,
// sample or SELECT *.
//
// The planner fallback is the one hook every bind path goes through: a query,
// a prepared statement, and ClientContext::BindStatement, which the Python
// client's relational API (`duckdb.sql()`, `con.query()`) binds with. (A
// ClientContextState's OnPlanningError covers only the first two.) It hands
// over the binder of the failed attempt, which still holds that attempt's
// bindings, and a statement binding may have changed in place; so the
// statement is parsed afresh from its text, and bound with a child of that
// binder, which shares the query's table indexes and parameters but none of
// its bindings.
//
// A statement no semantic view is read by keeps DuckDB's own error: the
// fallback binds nothing unless it rewrote something.

// One member of the view, as `sv_standard_query_view_rust` lists it.
struct SvStdMember {
    char kind; // 'D', 'M' or 'F'
    string table;
    string name;
};

static string sv_std_lower(const Identifier &id) {
    return StringUtil::Lower(id.GetIdentifierName());
}

// The children of `expr` that belong to its own query block: for a subquery,
// only its left side (`x` in `x IN (SELECT ...)`), as the subquery is a block
// of its own.
static void sv_std_children(const ParsedExpression &expr, const std::function<void(const ParsedExpression &)> &f) {
    if (expr.GetExpressionClass() == ExpressionClass::SUBQUERY) {
        auto &child = expr.Cast<SubqueryExpression>().GetChild();
        if (child) {
            f(*child);
        }
        return;
    }
    ParsedExpressionIterator::EnumerateChildren(expr, f);
}

static void sv_std_children(ParsedExpression &expr, const std::function<void(unique_ptr<ParsedExpression> &)> &f) {
    if (expr.GetExpressionClass() == ExpressionClass::SUBQUERY) {
        auto &child = expr.Cast<SubqueryExpression>().GetChildMutable();
        if (child) {
            f(child);
        }
        return;
    }
    ParsedExpressionIterator::EnumerateChildren(expr, f);
}

static unique_ptr<TableRef> sv_std_replacement(ClientContext &context, const BaseTableRef &ref);

// Whether `ref` reads a semantic view, and if so its members. A name with a
// table or view behind it is not one, nor is a CTE in scope.
static bool sv_std_lookup_view(ClientContext &context, const BaseTableRef &ref,
                               const std::set<string> &ctes, vector<SvStdMember> &members) {
    auto &qn = ref.GetQualifiedName();
    if (qn.Path().size() == 1 && ctes.count(sv_std_lower(qn.Name()))) {
        return false;
    }
    try {
        EntryLookupInfo lookup(CatalogType::TABLE_ENTRY, qn);
        if (Catalog::GetEntry(context, lookup, OnEntryNotFound::RETURN_NULL)) {
            return false;
        }
    } catch (std::exception &) {
        // An unknown catalog or schema holds no table, and no semantic view
        // either; the Rust lookup says which.
    }
    // Nor is a name a replacement scan resolves (a Python DataFrame or
    // relation of that name), which the binder resolved before the view.
    if (sv_std_replacement(context, ref)) {
        return false;
    }
    const string name = qn.ToString();
    const std::vector<uint8_t> sp = sv_caller_search_path(context);
    SvCallerScope caller(context);
    SvOwnedBuffer payload;
    char error_buf[1024];
    std::memset(error_buf, 0, sizeof(error_buf));
    uint8_t rc = sv_standard_query_view_rust(reinterpret_cast<const uint8_t *>(name.data()), name.size(),
                                             sp.empty() ? nullptr : sp.data(), sp.size(), &payload.ptr,
                                             &payload.len, error_buf, sizeof(error_buf));
    caller.ThrowIfCancelled();
    if (rc != 0) {
        throw BinderException(string(error_buf));
    }
    string text(payload.ptr ? payload.ptr : "", payload.len);
    if (!StringUtil::StartsWith(text, "view\n")) {
        return false;
    }
    for (auto &line : StringUtil::Split(text.substr(5), '\n')) {
        auto fields = StringUtil::Split(line, '\t');
        // Split drops empty fields: a derived metric has no table.
        if (fields.size() == 3) {
            members.push_back({fields[0][0], fields[1], fields[2]});
        } else if (fields.size() == 2) {
            members.push_back({fields[0][0], string(), fields[1]});
        }
    }
    return true;
}

// The table a replacement scan puts in place of `ref`, as the binder asks
// for it when no catalog entry has the name (Binder::BindWithReplacementScan,
// connection-scoped scans first). The Python client resolves a relation used
// by its variable name this way, to a subquery over the relation's own text,
// which the statement's text does not hold.
static unique_ptr<TableRef> sv_std_replacement(ClientContext &context, const BaseTableRef &ref) {
    if (!context.config.use_replacement_scans) {
        return nullptr;
    }
    try {
        ReplacementScanInput input(ref.GetQualifiedName());
        for (const auto &scan : context.config.replacement_scans) {
            if (auto result = scan->function(context, input, scan->data.get())) {
                return result;
            }
        }
        for (const auto &scan : DBConfig::GetConfig(context).replacement_scans.Snapshot()) {
            if (auto result = scan->function(context, input, scan->data.get())) {
                return result;
            }
        }
    } catch (std::exception &) {
        // The binder asks again, and reports what it finds.
    }
    return nullptr;
}

class SvStandardRewrite {
public:
    SvStandardRewrite(ClientContext &context, SelectNode &select, BaseTableRef &base, vector<SvStdMember> members,
                      const std::set<string> &outer)
        : context_(context), select_(select), base_(base), members_(std::move(members)), outer_(outer) {
        auto &qn = base_.GetQualifiedName();
        view_label_ = qn.Name().GetIdentifierName();
        qualifier_ = base_.alias.empty() ? sv_std_lower(qn.Name()) : sv_std_lower(base_.alias);
        for (auto &m : members_) {
            if (!m.table.empty()) {
                tables_.insert(m.table);
            }
        }
    }

    void Run() {
        Refuse();
        ForEachClauseExpression([&](unique_ptr<ParsedExpression> &e) { StripQualifier(e); });
        ForEachClauseExpression([&](unique_ptr<ParsedExpression> &e) { RefuseCorrelation(*e); });
        CollectGroups();
        std::set<string> aliases;
        for (auto &item : select_.select_list) {
            if (item->HasAlias()) {
                aliases.insert(sv_std_lower(item->GetAlias()));
            }
        }
        for (auto &item : select_.select_list) {
            RewriteSelectItem(item);
        }
        if (select_.having) {
            Replace(select_.having, Clause::HAVING);
            RefuseLeftoverInHaving(*select_.having);
        }
        for (auto &modifier : select_.modifiers) {
            if (modifier->type == ResultModifierType::ORDER_MODIFIER) {
                for (auto &order : modifier->Cast<OrderModifier>().orders) {
                    // A bare name a SELECT item is aliased to is that item, as
                    // in DuckDB and Snowflake, even when a member has the name:
                    // left as written, it binds to the rewritten item's alias.
                    if (!IsAlias(*order.expression, aliases)) {
                        Replace(order.expression, Clause::ORDER_BY);
                    }
                }
            }
        }
        Graft();
    }

private:
    enum class Clause { SELECT, HAVING, ORDER_BY };

    static bool IsAlias(const ParsedExpression &expr, const std::set<string> &aliases) {
        if (expr.GetExpressionClass() != ExpressionClass::COLUMN_REF) {
            return false;
        }
        auto &ref = expr.Cast<ColumnRefExpression>();
        return !ref.IsQualified() && aliases.count(sv_std_lower(ref.ColumnNames()[0]));
    }

    [[noreturn]] void Fail(const string &message) const {
        throw BinderException("semantic view '%s': %s", view_label_, message);
    }

    // What the query form leaves out (Snowflake's list, plus what has no
    // meaning over a semantic view).
    void Refuse() {
        if (select_.qualify) {
            Fail("QUALIFY is not supported in a query over a semantic view; query it in a subquery and filter "
                 "the result");
        }
        if (select_.sample || base_.sample) {
            Fail("sampling is not supported in a query over a semantic view; query it in a subquery and "
                 "sample the result");
        }
        if (base_.at_clause) {
            Fail("AT (time travel) is not supported on a semantic view");
        }
        if (!base_.column_name_alias.empty()) {
            Fail("a column alias list is not supported on a semantic view; alias the SELECT items instead");
        }
        if (select_.groups.grouping_sets.size() > 1) {
            Fail("GROUPING SETS, ROLLUP and CUBE are not supported in a query over a semantic view");
        }
        for (auto &item : select_.select_list) {
            if (item->GetExpressionClass() == ExpressionClass::STAR) {
                Fail("SELECT * is not supported in a query over a semantic view; name the dimensions, metrics "
                     "and facts to return");
            }
        }
        ForEachClauseExpression([&](unique_ptr<ParsedExpression> &e) { RefuseWindows(*e); });
    }

    void RefuseWindows(const ParsedExpression &expr) const {
        if (expr.GetExpressionClass() == ExpressionClass::WINDOW) {
            Fail("window functions are not supported in a query over a semantic view; query it in a subquery "
                 "and apply the window to the result");
        }
        sv_std_children(expr, [&](const ParsedExpression &child) { RefuseWindows(child); });
    }

    // Every expression of the block's own clauses (not of its subqueries).
    template <typename F>
    void ForEachClauseExpression(F &&f) {
        for (auto &item : select_.select_list) {
            f(item);
        }
        for (auto &group : select_.groups.group_expressions) {
            f(group);
        }
        for (auto *clause : {&select_.where_clause, &select_.having}) {
            if (*clause) {
                f(*clause);
            }
        }
        for (auto &modifier : select_.modifiers) {
            if (modifier->type == ResultModifierType::ORDER_MODIFIER) {
                for (auto &order : modifier->Cast<OrderModifier>().orders) {
                    f(order.expression);
                }
            }
        }
    }

    // `s.region` in `FROM shop s` (or `shop.region` in `FROM shop`) names the
    // member `region`: drop the qualifier. One that names a logical table of
    // the view (`c.region`) stays, for the construct to resolve.
    void StripQualifier(unique_ptr<ParsedExpression> &expr) {
        if (expr->GetExpressionClass() == ExpressionClass::COLUMN_REF) {
            auto &names = expr->Cast<ColumnRefExpression>().ColumnNamesMutable();
            if (names.size() >= 2 && sv_std_lower(names[0]) == qualifier_ &&
                (names.size() == 3 || !tables_.count(qualifier_))) {
                names.erase(names.begin());
            }
            return;
        }
        sv_std_children(*expr, [&](unique_ptr<ParsedExpression> &child) { StripQualifier(child); });
    }

    // A reference through a name an enclosing query binds is correlated, which
    // Snowflake refuses; and WHERE moves into the construct, where such a name
    // would silently resolve to the view's own logical table of that name.
    void RefuseCorrelation(const ParsedExpression &expr) const {
        if (expr.GetExpressionClass() == ExpressionClass::COLUMN_REF) {
            auto &names = expr.Cast<ColumnRefExpression>().ColumnNames();
            if (names.size() >= 2 && outer_.count(sv_std_lower(names[0]))) {
                Fail("'" + Text(expr) + "' refers to '" + names[0].GetIdentifierName() +
                     "' of an enclosing query; correlated references are not supported in a query over a "
                     "semantic view (if '" + names[0].GetIdentifierName() +
                     "' is meant as the view's own table, give the enclosing one another alias)");
            }
            return;
        }
        sv_std_children(expr, [&](const ParsedExpression &child) { RefuseCorrelation(child); });
    }

    // The members a column reference can name: by name, and by table too when
    // it is qualified.
    vector<const SvStdMember *> Resolve(const ColumnRefExpression &ref) const {
        vector<const SvStdMember *> out;
        auto &names = ref.ColumnNames();
        if (names.size() > 2) {
            return out;
        }
        const string name = sv_std_lower(names.back());
        const string table = names.size() == 2 ? sv_std_lower(names[0]) : string();
        for (auto &m : members_) {
            if (m.name == name && (names.size() == 1 || m.table == table)) {
                out.push_back(&m);
            }
        }
        return out;
    }

    static bool AllOfKind(const vector<const SvStdMember *> &found, char kind) {
        if (found.empty()) {
            return false;
        }
        for (auto *m : found) {
            if (m->kind != kind) {
                return false;
            }
        }
        return true;
    }

    // An expression's text, without its alias.
    static string Text(const ParsedExpression &expr) {
        auto copy = expr.Copy();
        copy->ClearAlias();
        if (copy->GetExpressionClass() == ExpressionClass::FUNCTION) {
            auto &f = copy->Cast<FunctionExpression>();
            if (sv_std_lower(f.FunctionName()) == "count_star" && f.GetArguments().empty()) {
                // COUNT(*) parses as count_star(), which reads back as no
                // function the construct knows.
                string text = "COUNT(*)";
                if (f.Filter()) {
                    text += " FILTER (WHERE " + f.Filter()->ToString() + ")";
                }
                return text;
            }
        }
        return copy->ToString();
    }

    // The key two expressions are the same group by: the expression with
    // each column reference that names exactly one member replaced by that
    // member, however it is written, and every other one lower-cased, as
    // DuckDB matches identifiers case-insensitively.
    string Key(const ParsedExpression &expr) const {
        auto copy = expr.Copy();
        Canonicalize(copy);
        return Text(*copy);
    }

    void Canonicalize(unique_ptr<ParsedExpression> &expr) const {
        if (expr->GetExpressionClass() == ExpressionClass::COLUMN_REF) {
            auto found = Resolve(expr->Cast<ColumnRefExpression>());
            if (found.size() == 1) {
                expr = make_uniq<ColumnRefExpression>(Identifier("\x01" + found[0]->table + "." + found[0]->name));
                return;
            }
            for (auto &name : expr->Cast<ColumnRefExpression>().ColumnNamesMutable()) {
                name = Identifier(sv_std_lower(name));
            }
            return;
        }
        sv_std_children(*expr, [&](unique_ptr<ParsedExpression> &child) { Canonicalize(child); });
    }

    bool IsAggregate(const FunctionExpression &f) const {
        try {
            // Functions share one catalog set: a lookup by name finds a
            // scalar function of that name too.
            EntryLookupInfo lookup(CatalogType::AGGREGATE_FUNCTION_ENTRY, f.GetQualifiedName());
            auto entry = Catalog::GetEntry(context_, lookup, OnEntryNotFound::RETURN_NULL);
            return entry && entry->type == CatalogType::AGGREGATE_FUNCTION_ENTRY;
        } catch (std::exception &) {
            return false;
        }
    }

    static bool ContainsAggregateCall(const ParsedExpression &expr, const SvStandardRewrite &rw) {
        if (expr.GetExpressionClass() == ExpressionClass::FUNCTION) {
            auto &f = expr.Cast<FunctionExpression>();
            if (sv_std_lower(f.FunctionName()) == "agg" || rw.IsAggregate(f)) {
                return true;
            }
        }
        bool found = false;
        sv_std_children(expr, [&](const ParsedExpression &child) {
            found = found || ContainsAggregateCall(child, rw);
        });
        return found;
    }

    // The GROUP BY keys, with ordinals, SELECT aliases and GROUP BY ALL
    // resolved to the SELECT item they stand for.
    void CollectGroups() {
        vector<unique_ptr<ParsedExpression>> keys;
        if (select_.aggregate_handling == AggregateHandling::FORCE_AGGREGATES) {
            for (auto &item : select_.select_list) {
                if (!ContainsAggregateCall(*item, *this) && HasColumnRef(*item)) {
                    keys.push_back(item->Copy());
                }
            }
        }
        for (auto &group : select_.groups.group_expressions) {
            keys.push_back(ResolveGroup(*group));
        }
        for (auto &key : keys) {
            key->ClearAlias();
            auto k = Key(*key);
            bool seen = false;
            for (auto &existing : group_keys_) {
                seen = seen || existing == k;
            }
            if (!seen) {
                group_keys_.push_back(k);
                dimensions_.push_back(IsLoneFact(*key) ? "(" + Text(*key) + ")" : Text(*key));
            }
        }
    }

    // A key that is a fact on its own. DIMENSIONS reads a bare name as a
    // dimension's, so the key goes in as the expression it is, as any other
    // expression over facts does: one row per value.
    bool IsLoneFact(const ParsedExpression &key) const {
        if (key.GetExpressionClass() != ExpressionClass::COLUMN_REF) {
            return false;
        }
        auto found = Resolve(key.Cast<ColumnRefExpression>());
        return found.size() == 1 && found[0]->kind == 'F';
    }

    unique_ptr<ParsedExpression> ResolveGroup(const ParsedExpression &group) const {
        if (group.GetExpressionClass() == ExpressionClass::CONSTANT) {
            auto &value = group.Cast<ConstantExpression>().GetLiteral();
            int64_t ordinal = 0;
            if (value.IsNumeric() && value.TryGetInt64(ordinal)) {
                if (ordinal < 1 || idx_t(ordinal) > select_.select_list.size()) {
                    Fail(StringUtil::Format("GROUP BY position %lld is not in the SELECT list",
                                            static_cast<long long>(ordinal)));
                }
                return select_.select_list[idx_t(ordinal) - 1]->Copy();
            }
        }
        if (group.GetExpressionClass() == ExpressionClass::COLUMN_REF) {
            auto &ref = group.Cast<ColumnRefExpression>();
            if (!ref.IsQualified() && Resolve(ref).empty()) {
                for (auto &item : select_.select_list) {
                    if (item->HasAlias() && sv_std_lower(item->GetAlias()) == sv_std_lower(ref.GetColumnName())) {
                        return item->Copy();
                    }
                }
            }
        }
        return group.Copy();
    }

    static bool HasColumnRef(const ParsedExpression &expr) {
        if (expr.GetExpressionClass() == ExpressionClass::COLUMN_REF) {
            return true;
        }
        bool found = false;
        sv_std_children(expr, [&](const ParsedExpression &child) { found = found || HasColumnRef(child); });
        return found;
    }

    static unique_ptr<ParsedExpression> Column(const string &prefix, idx_t i) {
        return make_uniq<ColumnRefExpression>(Identifier(prefix + std::to_string(i)));
    }

    static idx_t Add(vector<string> &items, const string &text) {
        for (idx_t i = 0; i < items.size(); i++) {
            if (items[i] == text) {
                return i;
            }
        }
        items.push_back(text);
        return items.size() - 1;
    }

    // A metric aggregated: AGG(m), or MIN / MAX / ANY_VALUE(m), which Snowflake
    // evaluates as AGG(m). Returns the METRICS item, or empty for another call.
    string MetricOf(const FunctionExpression &f) const {
        const string name = sv_std_lower(f.FunctionName());
        const bool plain = !f.Distinct() && !f.Filter() && (!f.OrderBy() || f.OrderBy()->orders.empty());
        if (name == "agg") {
            if (f.GetArguments().size() != 1 || !plain) {
                Fail("AGG takes one metric, e.g. AGG(revenue)");
            }
            return Text(f.GetArguments()[0].GetExpression());
        }
        if ((name == "min" || name == "max" || name == "any_value") && f.GetArguments().size() == 1 && plain) {
            auto &arg = f.GetArguments()[0].GetExpression();
            if (arg.GetExpressionClass() == ExpressionClass::COLUMN_REF &&
                AllOfKind(Resolve(arg.Cast<ColumnRefExpression>()), 'M')) {
                return Text(arg);
            }
        }
        return string();
    }

    // Put the construct's column in place of each group key and each
    // aggregate in `expr`; a member reference left over is the clause's to
    // judge.
    void Replace(unique_ptr<ParsedExpression> &expr, Clause clause) {
        if (!expr || expr->GetExpressionClass() == ExpressionClass::STAR) {
            return;
        }
        if (expr->GetExpressionClass() == ExpressionClass::SUBQUERY) {
            sv_std_children(*expr, [&](unique_ptr<ParsedExpression> &child) { Replace(child, clause); });
            return;
        }
        auto alias = expr->GetAlias();
        auto key = Key(*expr);
        for (idx_t i = 0; i < group_keys_.size(); i++) {
            if (group_keys_[i] == key && HasColumnRef(*expr)) {
                if (clause == Clause::HAVING) {
                    Fail("HAVING may refer only to metrics passed to AGG or another aggregate, not '" +
                         Text(*expr) + "'; filter dimensions in WHERE");
                }
                expr = Column("__svd", i);
                expr->SetAlias(alias);
                return;
            }
        }
        if (expr->GetExpressionClass() == ExpressionClass::FUNCTION) {
            auto &f = expr->Cast<FunctionExpression>();
            auto metric = MetricOf(f);
            if (metric.empty() && IsAggregate(f)) {
                metric = Text(f);
            }
            if (!metric.empty()) {
                expr = Column("__svm", Add(metrics_, metric));
                expr->SetAlias(alias);
                return;
            }
        }
        if (expr->GetExpressionClass() == ExpressionClass::COLUMN_REF) {
            auto found = Resolve(expr->Cast<ColumnRefExpression>());
            if (AllOfKind(found, 'M')) {
                Fail("metric '" + Text(*expr) + "' must be passed to AGG, e.g. AGG(" + Text(*expr) + ")");
            }
            if (AllOfKind(found, 'D') && clause != Clause::HAVING) {
                Fail("dimension '" + Text(*expr) + "' must appear in GROUP BY");
            }
            return;
        }
        sv_std_children(*expr, [&](unique_ptr<ParsedExpression> &child) { Replace(child, clause); });
    }

    // Column references the rewrite left in `expr`: none is one of its own.
    static void Leftovers(const ParsedExpression &expr, vector<const ColumnRefExpression *> &out) {
        if (expr.GetExpressionClass() == ExpressionClass::COLUMN_REF) {
            auto &ref = expr.Cast<ColumnRefExpression>();
            auto first = ref.ColumnNames()[0].GetIdentifierName();
            if (!(ref.ColumnNames().size() == 1 && StringUtil::StartsWith(first, "__sv"))) {
                out.push_back(&ref);
            }
            return;
        }
        sv_std_children(expr, [&](const ParsedExpression &child) { Leftovers(child, out); });
    }

    static bool HasOwnColumn(const ParsedExpression &expr) {
        if (expr.GetExpressionClass() == ExpressionClass::COLUMN_REF) {
            auto &names = expr.Cast<ColumnRefExpression>().ColumnNames();
            return names.size() == 1 && StringUtil::StartsWith(names[0].GetIdentifierName(), "__sv");
        }
        bool found = false;
        sv_std_children(expr, [&](const ParsedExpression &child) { found = found || HasOwnColumn(child); });
        return found;
    }

    // A SELECT item keeps its column name (DuckDB's for the item as written)
    // whatever it is rewritten to. Neither grouped nor aggregated, an item
    // over members is a FACTS item.
    void RewriteSelectItem(unique_ptr<ParsedExpression> &item) {
        const Identifier name = item->GetName();
        Replace(item, Clause::SELECT);
        vector<const ColumnRefExpression *> left;
        Leftovers(*item, left);
        if (!left.empty()) {
            if (HasOwnColumn(*item)) {
                Fail("'" + Text(*left[0]) + "' must be passed to an aggregate or appear in GROUP BY");
            }
            auto text = Text(*item);
            item = Column("__svf", Add(facts_, text));
        }
        item->SetAlias(name);
    }

    void RefuseLeftoverInHaving(const ParsedExpression &having) const {
        vector<const ColumnRefExpression *> left;
        Leftovers(having, left);
        if (!left.empty()) {
            Fail("HAVING may refer only to metrics passed to AGG or another aggregate, not '" + Text(*left[0]) +
                 "'");
        }
    }

    // Replace the view with the construct, and the block's grouping with the
    // construct's.
    void Graft() {
        string sql = "SELECT * FROM SEMANTIC_VIEW(" + base_.GetQualifiedName().ToString();
        auto clause = [&](const char *keyword, const vector<string> &items, const char *prefix) {
            for (idx_t i = 0; i < items.size(); i++) {
                sql += (i ? ", " : string(" ") + keyword + " ") + items[i] + " AS " + prefix + std::to_string(i);
            }
        };
        clause("DIMENSIONS", dimensions_, "__svd");
        clause("METRICS", metrics_, "__svm");
        clause("FACTS", facts_, "__svf");
        if (select_.where_clause) {
            sql += " WHERE " + select_.where_clause->ToString();
        }
        sql += ")";
        Parser parser(context_);
        parser.ParseQuery(sql);
        auto &inner = parser.statements[0]->Cast<SelectStatement>().node->Cast<SelectNode>();
        auto alias = base_.alias;
        select_.from_table = std::move(inner.from_table);
        select_.from_table->alias = alias;
        select_.where_clause = std::move(select_.having);
        select_.groups.group_expressions.clear();
        select_.groups.grouping_sets.clear();
        select_.aggregate_handling = AggregateHandling::STANDARD_HANDLING;
    }

    ClientContext &context_;
    SelectNode &select_;
    BaseTableRef &base_;
    vector<SvStdMember> members_;
    const std::set<string> &outer_;
    std::set<string> tables_;
    string view_label_;
    string qualifier_;
    vector<string> group_keys_;
    vector<string> dimensions_;
    vector<string> metrics_;
    vector<string> facts_;
};

// Walks a statement for the query blocks that read a semantic view, and
// rewrites each. `changed` records whether any was.
class SvStandardWalker {
public:
    explicit SvStandardWalker(ClientContext &context) : context_(context) {
    }

    bool changed = false;

    void Statement(SQLStatement &statement) {
        switch (statement.type) {
        case StatementType::SELECT_STATEMENT:
            Node(*statement.Cast<SelectStatement>().node, Scope());
            break;
        case StatementType::EXPLAIN_STATEMENT:
            Statement(*statement.Cast<ExplainStatement>().stmt);
            break;
        case StatementType::PREPARE_STATEMENT:
            Statement(*statement.Cast<PrepareStatement>().statement);
            break;
        case StatementType::INSERT_STATEMENT:
            Node(*statement.Cast<InsertStatement>().node, Scope());
            break;
        case StatementType::UPDATE_STATEMENT:
            Node(*statement.Cast<UpdateStatement>().node, Scope());
            break;
        case StatementType::DELETE_STATEMENT:
            Node(*statement.Cast<DeleteStatement>().node, Scope());
            break;
        case StatementType::SET_STATEMENT:
            if (auto *set = dynamic_cast<SetVariableStatement *>(&statement)) {
                if (set->value) {
                    Expr(set->value, Scope());
                }
            }
            break;
        case StatementType::COPY_STATEMENT: {
            auto &info = *statement.Cast<CopyStatement>().info;
            if (info.select_statement) {
                Node(*info.select_statement, Scope());
            }
            break;
        }
        case StatementType::CREATE_STATEMENT: {
            auto &info = *statement.Cast<CreateStatement>().info;
            if (info.type == CatalogType::VIEW_ENTRY && info.Cast<CreateViewInfo>().query) {
                Node(*info.Cast<CreateViewInfo>().query->node, Scope());
            } else if (info.type == CatalogType::TABLE_ENTRY && info.Cast<CreateTableInfo>().query) {
                Node(*info.Cast<CreateTableInfo>().query->node, Scope());
            }
            break;
        }
        default:
            break;
        }
    }

private:
    // What a query block sees from the blocks around it: the CTEs in scope,
    // and the names (aliases, else table names) their FROM clauses bind,
    // which a reference from inside it would be correlated through.
    struct Scope {
        std::set<string> ctes;
        std::set<string> outer;
    };

    static void Aliases(const TableRef &ref, std::set<string> &out) {
        if (ref.type == TableReferenceType::JOIN) {
            Aliases(*ref.Cast<JoinRef>().left, out);
            Aliases(*ref.Cast<JoinRef>().right, out);
            return;
        }
        if (!ref.alias.empty()) {
            out.insert(sv_std_lower(ref.alias));
        } else if (ref.type == TableReferenceType::BASE_TABLE) {
            out.insert(sv_std_lower(ref.Cast<BaseTableRef>().GetQualifiedName().Name()));
        }
    }

    // The subqueries in a node's ORDER BY / LIMIT / OFFSET.
    void Modifiers(QueryNode &node, const Scope &scope) {
        ParsedExpressionIterator::EnumerateQueryNodeModifiers(
            node, [&](unique_ptr<ParsedExpression> &e) { Expr(e, scope); });
    }

    void Node(QueryNode &node, Scope scope) {
        // A CTE is in scope from the next one on: its own body, unless it is
        // recursive, reads the table of its name.
        for (auto &kv : node.cte_map.map) {
            if (kv.second->query_node) {
                Node(*kv.second->query_node, scope);
            }
            scope.ctes.insert(sv_std_lower(kv.first));
        }
        switch (node.type) {
        case QueryNodeType::SET_OPERATION_NODE:
            for (auto &child : node.Cast<SetOperationNode>().children) {
                Node(*child, scope);
            }
            Modifiers(node, scope);
            return;
        case QueryNodeType::RECURSIVE_CTE_NODE: {
            auto &recursive = node.Cast<RecursiveCTENode>();
            scope.ctes.insert(sv_std_lower(recursive.ctename));
            Node(*recursive.left, scope);
            Node(*recursive.right, scope);
            Modifiers(node, scope);
            return;
        }
        case QueryNodeType::INSERT_QUERY_NODE: {
            auto &insert = node.Cast<InsertQueryNode>();
            if (insert.select_statement) {
                Node(*insert.select_statement->node, scope);
            }
            return;
        }
        case QueryNodeType::UPDATE_QUERY_NODE: {
            auto &update = node.Cast<UpdateQueryNode>();
            Scope inner = scope;
            if (update.table) {
                Aliases(*update.table, inner.outer);
            }
            if (update.from_table) {
                Ref(update.from_table, scope);
                Aliases(*update.from_table, inner.outer);
            }
            if (update.set_info) {
                for (auto &e : update.set_info->expressions) {
                    Expr(e, inner);
                }
                if (update.set_info->condition) {
                    Expr(update.set_info->condition, inner);
                }
            }
            for (auto &e : update.returning_list) {
                Expr(e, inner);
            }
            return;
        }
        case QueryNodeType::DELETE_QUERY_NODE: {
            auto &del = node.Cast<DeleteQueryNode>();
            Scope inner = scope;
            if (del.table) {
                Aliases(*del.table, inner.outer);
            }
            for (auto &ref : del.using_clauses) {
                Ref(ref, scope);
                Aliases(*ref, inner.outer);
            }
            if (del.condition) {
                Expr(del.condition, inner);
            }
            for (auto &e : del.returning_list) {
                Expr(e, inner);
            }
            return;
        }
        case QueryNodeType::SELECT_NODE:
            break;
        default:
            return;
        }
        auto &select = node.Cast<SelectNode>();
        // What this block's expressions' subqueries see: its own FROM too.
        Scope inner = scope;
        if (select.from_table) {
            Aliases(*select.from_table, inner.outer);
        }
        // Subqueries first: one in the WHERE of a block that reads a semantic
        // view moves into the construct as text, and is planned there as
        // written.
        auto visit = [&](unique_ptr<ParsedExpression> &e) { Expr(e, inner); };
        for (auto &item : select.select_list) {
            visit(item);
        }
        for (auto &group : select.groups.group_expressions) {
            visit(group);
        }
        for (auto *clause : {&select.where_clause, &select.having, &select.qualify}) {
            if (*clause) {
                visit(*clause);
            }
        }
        Modifiers(node, inner);
        if (!select.from_table) {
            return;
        }
        if (select.from_table->type == TableReferenceType::BASE_TABLE) {
            auto &base = select.from_table->Cast<BaseTableRef>();
            vector<SvStdMember> members;
            if (sv_std_lookup_view(context_, base, scope.ctes, members)) {
                SvStandardRewrite(context_, select, base, std::move(members), scope.outer).Run();
                changed = true;
                return;
            }
        }
        // A subquery in FROM sees the blocks around this one, not its own
        // alias (Ref adds the tables to its left, which DuckDB lets it read).
        Ref(select.from_table, scope);
    }

    // A name no table or CTE has may be one a replacement scan resolves to a
    // subquery (a Python relation); if that subquery reads a semantic view, it
    // goes in the name's place, rewritten.
    void Replace(unique_ptr<TableRef> &ref, const Scope &scope) {
        auto &base = ref->Cast<BaseTableRef>();
        auto &qn = base.GetQualifiedName();
        if (qn.Path().size() == 1 && scope.ctes.count(sv_std_lower(qn.Name()))) {
            return;
        }
        try {
            EntryLookupInfo lookup(CatalogType::TABLE_ENTRY, qn);
            if (Catalog::GetEntry(context_, lookup, OnEntryNotFound::RETURN_NULL)) {
                return;
            }
        } catch (std::exception &) {
        }
        auto replacement = sv_std_replacement(context_, base);
        if (!replacement || replacement->type != TableReferenceType::SUBQUERY) {
            return;
        }
        auto &subquery = replacement->Cast<SubqueryRef>();
        const bool before = changed;
        changed = false;
        Node(*subquery.subquery->node, Scope());
        if (changed) {
            replacement->alias = base.alias.empty() ? qn.Name() : base.alias;
            subquery.column_name_alias = base.column_name_alias;
            ref = std::move(replacement);
        }
        changed = changed || before;
    }

    void Expr(unique_ptr<ParsedExpression> &expr, const Scope &scope) {
        if (expr->GetExpressionClass() == ExpressionClass::SUBQUERY) {
            auto &sub = expr->Cast<SubqueryExpression>().SubqueryMutable();
            if (sub && sub->node) {
                // A subquery's column is named by its text, which the rewrite
                // changes: keep the text as written.
                const bool before = changed;
                const string text = expr->ToString();
                changed = false;
                Node(*sub->node, scope);
                if (changed && !expr->HasAlias()) {
                    expr->SetAlias(Identifier(text));
                }
                changed = changed || before;
            }
        }
        ParsedExpressionIterator::EnumerateChildren(
            *expr, [&](unique_ptr<ParsedExpression> &child) { Expr(child, scope); });
    }

    // A FROM other than a lone table: a semantic view in it is joined,
    // pivoted or listed with other tables, which the query form refuses.
    // `lone` is a block's whole FROM, which Node has already looked up.
    void Ref(unique_ptr<TableRef> &ref, const Scope &scope, bool lone = true) {
        switch (ref->type) {
        case TableReferenceType::SUBQUERY:
            Node(*ref->Cast<SubqueryRef>().subquery->node, scope);
            break;
        case TableReferenceType::JOIN: {
            auto &join = ref->Cast<JoinRef>();
            Ref(join.left, scope, false);
            // DuckDB lets a subquery on the right read the tables on the
            // left (an implicit LATERAL); the condition reads both.
            Scope right = scope;
            Aliases(*join.left, right.outer);
            Ref(join.right, right, false);
            if (join.condition) {
                Scope both = right;
                Aliases(*join.right, both.outer);
                Expr(join.condition, both);
            }
            break;
        }
        case TableReferenceType::PIVOT:
            Ref(ref->Cast<PivotRef>().source, scope, false);
            break;
        case TableReferenceType::SHOW_REF:
            // DESCRIBE / SUMMARIZE of a query.
            if (ref->Cast<ShowRef>().query) {
                Node(*ref->Cast<ShowRef>().query, scope);
            }
            break;
        case TableReferenceType::BASE_TABLE: {
            vector<SvStdMember> members;
            if (!lone && sv_std_lookup_view(context_, ref->Cast<BaseTableRef>(), scope.ctes, members)) {
                throw BinderException("semantic view '%s' cannot be joined, pivoted or read alongside other "
                                      "tables; query it on its own (in a subquery or CTE) and join the result",
                                      ref->Cast<BaseTableRef>().GetQualifiedName().Name().GetIdentifierName());
            }
            Replace(ref, scope);
            break;
        }
        default:
            break;
        }
    }

    ClientContext &context_;
};

// The planner's fallback after a bind error (see the top of this section).
// A plan-less BoundStatement declines, and the planner rethrows the error.
static BoundStatement sv_standard_query_bind(ClientContext &context, Binder &binder, OperatorExtensionInfo *,
                                            SQLStatement &statement) {
    if (statement.query.empty()) {
        return BoundStatement();
    }
    Parser parser(context);
    try {
        parser.ParseQuery(statement.query);
    } catch (std::exception &) {
        return BoundStatement();
    }
    if (parser.statements.size() != 1) {
        return BoundStatement();
    }
    // PREPARE plans its statement with a planner of its own, whose statement
    // keeps the PREPARE's text.
    SQLStatement *fresh = parser.statements[0].get();
    if (fresh->type == StatementType::PREPARE_STATEMENT && statement.type != StatementType::PREPARE_STATEMENT) {
        fresh = fresh->Cast<PrepareStatement>().statement.get();
    }
    if (!fresh || fresh->type != statement.type) {
        return BoundStatement();
    }
    SvStandardWalker walker(context);
    walker.Statement(*fresh);
    if (!walker.changed) {
        return BoundStatement();
    }
    return Binder::CreateBinder(context, &binder)->Bind(*fresh);
}

class SvStandardQuery final : public OperatorExtension {
public:
    SvStandardQuery() {
        Bind = sv_standard_query_bind;
    }
    std::string GetName() override {
        return "semantic_views_standard_query";
    }
    unique_ptr<LogicalExtensionOperator> Deserialize(Deserializer &) override {
        throw InternalException("semantic_views_standard_query plans no operator of its own");
    }
};

class SemanticViewsGrammarActivation final : public ExtensionCallback {
public:
    void OnConnectionOpened(ClientContext &context) override {
        sv_grammar_activate(context);
    }
};

static constexpr const char *SV_GRAMMAR_PENDING_KEY = "semantic_views_grammar_pending";

// Activates the grammar on a connection that was open when the extension
// loaded, the next time that connection begins or ends a query: both run on
// its own thread, under its own lock (TECH-DEBT #103). The connection that ran
// LOAD gets it when the LOAD ends, so the next statement, in the same query
// string or a later one, parses with the grammar. Any other connection gets it
// when its running query ends, or when its next query begins; DuckDB parses a
// statement before the query begins, and a statement that fails to parse
// begins none, so such a connection parses without the grammar until it runs a
// statement that parses without it (TECH-DEBT #108).
class SemanticViewsGrammarPending final : public ClientContextState {
public:
    void QueryBegin(ClientContext &context) override {
        Activate(context);
    }
    void QueryEnd(ClientContext &context) override {
        Activate(context);
    }

private:
    static void Activate(ClientContext &context) {
        try {
            sv_grammar_activate(context);
        } catch (...) {
            // Leave the state registered: the next query tries again.
            return;
        }
        context.registered_state->Remove(SV_GRAMMAR_PENDING_KEY);
    }
};

// Register the grammar once per database, and activate it on every connection:
// those opened later through the ExtensionCallback, and those open now through
// SemanticViewsGrammarPending. The registry each connection keeps its states in
// is synchronised, so registering one from the loading thread is safe, where
// changing the connection's settings from here is not. The standard-SQL
// query form's binder fallback (SvStandardQuery) is registered with them.
static void sv_register_grammar(DatabaseInstance &db) {
    auto &config = DBConfig::GetConfig(db);
    if (!config.GetCallbackManager().FindGrammarExtension(SV_GRAMMAR_NAME)) {
        GrammarExtension::Register(db, make_shared_ptr<SemanticViewsGrammar>(sv_grammar_changes()));
        ExtensionCallback::Register(config, make_shared_ptr<SemanticViewsGrammarActivation>());
        OperatorExtension::Register(config, make_shared_ptr<SvStandardQuery>());
    }
    for (auto &context : db.GetConnectionManager().GetConnectionList()) {
        context->registered_state->GetOrCreate<SemanticViewsGrammarPending>(SV_GRAMMAR_PENDING_KEY);
    }
}

// ---------------------------------------------------------------------------
// sv_register_grammar_hooks -- called from Rust after C API init
// ---------------------------------------------------------------------------
// Registers the `__sv_compute_create_from_yaml` helper table function, then
// the grammar productions (sv_register_grammar), on the database behind the
// C API handle. A failure is written to `error_buf`, as the read-side
// dispatchers report theirs.
extern "C" {
    bool sv_register_grammar_hooks(
        duckdb_database db_handle,
        char *error_buf, size_t error_buf_len) {
        try {
            // duckdb_database -> internal_ptr -> DatabaseWrapper ->
            //   shared_ptr<DuckDB> -> shared_ptr<DatabaseInstance>
            auto *wrapper = reinterpret_cast<duckdb::DatabaseWrapper *>(
                db_handle->internal_ptr);
            auto &db = *wrapper->database->instance;

            // The helper TF goes first: `CREATE SEMANTIC VIEW ... FROM YAML
            // FILE` lowers to a call to it, so the grammar must not be live
            // without it (LIFE-2). Its bind reads the file through DuckDB's
            // FileSystem with the caller's ClientContext.
            {
                LogicalType arg_types[] = {
                    LogicalType::VARCHAR,  // file_path
                    LogicalType::VARCHAR,  // view_name
                    LogicalType::VARCHAR,  // comment (empty = none)
                    LogicalType::VARCHAR,  // schema (empty = current)
                    LogicalType::BOOLEAN,  // skip_if_present
                };
                char helper_err[1024];
                std::memset(helper_err, 0, sizeof(helper_err));
                if (!sv_register_table_function(
                        db_handle,
                        "__sv_compute_create_from_yaml",
                        arg_types, 5,
                        sv_create_from_yaml_bind,
                        sv_create_from_yaml_function,
                        sv_create_from_yaml_init_local,
                        helper_err, sizeof(helper_err))) {
                    if (error_buf != nullptr && error_buf_len > 0) {
                        snprintf(error_buf, error_buf_len,
                            "sv_register_grammar_hooks: failed to register "
                            "__sv_compute_create_from_yaml helper TF: %s",
                            helper_err);
                    }
                    return false;
                }
            }

            // `CREATE SEMANTIC VIEW ... AS` and `... FROM YAML` lower to a
            // read of this one, which checks every member binds.
            {
                LogicalType arg_types[] = {
                    LogicalType::VARCHAR,  // definition
                    LogicalType::VARCHAR,  // view_name
                    LogicalType::VARCHAR,  // schema (empty = current)
                    LogicalType::BOOLEAN,  // skip_if_present
                };
                char helper_err[1024];
                std::memset(helper_err, 0, sizeof(helper_err));
                if (!sv_register_table_function(
                        db_handle,
                        "__sv_check_create",
                        arg_types, 4,
                        sv_check_create_bind,
                        sv_create_from_yaml_function,
                        sv_create_from_yaml_init_local,
                        helper_err, sizeof(helper_err))) {
                    if (error_buf != nullptr && error_buf_len > 0) {
                        snprintf(error_buf, error_buf_len,
                            "sv_register_grammar_hooks: failed to register "
                            "__sv_check_create helper TF: %s",
                            helper_err);
                    }
                    return false;
                }
            }

            sv_register_grammar(db);
            return true;
        } catch (const std::exception &e) {
            if (error_buf != nullptr && error_buf_len > 0) {
                snprintf(error_buf, error_buf_len,
                    "sv_register_grammar_hooks failed: %s", e.what());
            }
            return false;
        }
    }
}
