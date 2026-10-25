// C++ helper declarations for the DuckDB semantic_views extension.
//
// Phase 65 (v0.10.0) Plan 04 (A2 resolution): introduces a reusable C-callable
// `sv_register_table_function` shim that wraps the C++ Catalog API table-function
// registration pattern proven by `65-READ-PATH-SPIKE.md`. Bind callbacks
// registered via this path receive a native `ClientContext &` (not the
// duckdb-rs `BindInfo` wrapper which marshals `ClientContext` away), so they
// can read the catalog in the caller's own context and transaction (the
// `sv_caller_*` entry points, TECH-DEBT #19) without any connection, long-lived
// or per-call.
//
// This header is consumed both by C++ translation units inside the shim
// (e.g. `shim.cpp` itself, future helper-TF translation units) and by Rust
// via the existing `extern "C"` declarations in `src/lib.rs` (Plan 05 will
// add Rust-side registration calls).
//
// IMPORTANT: Keep the signatures here in sync with the `extern "C"` block in
// `cpp/src/shim.cpp`. Mismatches surface as undefined symbols at link time.

#pragma once

#include "parser_extension_compat.hpp"

extern "C" {

// Registers the `__sv_compute_create_from_yaml` helper TF and the extension's
// grammar productions on the database behind `db_handle`, then turns the
// grammar on for every connection. Returns false with a diagnostic in
// `error_buf` on failure, like the read-side dispatchers.
bool sv_register_grammar_hooks(
    duckdb_database db_handle,
    char *error_buf, size_t error_buf_len);

// Phase 65 Plan 04 (A2 resolution) — register a table function via the C++
// Catalog API so its bind callback receives a native `ClientContext &`.
//
// Phase 65.1 Plan 02a (WR-02 D-08/D-09 + CR-02 D-05) — trailing
// `(char *error_buf, size_t error_buf_len)` pair surfaces registration
// failures via the same ABI-stable channel the 17 read-side dispatchers
// use. The caller MUST provide a writable buffer of
// `error_buf_len` bytes; the helper truncates and NUL-terminates within
// the cap. D-09: no stderr write occurs — `error_buf` is the only
// channel (ADBC/JDBC/Python callers may have redirected stderr).
//
// Parameters:
//   db_handle      — the DuckDB C API database handle (unwrapped internally
//                    to a `DatabaseInstance &` for the registration call).
//   name           — UTF-8 NUL-terminated table-function name to register.
//   arg_types      — pointer to `arg_count` `duckdb::LogicalType` values.
//                    May be null when `arg_count == 0`. The values are
//                    copied into the constructed `TableFunction`; the
//                    caller can free the underlying storage after the call
//                    returns.
//   arg_count      — number of entries in `arg_types`.
//   bind_cb        — `unique_ptr<FunctionData>(ClientContext &, ...)`
//                    callback invoked during binding; must be non-null.
//   exec_cb        — `void(ClientContext &, TableFunctionInput &, DataChunk &)`
//                    callback invoked during execution; must be non-null.
//   init_cb        — `unique_ptr<LocalTableFunctionState>(ExecutionContext &, ...)`
//                    callback invoked once per execution to construct local
//                    state. Phase 65.1 D-05 (CR-02): now MANDATORY — null
//                    is rejected at registration time. Forces every TF
//                    callback onto the single-shot-via-local-state path
//                    so the double-emit / unbounded-loop hazard cannot
//                    recur.
//   error_buf      — caller-allocated writable buffer of `error_buf_len`
//                    bytes. On failure, the helper writes a
//                    NUL-terminated diagnostic via snprintf (truncates
//                    within the cap). Pass a non-null buffer; passing
//                    null + 0 is tolerated (the diagnostic is dropped but
//                    the call still returns false on error).
//   error_buf_len  — capacity of `error_buf` in bytes.
//
// Returns true on success. On failure (registration throws, db handle
// invalid, any callback null), writes the underlying diagnostic into
// `error_buf` and returns false. Uses `OnCreateConflict::ALTER_ON_CONFLICT`
// internally so extension reload does not trip on a duplicate name.
//
// Thread safety: the registration call uses `Catalog::GetSystemCatalog(db)`
// + `CatalogTransaction::GetSystemTransaction(db)`, the same pattern the
// read-path spike (`65-READ-PATH-SPIKE.md`) validated. Safe to call from
// extension init.
bool sv_register_table_function(
    duckdb_database db_handle,
    const char *name,
    const duckdb::LogicalType *arg_types,
    size_t arg_count,
    duckdb::table_function_bind_t bind_cb,
    duckdb::table_function_t exec_cb,
    duckdb::table_function_init_local_t init_cb,
    char *error_buf, size_t error_buf_len);

// Phase 65 Plan 05 (Task 2 Step A) — register a scalar function via the C++
// Catalog API, the scalar sibling of sv_register_table_function. Used by
// Task 4 / Wave 4 (get_ddl + read_yaml_from_semantic_view migrations).
//
// Phase 65.1 Plan 02a (WR-02 D-08/D-09) — trailing
// `(char *error_buf, size_t error_buf_len)` pair surfaces registration
// failures via the same ABI-stable channel as the table-function helper
// above. The caller MUST provide a writable buffer of
// `error_buf_len` bytes; the helper truncates and NUL-terminates within
// the cap. D-09: no stderr write occurs — `error_buf` is the only
// channel. NOTE: scalar functions have no `init_local` concept, so D-05
// (null-init refusal) does NOT apply here.
//
// Parameters:
//   db_handle      — DuckDB C API database handle (unwrapped to
//                    `DatabaseInstance &` internally).
//   name           — UTF-8 NUL-terminated scalar-function name.
//   arg_types      — pointer to `arg_count` `duckdb::LogicalType` values;
//                    may be null when `arg_count == 0`.
//   arg_count      — number of entries in `arg_types`.
//   return_type    — `duckdb::LogicalType` of the scalar's return value.
//   exec_cb        — `void(DataChunk &args, ExpressionState &state, Vector &result)`
//                    callback invoked at execution time; must be non-null.
//   error_buf      — caller-allocated writable buffer of `error_buf_len`
//                    bytes. On failure, the helper writes a
//                    NUL-terminated diagnostic via snprintf.
//   error_buf_len  — capacity of `error_buf` in bytes.
//
// Returns true on success. Uses `OnCreateConflict::ALTER_ON_CONFLICT` so
// extension reload does not trip on a duplicate name. On failure writes
// the underlying diagnostic into `error_buf` and returns false.
bool sv_register_scalar_function(
    duckdb_database db_handle,
    const char *name,
    const duckdb::LogicalType *arg_types,
    size_t arg_count,
    duckdb::LogicalType return_type,
    duckdb::scalar_function_t exec_cb,
    char *error_buf, size_t error_buf_len);

// Phase 65 Plan 05 (Task 1 / Wave 0 bridge spike) — register the read-side
// `list_semantic_views()` table function via the C++ Catalog API. The bind
// callback sets the caller's context for the thread (`SvCallerScope`) and
// calls the Rust dispatcher (`sv_list_semantic_views_bind_rust`), which reads
// the catalog in the caller's transaction through `sv_caller_definitions`
// (TECH-DEBT #19). Every other read-side function works the same way.
//
// This entry point is the registration helper; the bind/function/init
// callbacks themselves live inside `shim.cpp` (file-static) and are
// passed into `sv_register_table_function` by name.
//
// Phase 65.1 Plan 02a: trailing `(error_buf, error_buf_len)` pair forwards
// the caller's buffer straight through to the inner
// `sv_register_table_function` so the failure message names this wrapper's
// TF (e.g. `sv_register_table_function('list_semantic_views') failed: ...`).
bool sv_register_list_semantic_views(duckdb_database db_handle,
                                     char *error_buf, size_t error_buf_len);

// Phase 65 Plan 05 Task 2 (Wave 1) — register the migrated
// `list_terse_semantic_views()` table function via the C++ Catalog API.
// 5-column subset of list_semantic_views; same bridge mechanism.
bool sv_register_list_terse_semantic_views(duckdb_database db_handle,
                                           char *error_buf, size_t error_buf_len);

// Phase 65 Plan 05 Task 2 (Wave 1) — register the migrated zero-arg "_all"
// TFs via the C++ Catalog API. All emit homogeneous VARCHAR rows; column
// counts and names match the legacy duckdb-rs registrations.
bool sv_register_show_semantic_dimensions_all(duckdb_database db_handle,
                                              char *error_buf, size_t error_buf_len);
bool sv_register_show_semantic_metrics_all(duckdb_database db_handle,
                                           char *error_buf, size_t error_buf_len);
bool sv_register_show_semantic_facts_all(duckdb_database db_handle,
                                         char *error_buf, size_t error_buf_len);
bool sv_register_show_semantic_materializations_all(duckdb_database db_handle,
                                                    char *error_buf, size_t error_buf_len);

// Phase 65 Plan 05 Task 3 (Wave 2) — register the migrated single-arg /
// two-arg TFs via the C++ Catalog API. All take a VARCHAR view-name
// argument (the dimensions_for_metric variant additionally takes a metric
// name). The dimensions_for_metric variant returns 3 VARCHAR + 1 BOOLEAN;
// the rest return homogeneous VARCHAR rows.
bool sv_register_show_columns_in_semantic_view(duckdb_database db_handle,
                                               char *error_buf, size_t error_buf_len);
bool sv_register_describe_semantic_view(duckdb_database db_handle,
                                        char *error_buf, size_t error_buf_len);
bool sv_register_show_semantic_dimensions(duckdb_database db_handle,
                                          char *error_buf, size_t error_buf_len);
bool sv_register_show_semantic_metrics(duckdb_database db_handle,
                                       char *error_buf, size_t error_buf_len);
bool sv_register_show_semantic_facts(duckdb_database db_handle,
                                     char *error_buf, size_t error_buf_len);
bool sv_register_show_semantic_materializations(duckdb_database db_handle,
                                                char *error_buf, size_t error_buf_len);
bool sv_register_show_semantic_dimensions_for_metric(duckdb_database db_handle,
                                                     char *error_buf, size_t error_buf_len);

// Phase 65 Plan 05 Task 4 (Wave 3) — register the migrated read-side
// scalars via the C++ Catalog API. The exec callbacks set the caller's
// context (`SvCallerScope` on `state.GetContext()`) and call the matching
// Rust dispatcher (`sv_get_ddl_exec_rust`,
// `sv_read_yaml_from_semantic_view_exec_rust`), which looks the view up in
// the caller's transaction, as the table functions do. See
// `cpp/src/shim.cpp` per-callback comment blocks.
//
// `get_ddl(object_type VARCHAR, name VARCHAR
//          [, use_fully_qualified_names BOOLEAN]) -> VARCHAR` — 2 or 3 args,
//   registered as one `ScalarFunctionSet` (TECH-DEBT #25).
// `read_yaml_from_semantic_view(name VARCHAR) -> VARCHAR` — 1 arg.
bool sv_register_get_ddl(duckdb_database db_handle,
                         char *error_buf, size_t error_buf_len);
bool sv_register_read_yaml_from_semantic_view(duckdb_database db_handle,
                                              char *error_buf, size_t error_buf_len);

// Phase 65 Plan 05 Task 5 (Wave 5) — register the migrated
// `explain_semantic_view(view_name VARCHAR, dimensions := LIST(VARCHAR),
// metrics := LIST(VARCHAR), facts := LIST(VARCHAR))` table function via
// the C++ Catalog API. The bind sets the caller's context (`SvCallerScope`)
// and calls `sv_explain_semantic_view_bind_rust`, which plans the expansion
// in that context (`sv_caller_explain`).
// Registers through the shared spec-based core (C-7,
// `cpp/src/shim.cpp::sv_register_table_function_core`) with the named
// LIST(VARCHAR) parameters (`dimensions`/`metrics`/`facts`, from the single
// `sv_semantic_named_params`) and an init_local state constructor. Failures
// surface via the forwarded `error_buf` (D-08/D-09), never stderr.
bool sv_register_explain_semantic_view(duckdb_database db_handle,
                                       char *error_buf, size_t error_buf_len);

// Register the `semantic_view(view_name VARCHAR, dimensions := LIST(VARCHAR),
// metrics := LIST(VARCHAR), facts := LIST(VARCHAR), ...)` table function via
// the C++ Catalog API. It is a `bind_replace` function (#42): the bind sets
// the caller's context (`SvCallerScope`), calls `sv_semantic_view_bind_rust`
// for the catalog lookup (in the caller's transaction, TECH-DEBT #19) and the
// expansion, and returns the expanded query as a subquery that DuckDB plans
// in the caller's statement. No connection is opened.
//
// Registers through the shared spec-based core (C-7,
// `cpp/src/shim.cpp::sv_register_table_function_core`) with the shared named
// parameters and a `bind_replace` callback. Failures surface via `error_buf`
// (D-08/D-09), never stderr.
bool sv_register_semantic_view(duckdb_database db_handle,
                               char *error_buf, size_t error_buf_len);

} // extern "C"
