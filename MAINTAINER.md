# Maintainer Guide

Everything a contributor needs to build, test, fuzz, and publish the DuckDB Semantic Views extension.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
- [Architecture Overview](#architecture-overview)
- [Building](#building)
- [Testing](#testing)
- [Loading the Extension](#loading-the-extension)
- [Updating the DuckDB Version Pin](#updating-the-duckdb-version-pin)
- [Multi-Version Branching Strategy](#multi-version-branching-strategy)
- [Fuzzing](#fuzzing)
- [Publishing to Community Extension Registry](#publishing-to-community-extension-registry)
- [Worked Examples](#worked-examples)
- [Troubleshooting](#troubleshooting)
- [CI Workflows](#ci-workflows)

---

## Prerequisites

Install these before anything else.

| Tool | Install | What It Does |
|------|---------|--------------|
| `rustup` | [rustup.rs](https://rustup.rs/) | Manages Rust toolchain versions (like `pyenv` for Python) |
| Rust stable | `rustup install stable` | The Rust compiler and package manager (`cargo`) |
| `just` | `cargo install just` or `brew install just` | Command runner (like `make` but simpler -- run `just` to see all commands) |
| Python 3 | You likely already have this | Needed for the SQLLogicTest runner and DuckLake integration tests |
| Git | With submodule support | The repo uses Git submodules for DuckDB CI tooling |
| CMake (and optionally Ninja) | `brew install cmake ninja` / `apt install cmake ninja-build` | Builds the DuckDB engine the C++ shim links against (once per engine version; see [Which DuckDB the C++ shim compiles against](#which-duckdb-the-c-shim-compiles-against)) |

You do **not** need to install DuckDB separately. The build system installs the matching `duckdb` Python package for the test runner (`make configure`) and builds the engine the extension links against from its PyPI sdist on the first `just build`; unit tests compile DuckDB from source via the `bundled` feature.

## Quick Start

From zero to a working extension in five commands:

```bash
git clone --recurse-submodules https://github.com/s-layer/duckdb-semantic-views.git
cd duckdb-semantic-views
just setup          # installs dev tools (cargo-nextest, cargo-deny, cargo-llvm-cov), downloads DuckDB test binary
just build          # builds the debug extension (.duckdb_extension file)
just test-rust      # runs Rust unit tests (~30 seconds)
just test-sql       # runs SQL logic tests via DuckDB's test runner
```

After `just build`, the extension binary is at `build/debug/semantic_views.duckdb_extension`.

## Architecture Overview

### Source Tree

```
src/
├── lib.rs                     # Extension entrypoint — registers table functions + the grammar productions
├── model.rs                   # Core data types: SemanticViewDefinition, Dimension, Metric, Fact, Relationship…
├── errors.rs                  # Typed error surface (ParseError + optional caret) for the CREATE/parse boundary
├── ident.rs                   # Identifier grammar: quoting, case-folding, qualified-name splitting
├── load_guard.rs              # Load-time check that the host DuckDB is the release we were built for
├── expr_tokens.rs             # Quote/literal-aware tokenizer for stored SQL expressions (reference find/inline)
├── sql_lit.rs                 # SqlLit newtype — makes "forgot to escape a string literal" a compile error
├── util.rs                    # Shared lexical helpers (is_ident_byte, blank_sql_comments, dollar-tag grammar, QuoteState)
├── ffi_util.rs                # FFI seam helpers: buffer handoff, UTF-8-safe error truncation
├── render_ddl.rs              # SemanticViewDefinition → CREATE SEMANTIC VIEW text (GET_DDL)
├── render_yaml.rs             # SemanticViewDefinition → YAML
│
├── body_parser/               # Tokenizer + clause-body parser for the CREATE body (pure, always compiled)
│   ├── lexer.rs cursor.rs scan.rs clause_bounds.rs   #   token layer, cursor, clause bounds
│   ├── tables.rs relationships.rs metrics.rs entries.rs
│   ├── annotations.rs window.rs materializations.rs
│   ├── metadata.rs            #   Cortex Analyst clauses: SAMPLE_VALUES, CORTEX SEARCH, MAX_STALENESS, AI_*
│   ├── cross_refs.rs          #   semantic cross-reference checks shared by the DDL and YAML paths
│   └── mod.rs
├── parse/                     # Text-level planning behind the grammar productions (write side)
│   ├── detect.rs              #   CREATE-prefix detection
│   ├── rewrite.rs             #   plan_create (CREATE text → RewriteAction), DESCRIBE / SHOW lowering
│   ├── create_body.rs         #   CREATE body (validate_create_body)
│   ├── native_sql.rs          #   INSERT/UPDATE/DELETE emission on _definitions
│   ├── show_clauses.rs        #   SHOW clauses → catalog WHERE / LIMIT suffix, shared messages
│   └── mod.rs
├── grammar/                   # DuckDB 2.0 grammar productions (Phase 3): rule text + transforms to SQL
│   ├── tree.rs                #   decodes the parse subtree the C++ bridge serialises
│   ├── show.rs                #   SHOW / SHOW COLUMNS / DESCRIBE SEMANTIC productions (queries)
│   ├── write.rs               #   DROP / ALTER SEMANTIC VIEW productions (statements; lowered by
│   │                          #   parse::lower_action, returned to DuckDB as one MultiStatement)
│   ├── create.rs              #   CREATE [OR REPLACE] SEMANTIC VIEW: the body is taken by DuckDB's
│   │                          #   any-token matcher (ANY_TOKEN rules), rebuilt at its query offsets
│   │                          #   and planned by parse::plan_create (TECH-DEBT #100)
│   ├── query.rs               #   SEMANTIC_VIEW(...) in FROM (a table reference, an InnerTableRef
│   │                          #   alternative), lowered to a semantic_view() call
│   ├── ffi.rs                 #   FFI entry points: sv_grammar_changes_rust / sv_grammar_transform_rust
│   └── mod.rs                 #   the rule list registered with DuckDB, and transform dispatch
├── graph/                     # Relationship graph: cardinality, join tree, toposort, derived-metric DAG
│   ├── relationship.rs cardinality.rs join_tree.rs toposort.rs
│   ├── derived_metrics.rs facts.rs using.rs names.rs
│   ├── member_exprs.rs        #   CREATE-time content checks: no aggregate in a dimension or fact, no
│   │                          #   prepared-statement parameter in any member (TECH-DEBT #95)
│   ├── member_refs.rs         #   CREATE-time scoping: a member expression may name a column of
│   │                          #   its own table or a declared fact/metric, never a raw column of
│   │                          #   another table (TECH-DEBT #52 / PAR-3)
│   ├── same_table_metrics.rs  #   a metric written over metrics on its own table (Phase 6.3): CREATE
│   │                          #   checks, and the inlining `expand()` applies before anything else
│   └── mod.rs
├── expand/                    # Query expansion: definition + QueryRequest → SQL (pure, always compiled)
│   ├── mod.rs resolution.rs join_resolver.rs sql_gen.rs select_spec.rs types.rs
│   ├── facts.rs fan_trap.rs semi_additive.rs window.rs wildcard.rs role_playing.rs materialization.rs
│   ├── where_clause.rs        #   pre-aggregation `where_clause :=` predicate: resolves the members it
│   │                          #   names and the tables its FACT CHAINS reach (`WhereMember::fact_tables`),
│   │                          #   which both fan-trap and role-playing fences must walk (EXP-27 / EXP-31)
│   ├── per_grain.rs           #   multi-grain queries: one CTE per metric grain, joined (Snowflake parity);
│   │                          #   also `window_cte_anchor`, which picks the grain window.rs anchors __sv_agg at
│   └── tests_*.rs             #   behaviour-named extracted test modules
├── catalog/                   # Reads/writes of semantic_layer._definitions
│   ├── mod.rs                 #   CatalogReader (the catalog rows as the caller's transaction sees them)
│   └── writes.rs              #   write-side race guards
├── ddl/                       # DDL execution + read-side table functions (only compiled under --features extension)
│   ├── define.rs              #   CREATE-time enrichment (cardinality inference, graph validation)
│   ├── describe.rs get_ddl.rs list.rs
│   ├── show_columns.rs show_entities.rs show_dims_for_metric.rs show_materializations.rs
│   ├── read_ffi.rs read_yaml.rs alter_helpers_ffi.rs   #   FFI seam (dispatcher scaffold, caller-context reads)
│   └── mod.rs
└── query/                     # Query interface
    ├── table_function.rs      #   semantic_view() — reads the definition, expands it and hands the SQL to the
    │                          #   C++ bind_replace, which DuckDB plans in the caller's query (extension-only)
    ├── explain.rs             #   explain_semantic_view() — expanded SQL + EXPLAIN plan (extension-only)
    ├── items.rs               #   Request items `<member> [[AS] alias]`, column_order and the output
    │                          #   column layout they ask for (always compiled)
    ├── expr_items.rs          #   Expression items (TECH-DEBT #76) lowered to members of the query's own
    │                          #   definition (always compiled)
    ├── wire.rs                #   Pure wire-format/SQL-shape helpers (always compiled + unit-tested)
    ├── raw_sql.rs             #   Raw C-API query helpers: DESCRIBE names and types, VARCHAR reads (always compiled)
    ├── member_types.rs        #   Member type probe for SHOW/DESCRIBE `data_type` (TECH-DEBT #51), also
    │                          #   CREATE's member bind (`first_unbound_member`; always compiled)
    ├── standard.rs            #   Members a standard-SQL query over a view by name is classified by (Phase 5);
    │                          #   the listing is always compiled, its FFI entrypoint extension-only
    ├── error.rs               #   Query-specific error types (extension-only)
    └── mod.rs

fuzz/                          # Fuzz testing (independent Cargo crate; depends on semantic_views + "arbitrary")
├── fuzz_targets/              #   Nine targets — see the Fuzzing section for what each covers
│   ├── fuzz_json_parse.rs fuzz_yaml_parse.rs fuzz_ddl_parse.rs fuzz_keyword_body.rs
│   ├── fuzz_sql_expand.rs fuzz_query_names.rs fuzz_render_roundtrip.rs fuzz_create_text.rs
│   └── fuzz_where_predicate.rs
├── seeds/                     #   Committed seed inputs (per target)
└── corpus/                    #   Fuzzer-discovered inputs (gitignored)

examples/                      # Runnable Python demos — every one runs in CI via `just test-examples`

scripts/                       # Build/test helpers, among them:
├── run_examples.py            #   `just test-examples` runner (PASS/FAIL/SKIPPED/XFAIL per example)
├── run_sqllogictest.py        #   strict sqllogictest wrapper — the Makefile's TEST_RUNNER
└── windows_crash_report.py    #   Windows only: re-runs a runner that crashed under cdb for the native stack

test/
├── sql/                       # sqllogictest files (run via test/sql/TEST_LIST — drift is a CI error)
│   └── *.test                 #   e.g. phase4_query.test, phase29_facts.test, phase47_semi_additive.test
└── integration/               # Python integration suites (test_ducklake_ci.py, test_differential.py, …)

.github/workflows/             # CI pipelines — see the CI Workflows section for triggers
├── BuildQuick.yml BuildAll.yml            #   extension build (PR / main); BuildAll also
│                                          #   sqllogictests Windows + macOS-arm64
├── CodeQuality.yml IntegrationChecks.yml  #   lint+coverage / DuckLake+Python+sqllogictest
├── DocsCheck.yml Docs.yml                 #   docs build check (branches) / build+deploy (main)
└── Fuzz.yml DuckDBVersionMonitor.yml PublishExtension.yml
```

### Data Flow

Every semantic-view statement is a production of DuckDB's own PEG grammar: `src/grammar/` adds them to DuckDB 2.0's grammar at load, so DuckDB splits a multi-statement string, finds our statements wherever its own DDL may appear (at the top level, or `SHOW` / `DESCRIBE` as a subquery), and reports a malformed one as its own syntax error. Each matched statement is lowered to native SQL that DuckDB then plans and executes on the caller's connection. There is no parser override, parse stub or `sv_ddl_internal` table-function fallback any more (the last was deleted in the v0.8.0 architectural unification (Phase 59) — see CHANGELOG; the override and stub in the Phase 3 deletion PR).

```
DDL (CREATE, including FROM YAML [FILE] / DROP / ALTER / DESCRIBE / SHOW)
   └── DuckDB's PEG grammar matches an Sv* production (rules from src/grammar/, registered at load
       and activated per connection: see "Grammar activation" below)
   └── C++ grammar bridge (cpp/src/shim.cpp::sv_grammar_lower): serialises the match tree and the
       statement's own tokens (sv_grammar_span)
   └── Rust FFI (src/grammar/ffi.rs::sv_grammar_transform_rust) → grammar::transform
         ├── CREATE: text rebuilt from the tokens → parse::plan_create (body parser, then
         │   INSERT/UPDATE/DELETE on _definitions)
         ├── DROP / ALTER: parse::lower_action (writes, with race-guard SELECT prefix)
         └── SHOW / DESCRIBE: lower_show / lower_name_only_read (SELECT * FROM <read_side_fn>(...))
       An invalid statement is rc=1 with the whole message on the heap and a byte offset, thrown
       as a ParserException with a QueryLocation, which DuckDB renders as `LINE n: … ^`.
   └── C++ Parser::ParseQuery of the lowered SQL (the builtin parser, which runs no extension
       grammar); DuckDB runs the statement(s) on the caller's connection.

semantic_view('shop', dimensions := [...], metrics := [...])
   └── bind_replace table function. Bind reads the definition via catalog::CatalogReader
       (CatalogReader::for_caller, in the caller's transaction; see Catalog Persistence),
       expand/ generates SQL, and DuckDB plans it as a subquery of the caller's query.
       TABLES entries arrive qualified with the stamp, or as temp.main for one CREATE found
       temporary (SemanticViewDefinition::temp_tables). The shim (SvTablePinner) first
       resolves the other names the expansion uses (a member subquery's tables, the tables an
       inline-query table's query reads (Phase 6.4; `expand::table_source` emits the query as a
       FROM subquery), functions,
       macros, table functions, cast types) as DuckDB resolves a view stored in the stamped
       schema, and pins each to what it finds, so a caller's CTE or current schema can't
       stand in for it.
```

`expand/` is pure Rust that converts a `SemanticViewDefinition` plus a `QueryRequest` into a SQL string. DuckDB handles all actual data processing. The generated SQL looks like:

```sql
SELECT
    "o"."region" AS "region",
    SUM("o"."amount") AS "revenue"
FROM "orders" AS "o"
LEFT JOIN "customers" AS "c" ON "o"."customer_id" = "c"."id"
GROUP BY
    "o"."region"
```

#### Function registration and `duckdb_functions()` docs

Every table and scalar function is registered through the C++ Catalog API helpers in `cpp/src/shim.cpp` (`sv_register_table_function_core` and `sv_register_scalar_function_set`). Each one attaches a `FunctionDescription` (description, parameter names, examples, categories) looked up by function name in `sv_function_docs`, so agents and tools can discover the function over SQL. **A function with no entry in that table fails to register**: adding a new function means adding its doc entry in the same change. A function that a SQL statement lowers to (the `SHOW` / `DESCRIBE` family) should say "Backs `<statement>`" and tell the caller to use the statement instead. `test/sql/function_descriptions.test` pins the rendered result and runs every example; extend its function list when you add one.

#### Grammar activation

DuckDB keeps the active grammar extensions per connection (`active_grammar_extensions`, settable only locally), so loading the extension does not by itself change how other connections parse. `sv_register_grammar` (`cpp/src/shim.cpp`) registers the productions once per database, then activates them two ways:

1. **Connections opened later**, from the extension callback's `OnConnectionOpened`, on their own thread.
2. **Connections already open**, the loading one included, through a `SemanticViewsGrammarPending` `ClientContextState` registered on each (that registry is mutex-synchronised). Its `QueryBegin` / `QueryEnd` runs on the connection's own thread, activates the grammar there and removes itself. The loading connection activates at the `QueryEnd` of its `LOAD`, so its next query has the grammar. Nothing writes another connection's `ClientConfig` from the loading thread (TECH-DEBT #103).

DuckDB parses a statement before `QueryBegin`, and a statement that fails to parse begins no query. So any **other** connection that was open at the `LOAD` (and was not running a query then) parses without the grammar until it runs a statement that parses without it, such as `SELECT 1` or `LOAD semantic_views`: every semantic-view statement before that fails as a DuckDB syntax error, retries included. `test_connection_open_before_load` in `test/integration/test_load_extension_twice_idempotent.py` pins this. DuckDB offers no pre-parse hook and no global form of `active_grammar_extensions` that would avoid it (TECH-DEBT #108).

#### CREATE binds every member

The CREATE transform has no `ClientContext`, so the check runs in a table function's bind. `parse::native_sql::emit_native_create_sql` reads the catalog `INSERT`'s row from `__sv_check_create('<definition>', '<view>', '<schema>', <skip_if_present>)` (every CREATE shape, `OR REPLACE` and `IF NOT EXISTS` included), and `FROM YAML FILE`'s helper bind calls the same check once it has the definition. `skip_if_present` is true for every form but `OR REPLACE`: when the view already exists in the schema the CREATE lands in (`ddl::read_ffi::view_present`), the check returns without binding, so `IF NOT EXISTS` stays a no-op and a plain CREATE reaches its "already exists" error. The bind (`sv_check_create_bind` in `cpp/src/shim.cpp`) calls `sv_check_create_members` under `SvCallerScope`, which hands the caller's current database and schema to `ddl::read_ffi::check_create_members`. That stamps them on the definition as CREATE will, clears its materializations (their tables may not exist yet), notes which tables are temporary, and calls `query::member_types::first_unbound_member`. It probes every member through the same `caller_describe` path as `SHOW` / `DESCRIBE` types, so a member binds in the caller's transaction and sees its temporary tables. `PRIVATE` members are probed through a public copy, a role-played member through a single-role copy, and a window metric with its required dimensions. A member is reported only when its own probe ran and failed to bind; the error names it (`semantic view '<v>': <kind> '<name>' does not bind: ...`). `test/sql/create_binds_members.test` pins each refusal and the controls.

#### Standard-SQL queries by view name

`SELECT region, AGG(revenue) FROM sv GROUP BY region` (Phase 5) is handled by `SvStandardQuery`, an `OperatorExtension` that `sv_register_grammar` registers once per database (`cpp/src/shim.cpp`). DuckDB calls an operator extension's `Bind` only after its own binder fails, so a statement that reads no semantic view never reaches it and keeps DuckDB's error. The hook re-parses the statement's own text (unwrapping `PREPARE`), and `SvStandardWalker` walks every query block, CTE and subquery (in every clause, `ORDER BY` / `LIMIT` and `GROUP BY` included, and in `INSERT`, `UPDATE`, `DELETE`, `SET VARIABLE`, `COPY` and `CREATE VIEW` / `TABLE AS`), following Python replacement scans so a relation read by its variable name is walked too. A CTE is in scope from the next one on, as in the binder; a subquery in `FROM` sees the blocks around it but not its own alias. A `FROM` naming a semantic view (and no CTE in scope, table, view or replacement-scan object of that name, which the binder resolved first) becomes a `SvStandardRewrite`: it classifies each name with the member listing from `sv_standard_query_view_rust` (`src/query/standard.rs`), enforces the query form's rules, and grafts in the `SEMANTIC_VIEW(...)` construct, so every check the construct runs (fan traps, role-playing paths, private members, `WHERE` over a metric) applies unchanged. The rewritten statement is bound by a child binder (`Binder::CreateBinder(context, &binder)`), which shares the parameters and table indexes but none of the failed attempt's bindings; if nothing was rewritten the hook declines and the planner rethrows the original error. This is the only hook every bind path goes through: `ClientContext::BindStatement` (Python's `duckdb.sql()` / `con.sql()`, and the sqllogictest runner) skips the `OnPlanningError` rebind hook.

### Feature Flag Split

The crate has two Cargo feature configurations (think of features as build-time toggles, like Python's `extras_require`):

| Feature | When Used | What It Enables |
|---------|-----------|-----------------|
| `default` (`duckdb/bundled`, `duckdb/json`) | `cargo test`, fuzzing | Compiles DuckDB from source into the binary. Enables `Connection::open_in_memory()` for unit tests. `duckdb/json` statically links DuckDB's json extension, which the catalog-migration tests need (`json_merge_patch`); without it DuckDB downloads json at test time, which fails offline. It is a default feature, not a `[dev-dependencies]` feature, because resolver 2 unifies dev-dependency features into `cargo test --no-default-features --features extension` too, where `duckdb/json` would pull in `duckdb/bundled`. |
| `extension` (`duckdb/loadable-extension`, `duckdb/vscalar`) | `just build`, CI builds | Produces a loadable `.duckdb_extension` file. Uses function-pointer stubs instead of bundled DuckDB. |

This split exists because DuckDB loadable extensions cannot be tested as standalone binaries -- the function-pointer stubs are only initialized when DuckDB loads the extension at runtime. The `bundled` feature sidesteps this for unit tests.

The `ddl/` module and the FFI entrypoints in `query/` (`table_function.rs`, `explain.rs`, `error.rs`) are gated behind `#[cfg(feature = "extension")]` -- they are excluded from `cargo test` compilation because they use DuckDB APIs only available in the extension build. `query/wire.rs`, `query/items.rs`, `query/expr_items.rs`, `query/raw_sql.rs`, `query/member_types.rs` and `query/standard.rs` are always compiled (`standard.rs` gates only its FFI entrypoint): `items.rs`, `expr_items.rs` and `standard.rs` are pure, and the others take a raw `duckdb_connection`, so `cargo test` drives them against the bundled DuckDB (`test_helpers::RawDb`).

### Catalog Persistence

Definitions live in `semantic_layer._definitions` (a regular DuckDB table that participates in normal transactional semantics), keyed by `(schema_name, name)` — semantic views are scoped to a schema, so two schemas may each hold a view of the same name. `schema_name` is a column rather than a field inside the JSON `definition` so the key can enforce that; the JSON keeps a matching copy for the SHOW / DESCRIBE listings, written from the same expression and kept in lockstep (see `catalog::writes::create_target_schema_expr`). A catalog in the pre-scoping single-column shape is rebuilt in place on the next `LOAD` by `migrate_definitions_to_schema_scoped` — a rebuild rather than an `ALTER`, because DuckDB cannot retrofit a primary key.

Writes and reads both happen in the caller's transaction:

- **Writes.** The grammar bridge produces `INSERT / UPDATE / DELETE ... RETURNING ...` SQL, and DuckDB runs it on the caller's connection, in whatever transaction the caller has open.
- **Reads** (`semantic_view()`, `explain_semantic_view()`, `describe_*`, `show_*`, `list_*`, `read_yaml_*`, `get_ddl`). The C++ bind or scalar callback names the caller's `ClientContext` for the call (`SvCallerScope`), and the Rust dispatcher reads through `read_ffi::caller_definitions` / `caller_describe` / `caller_explain`, backed by the shim's `sv_caller_*`: a storage-layer scan of `_definitions` with the caller's `DuckTransaction` (its uncommitted rows and deletes included), and binds / plans with the caller's context for member typing and `explain_semantic_view`. None of them runs a statement: `ClientContext::Query` from inside the caller's own bind would self-deadlock on its `context_lock` (TECH-DEBT #19). Reads look the table up in the database new connections start in, whatever the caller has `USE`-d (#26).

The in-memory `CatalogState` HashMap mirror was removed in v0.8.0 (Phase 58).

## Building

### Debug Build

```bash
just build          # builds build/debug/semantic_views.duckdb_extension
```

### Release Build

```bash
just build-release  # builds build/release/semantic_views.duckdb_extension (optimized, stripped)
```

### How the Build Works

- **`Cargo.toml`** (Rust's equivalent of `pyproject.toml`) defines dependencies, features, and lint configuration.
- **`Makefile`** delegates to `cargo build` with the correct feature flags: `--no-default-features --features extension`. This produces a `cdylib` (C-compatible dynamic library) that DuckDB can load.
- **`crate-type = ["cdylib", "lib"]`** in `Cargo.toml` means two output types:
  - `cdylib`: the `.duckdb_extension` shared library file (for DuckDB to load)
  - `lib`: a regular Rust library (for unit tests and fuzz targets to link against)

The Makefile also handles building the DuckDB engine the C++ shim links against, running the SQLLogicTest runner, and packaging the extension with metadata.

Before each `cargo build`, `make ensure_rust_toolchain` (`scripts/ensure_rust_toolchain.py`, guarded by `tests/ensure_rust_toolchain_script.rs`) installs the toolchain `rust-toolchain.toml` pins, with its components, retrying a failed install five times with a growing delay. Without it a fresh runner (every community-extensions registry build) has rustup fetch the toolchain lazily inside `cargo build`, once, so one network failure fails that platform and the whole registry release (v0.15.0's osx_arm64 build died that way on a DNS error). It does nothing when the toolchain is already installed, when there is no rustup on `PATH`, or when `RUSTUP_TOOLCHAIN` or a `rustup override` picks another toolchain (both take precedence over the file). Between attempts it removes a partial install, but never a toolchain that was installed before it ran; one that only lacks a component is built with if the install keeps failing. BuildAll and BuildQuick run the same reusable workflow as the registry (`_extension_distribution.yml`), so their `make release` takes the real install path on every platform and checks it before a merge. The workflows that set Rust up with `.github/actions/setup-pinned-rust` (CodeQuality, IntegrationChecks, DuckDBVersionMonitor) already have the toolchain, so there it is a no-op.

#### Which DuckDB the C++ shim compiles against

`build.rs` links a static build of the DuckDB engine *into* the extension binary
alongside `cpp/src/shim.cpp`, because the parser hook the shim registers is C++ internals
with no C-API equivalent. That makes the engine an ABI contract with whatever DuckDB loads
the result: the shim reads host objects by layout, so the extension refuses any host but
the exact release it was built against (`src/load_guard.rs`).

DuckDB 2.0 ships no amalgamation, and its release archives carry no C++ headers, so
`make ensure_engine` (`scripts/ensure_engine.py`) builds the engine from source with its
own CMake build (the `duckdb_static` target; `cmake` is required, `ninja` is used when
present) and writes `cpp/engine.manifest`, the include directories and static libraries
`build.rs` compiles and links against. The source is, in this order:

1. **An engine source tree** — `ENGINE_SRC_DIR`, default `./duckdb`. A harness that builds
   the extension against a specific engine clones it there (extension-ci-tools and
   community-extensions do, as does Haybarn's fork). That tree *is* the engine the
   extension will be loaded into, so it is built under the label the host's own build of
   it carries, never our pin. The label is, in this order:
   - a release label the harness exports (`DUCKDB_VERSION`, else `OVERRIDE_GIT_DESCRIBE`),
     used as given;
   - any other harness label, used verbatim, as DuckDB's CMake does with the same
     variable (a fork's `haybarn-v1.5.5-rc1-42-g<sha>`, a `git describe`). A harness value
     that is a git reference rather than a label is skipped: a commit (extension-ci-tools
     passes `duckdb_version` as one, and no host reports a commit as its version) or a
     branch or tag name of the tree (`main`);
   - the release tag the tree is checked out at;
   - CMake's own `vX.Y.0-devN`. That is a guess, and the build says so: N is the clone's
     commit count, so it matches only a host built from a clone of the same depth.

   A tree that is not the pinned release is reported, not refused: the result loads only
   in a DuckDB built from that tree.
2. **The pinned release** — `.duckdb-version`, from the PyPI sdist of the matching
   `duckdb` package (`.duckdb-pypi-version` while the pin is a pre-release). The sdist
   carries the exact engine source the wheel was built from, `third_party/` included, and
   PyPI is reachable from sandboxes that block GitHub. The archive must be that release
   (its `PKG-INFO`), and it is built with `DUCKDB_VERSION` set to the pin. The engine
   source carries no label or commit of its own, so nothing in the build could notice a
   wheel pin that names some other engine: `just test-engine-pins` checks the pair by
   asking the wheel which engine and commit it is. This is the local-developer path and
   the one our own CI jobs take.

The engine is built for the target the extension is: on macOS with DuckDB's 11.0
deployment target, and for the architecture in `OSX_BUILD_ARCH` when extension-ci-tools
cross-compiles (osx_amd64 on an arm64 runner). On Windows, DuckDB's CMake forces the
static MSVC runtime (`/MT`), so `.cargo/config.toml` links the extension with
`+crt-static` to match; the `cc`-compiled shim follows that. A `RUSTFLAGS` in the
environment replaces that config's flags rather than adding to them, so `build.rs`
refuses an MSVC build without `crt-static` instead of failing at the link.
The shim is also compiled with `/EHsc`, which the `cc` crate does not add on its own:
without it MSVC skips the destructors of a frame a C++ exception unwinds through, so a
bind that threw would leave its thread pointing at a dead caller context (a cancellation
watcher thread the binds used to start caused the windows_amd64 access violations from
#248 on). `shim.cpp` refuses to compile without it.

Builds are cached under `.engine/<id>/` (the static libraries, plus the extracted source
for the pinned release's headers), keyed on the pin or on the tree's label and commit, so
switching branches or engines is a cache hit; the wheel pin and a cross-compile
architecture are part of the key, and a tree with no git commit, or with uncommitted
changes to tracked files, is rebuilt every time. The first build of an engine takes a while (~20 minutes on four cores); after that
`make` only checks the cache. An entry counts as cached only when its own `manifest`
(written last, atomically) is complete and every include directory and library it names
still exists; the manifest is deleted before a rebuild starts, and anything an interrupted
or failed run left (a partial source tree, a build directory with another generator's
CMake cache) is discarded and rebuilt.

The include directories are the engine's `src/include` and, for each `third_party/`
library, its `include/` directory when it has one (else its root), as DuckDB's own CMake
does. A library root next to an `include/` holds files such as brotli's `VERSION`, which
macOS's case-insensitive file system returns for `#include <version>`. `ensure_engine`
also deletes the DuckDB 1.x amalgamation (`cpp/include/duckdb.{hpp,cpp}`,
`.amalgamation_id`) that a pre-2.0 build of this repo left behind: `duckdb.hpp` there
would shadow the engine's for the shim's quoted includes, and `build.rs` refuses to build
while it exists. `cpp/engine.manifest` is rewritten only when it changes, so
an unchanged engine keeps `cargo build` incremental; every rebuild of the libraries
changes it (its `built` line), and `build.rs` also watches the libraries themselves, so a
rebuilt engine is always relinked. `tests/build_config.rs` covers the
selection rules; `just update-engine` rebuilds the cached engine from scratch.

### Common Build Errors

**Missing submodules:**
```
make[1]: *** No rule to make target 'extension-ci-tools/makefiles/...'. Stop.
```
Fix: `git submodule update --init --recursive`

**Wrong Rust version:**
```
error[E0658]: use of unstable library feature
```
Fix: `rustup update stable`

## Testing

### Test Types

| Command | What It Tests | How It Works |
|---------|---------------|--------------|
| `just test-rust` | Unit tests (model, catalog, expand) | Runs `cargo nextest run` with bundled DuckDB -- fast, no extension loading |
| `just test-sql` | SQL logic tests (DDL + query round-trips) | Builds the extension, loads it in DuckDB, and runs the `.test` files **enumerated in `test/sql/TEST_LIST`** — the runner takes `--file-list`, not a directory glob, so a file missing from that list is silently skipped. `just check-test-list` makes that a hard error. The runner is invoked through `scripts/run_sqllogictest.py` (the Makefile's `TEST_RUNNER`), which fails the run when `require semantic_views` cannot be met (a missing or renamed extension file — the bare runner SKIPs every file and exits 0), when a listed file never runs (unparseable), when a file is SKIPPED for any reason other than a `require` of something else (e.g. `require windows`, listed loudly), when a file reports SUCCESS having executed zero statements, when any statement raised an `INTERNAL Error` (even one a `statement error` block matched: DuckDB keeps the original message inside the INTERNAL wrapper, e.g. for a scalar function that throws without being marked fallible), and when a file hits an error the runner silently ignores ("HTTP", "Unable to connect" — upstream counts those as a pass; under the local `SV_OFFLINE_EXTENSIONS=1` opt-in too: the files that need a downloaded extension are left out of the list before the wrapper runs, see below). It ends with `sqllogictest: N file(s) ran, M statements/queries executed`. |
| `just test-sql-prebuilt` | The above, minus the build | Same body as `test-sql` (which is defined as `build` + this), for callers that already have `build/debug/semantic_views.duckdb_extension`. CI's `SQL logic tests` job uses it because it downloads a prebuilt artifact; running `test-sql` there would rebuild what it just downloaded. |
| `just test-ducklake-ci` | DuckLake integration (synthetic data) | Builds extension, runs the Python DuckLake CI test — the leg included in `test-all`. With `SV_OFFLINE_EXTENSIONS=1` (local only, see below) it prints a `SKIPPED` line and exits 0 when the `ducklake` extension cannot be downloaded. |
| `just test-ducklake` | DuckLake integration (real jaffle-shop data) | Builds extension, runs the Python test against DuckLake tables; requires `just setup-ducklake` first |
| `just test-integration` | Python integration suites | Runs the full `test/integration/*.py` suite list (caret, ADBC, multi-db, concurrency, differential, interrupt, …), plus `test-examples`, `test-sqllogictest-guards`, `test-multi-statement-error` and `test-standard-sql-python` |
| `just test-examples` | Every `examples/*.py` | Builds the extension, then `scripts/run_examples.py` runs each example under `uv run --script` (so each gets its own PEP 723 dependencies — `transactional_ddl.py` gets `adbc-driver-manager`/`pyarrow`) from the repo root, and fails on any non-zero exit. Every file prints `PASS`/`FAIL`/`SKIPPED`/`XFAIL`/`XPASS`; nothing is skipped silently. Two explicit allow-lists in the script: `NEEDS_DOWNLOADED_EXTENSION` (examples that download a DuckDB extension at run time, none at present; such a file runs normally, `SKIPPED` only under `SV_OFFLINE_EXTENSIONS=1`, which is refused when `CI` is set) and `EXPECTED_FAILURES` (must fail with its recorded error and cite TECH-DEBT; a different error is a `FAIL`, a pass is a loud `XPASS` warning so the entry gets deleted). Pass file names to run a subset. Part of `test-integration`. |
| `just test-sqllogictest-guards` | The sqllogictest wrapper itself | Drives the real `make test_debug` with a one-file `TEST_LIST` per scenario and asserts each silent-skip case — missing extension, renamed extension, zero statements executed, unparseable file, `halt`, a query failing with an "HTTP" / "Unable to connect" error (which the runner's `ignore_error_messages` counts as a pass), and an expected-error block that matched an INTERNAL error — now exits non-zero with its own reason, and that a normal file, an expected-error block on an ordinary error, and a `require windows` platform skip still exit 0. The offline scenarios drive the wrapper directly: under `SV_OFFLINE_EXTENSIONS=1` only a file passed as `--offline-excused` may hit a network error, reported as `SKIPPED` (`make` passes none; it leaves `OFFLINE_EXTENSION_TESTS` out of the list instead); any other file still fails, and the variable is refused when `CI` is set (`test/integration/test_sqllogictest_guards.py`). Part of `test-integration`. |
| `just test-multi-statement-error` | Multi-statement DDL (TECH-DEBT #85) | Semantic-view DDL inside a multi-statement `execute()` string runs through the grammar productions, an error in it shows a caret on its own line, an unquoted reserved-word name is refused and `GET_DDL` quotes it, and each statement costs only its own length. Part of `test-integration`. |
| `just test-standard-sql-python` | Standard-SQL query form from Python | `con.execute()`, `con.sql()` and `duckdb.sql()` run the form, a relation over it is read by its variable name, a DataFrame or relation named like the view keeps its meaning (checked through both `execute()` and `sql()`), and the form runs before and after another statement in one string. Part of `test-integration`. |
| `just test-interrupt` | Query cancellation (TECH-DEBT #42) | Builds the extension and fires `con.interrupt()` from a second thread mid-query; asserts `semantic_view()` aborts early (not just eventually) and reports a bare `Interrupted!`, then the same for the SHOW / DESCRIBE member-type probes (single-view, DIMENSIONS FOR METRIC and cross-view) over a source whose bind expands thousands of globs. Every check runs a second time under DuckDB 2.0's `SET max_execution_time` deadline, which must surface as the engine's bare "Query exceeded maximum execution time". DuckDB checks the deadline only once execution starts, so where the slow part is a bind DuckDB runs in the caller's context (`semantic_view()`'s own bind, a member-type probe, `explain_semantic_view()`'s plan), the deadline case is held to a plain query of the same source (documented under the reference's Time Limits). Part of `test-integration`. |
| `just test-load-version-guard` | The load-time DuckDB version check | Builds the extension and `LOAD`s it, in a child process, into a DuckDB other than `.duckdb-version` (DuckDB 1.5.6, the last 1.5 release, whose C API is the one we declare; pinned in the `test-load-version-guard` recipe via `uv run --with`). Asserts the load fails with the check's message from `src/load_guard.rs` rather than loading or crashing; without the check a v1.5.6 build segfaulted a 2.0 host. When the build moves to a new DuckDB, repin the recipe to a different release. Part of `test-integration`. |
| `just test-engine-pins` | The wheel pin against the engine pin | Runs under the wheel `.duckdb-pypi-version` names and checks it is that release, that its engine reports `.duckdb-version`, and, while BuildAll / BuildQuick pin the engine by commit, that it was built from that commit. The engine is built from this wheel's sdist under `.duckdb-version`'s label, and nothing in the source could contradict a wrong pair. No build needed. Part of `test-integration`. |
| `just test-legacy-schema-spelling` | Legacy-catalog migration (TECH-DEBT #44) | Builds a real pre-scoping database file (`_definitions` keyed on `name` alone), opens it through a real `LOAD`, and asserts the migration records the **catalog's** spelling of a schema rather than the row's — then that `CREATE OR REPLACE` replaces the migrated row instead of duplicating it. Part of `test-integration`. |
| `just test-all` | Rust + SQL logic + DuckLake CI + integration | Runs `test-rust`, `test-sql`, `test-ducklake-ci`, and `test-integration` sequentially |

#### Offline runs: `SV_OFFLINE_EXTENSIONS=1` (local only)

Some legs need a DuckDB extension that is neither in the pip `duckdb` wheel nor in this repo, so DuckDB downloads it from `extensions.duckdb.org` on first use. Today that is only `just test-ducklake-ci` (`INSTALL ducklake`); `test/sql/peg_compat.test` and `examples/race_guards_and_unification.py` were listed too while they called `enable_peg_parser()` (which autoloads `autocomplete`), until DuckDB 2.0 removed it. Behind a proxy that blocks that host they fail, and a failing sqllogictest file halts the whole run.

Setting `SV_OFFLINE_EXTENSIONS=1` (e.g. `SV_OFFLINE_EXTENSIONS=1 just test-sql`) makes them report themselves instead:

- the Makefile's `test_extension_{debug,release}_internal` targets (behind `just test-sql` / `make test_debug`) run a copy of `TEST_LIST` without the files in `OFFLINE_EXTENSION_TESTS` (written to `build/test_list_offline`), print `SKIPPED (SV_OFFLINE_EXTENSIONS=1): <file> -- … NOT run` for each, and end with `N test files passed (single process) -- M SKIPPED offline` (the list is empty today, so nothing is left out);
- `test/integration/test_ducklake_ci.py` prints a `SKIPPED` line and exits 0 when `INSTALL ducklake` fails (`test_ducklake_helpers.exit_skipped_if_offline`);
- `scripts/run_examples.py` reports the files in its `NEEDS_DOWNLOADED_EXTENSION` list as `SKIPPED` without running them.

It is an explicit opt-in on purpose. A sqllogictest `require autocomplete` would skip silently wherever the extension cannot load, CI included, so a broken download in CI would switch the coverage off without a red. Without the variable nothing changes (CI runs them all and fails hard if it cannot load the extension), and with it set **and** `CI` set (GitHub Actions sets `CI=true`) the Makefile, the Python helper and the examples runner all refuse to run. A new test file that needs a downloadable extension goes in `OFFLINE_EXTENSION_TESTS` in the Makefile.
| `just coverage` | Coverage report | Runs unit tests with `cargo-llvm-cov`, fails if below 80% line coverage |
| `just lint` | Code quality (authoritative) | `cargo fmt --check` + full default-features `cargo clippy` + `cargo deny check`. The clippy step compiles the ~25 MB bundled DuckDB, so a cold run is ~10 min. |
| `just lint-fast` | Fast lint (pre-commit) | `cargo fmt --check` + the extension-feature clippy with `SV_SKIP_CPP_BUILD` (no C++ build). What the pre-commit hook runs; lints the same production code in ~1 min cold, seconds warm. |

### The Critical Difference: `cargo test` vs `just test-sql`

`cargo test` (or `just test-rust`) runs unit tests with a **bundled** DuckDB compiled into the test binary. It exercises `model.rs`, `catalog/`, and `expand/` -- the pure Rust logic.

**The bundled DuckDB is not the engine the extension ships for.** duckdb-rs has no 2.0 release, so the bundled build is DuckDB 1.5.6 (the `.duckdb-capi-version` pin), while the extension links and loads only in the DuckDB 2.0 engine in `.duckdb-version`. `cargo test`, the differential proptests and the coverage gate therefore check 1.5.6; only `just test-sql`, `test-ducklake-ci` and the Python integration suites run on 2.0. TECH-DEBT #92 records the gap and what would close it.

`just test-sql` builds the **actual extension binary** and loads it into a real DuckDB process via `LOAD`. This catches:
- ABI mismatches between the Rust code and the DuckDB version
- Registration bugs in the FFI entrypoint
- SQL logic errors in the DDL and query functions

**Always run `just test-sql` before submitting a PR.** A passing `cargo test` does not guarantee the extension loads correctly.

### How to read the 80% coverage gate (CI-6)

The `Coverage (80% minimum)` job in `CodeQuality.yml` runs `cargo llvm-cov nextest` with the **default (bundled) feature set only**. Two consequences worth keeping in mind so the number isn't misread:

- **The `extension`-gated FFI code is excluded from the denominator.** Modules compiled only under `--features extension` (the `#[no_mangle] extern "C"` bind callbacks in `src/query/{table_function,explain}.rs`, `src/ddl/*_ffi` entrypoints, etc.) are not built during the coverage run, so they neither raise nor lower the percentage. The line coverage figure describes the *pure Rust core*, not the FFI seam. Pure logic that the FFI callbacks delegate to lives in always-compiled modules (e.g. `src/query/wire.rs`) specifically so it *is* covered; behaviour that only exists across the FFI boundary is exercised by `just test-sql` and the Python integration suites instead.
- **`nextest` does not run doc tests.** `cargo test` runs them; `cargo llvm-cov nextest` does not. Doc-test-only examples therefore contribute nothing to the coverage number even though they run (and can fail) under `just test-rust` / `cargo test`.

Net: treat the 80% figure as a floor on the bundled core's line coverage, not as end-to-end coverage of the shipped extension.

### Linting the extension-gated FFI layer

`cargo clippy` (and the `Clippy (pedantic lints, deny warnings)` CI step) compiles the **default** feature set, so it never lints the `#[cfg(feature = "extension")]` FFI modules. A second CI step, `Clippy (extension feature, deny warnings)`, closes that gap:

```bash
SV_SKIP_CPP_BUILD=1 cargo clippy --no-default-features --features extension -- -D warnings
```

`SV_SKIP_CPP_BUILD` makes `build.rs` skip the C++ shim compile and the DuckDB engine link. Clippy only type-checks (it never links the final `cdylib`), so the C++ half is irrelevant and the check needs no engine build — it runs in seconds. Use the same command locally before touching any FFI (`src/query/{table_function,explain}.rs`, `src/ddl/*_ffi.rs`, `src/grammar/ffi.rs`) code. Do **not** set `SV_SKIP_CPP_BUILD` for a real `just build` — the extension won't link without the C++ shim.

**This is also what the pre-commit hook runs** (`just lint-fast` / `.cargo-husky/hooks/pre-commit`). The default-features `cargo clippy -- -D warnings` compiles the bundled DuckDB amalgamation — a ~10 min COLD build (its cargo profile differs from `cargo test`'s, so a green `cargo test` does not warm it, and tight disk evicts it between runs), which used to stall every `git commit`. The extension-feature clippy lints the same production code with no C++ build, because every `#[cfg(not(feature = "extension"))]` item in `src/` is either inside a `#[cfg(test)]` module (clippy without `--tests` skips it) or the `test_helpers` module (which carries `#[allow(clippy::pedantic, ...)]`). CI's default-features `Clippy (pedantic lints, deny warnings)` step remains the authoritative full-coverage gate.

### DuckLake/Iceberg Tests

The DuckLake integration test requires one-time setup:

```bash
just setup-ducklake   # downloads jaffle-shop data, creates DuckLake catalog (idempotent)
just test-ducklake    # runs the integration test against the real jaffle-shop data
```

This test verifies that `semantic_view()` works against DuckLake-managed Iceberg tables with real data (the jaffle-shop dataset).

## Loading the Extension

After building (`just build`), load the extension in a Python DuckDB session:

```python
import duckdb

con = duckdb.connect()
con.install_extension('build/debug/semantic_views.duckdb_extension', force_install=True)
con.load_extension('semantic_views')
```

### Complete Worked Example

```python
import duckdb

con = duckdb.connect()
con.install_extension('build/debug/semantic_views.duckdb_extension', force_install=True)
con.load_extension('semantic_views')

# Create sample data
con.execute("""
CREATE TABLE orders (
    id INTEGER, region VARCHAR, status VARCHAR, amount DECIMAL(10,2)
);
INSERT INTO orders VALUES
    (1, 'US', 'completed', 100.00),
    (2, 'US', 'completed', 200.00),
    (3, 'EU', 'completed', 150.00),
    (4, 'EU', 'pending',    50.00);
""")

# Define a semantic view (native DDL)
con.execute("""
CREATE SEMANTIC VIEW shop AS
  TABLES (o AS orders PRIMARY KEY (id))
  DIMENSIONS (
    o.region AS o.region,
    o.status AS o.status
  )
  METRICS (
    o.revenue     AS SUM(o.amount),
    o.order_count AS COUNT(*)
  );
""")

# Query with any dimension/metric combination
result = con.execute("""
    SELECT * FROM semantic_view('shop',
        dimensions := ['region'],
        metrics := ['revenue']
    ) ORDER BY region
""").fetchall()
print(result)
# [('EU', Decimal('200.00')), ('US', Decimal('300.00'))]

# See the generated SQL
con.execute("""
    SELECT * FROM explain_semantic_view('shop',
        dimensions := ['region'],
        metrics := ['revenue']
    )
""").fetchall()

# List all views
con.execute("SHOW SEMANTIC VIEWS").fetchall()

# Describe a view
con.execute("DESCRIBE SEMANTIC VIEW shop").fetchall()

# Remove a view
con.execute("DROP SEMANTIC VIEW shop")
```

For release builds, change the path to `build/release/semantic_views.duckdb_extension`.

## Updating the DuckDB Version Pin

Since DuckDB 2.0 two DuckDB versions are pinned, and they differ:

- **The engine** — **`.duckdb-version`** (repo root), the DuckDB release the extension
  runs in. The C++ shim is compiled and linked against it, and the extension refuses to
  load in any other release. **`.duckdb-pypi-version`** names its `duckdb` PyPI release
  while the pin is a pre-release (`2.0.0.dev2610151406` for `v2.0.0-alpha47600`); for a
  release, delete the file and the version without its `v` is used.
- **The C API** — **`.duckdb-capi-version`**, the C API the binary is built against and
  declares to the loader: v1.5.6, which DuckDB 2.0 still loads as a `C_STRUCT` extension
  (see `UNSTABLE_C_API_FLAG` in the Makefile). It moves only by a deliberate decision, not
  with engine bumps, and the `duckdb` / `libduckdb-sys` crates follow it (no duckdb-rs
  release targets 2.0).

| File | How It Gets the Version |
|------|------------------------|
| `.duckdb-version` | **Engine source** — single `vX.Y.Z[-alphaN]` line |
| `.duckdb-pypi-version` | **Engine's PyPI release**, only while the engine is a pre-release |
| `.duckdb-capi-version` | **C API source** — single `vX.Y.Z` line |
| `Makefile` | `TARGET_DUCKDB_VERSION` (extension-ci-tools' name: the metadata stamp and `DUCKDB_EXTENSION_MIN_DUCKDB_VERSION`) from `.duckdb-capi-version`; `ENGINE_DUCKDB_VERSION` / `ENGINE_PYPI_VERSION` from the engine pins. `DUCKDB_TEST_VERSION` (the runner's `duckdb` pip package) is pinned to the engine's PyPI release **only** when the harness's engine is that release (`DUCKDB_TAG`, else `DUCKDB_GIT_VERSION`, empty or naming it) — a harness building against another engine ships its own runner package under its own versioning |
| `cpp/engine.manifest` | Written by `scripts/ensure_engine.py` — the engine tree in `./duckdb` when a harness supplied one, else the release in `.duckdb-version` built from its PyPI sdist and verified against the pin |
| `Cargo.toml` | `duckdb` and `libduckdb-sys` `"=1.1XY0Z.0"` — the **C API** version, by hand |
| `src/lib.rs` | `MINIMUM_DUCKDB_VERSION` — the C API version declared to DuckDB at extension init, by hand |
| `test/**/*.py`, `examples/*.py`, `configure/*.py` | PEP 723 `"duckdb==…"` — the engine's PyPI release; updated by monitor workflow. One deliberate exception: `test_load_version_guard.py` needs a host *other* than `.duckdb-version`, so it has no header pin; its host comes from `uv run --with duckdb==…` in the `test-load-version-guard` recipe (1.5.6), which neither the monitor nor the pin test touches. Keep it on a release other than the new one on each bump. |
| `.github/workflows/BuildAll.yml` | `duckdb_version` (the engine's release tag, or its commit plus `duckdb_tag: <release>` while it is an untagged pre-release) and the `currently …` comment — updated by monitor workflow. The `uses:` ref and `ci_tools_version` name the extension-ci-tools branch for the release line (`v2.0-cyanoptera`), by hand |
| `.github/workflows/BuildQuick.yml` | Same as `BuildAll.yml` |
| `extension-ci-tools` submodule | Checked out to the extension-ci-tools branch the workflows name, by hand. Supplies the makefiles local `make` uses; CI's platform builds fetch `ci_tools_version` independently, so drift here shows up as a local-vs-CI build difference rather than a red check. |

Every row above **except the submodule** is machine-guarded against drift by two unit tests in `src/lib.rs`:
`tests::duckdb_version_pins_agree` (`.duckdb-capi-version` ↔ `MINIMUM_DUCKDB_VERSION` ↔ the
`libduckdb-sys` pin) and `tests::duckdb_derived_pins_agree` (the PEP 723 headers against
the engine's PyPI release, and in the two build workflows the engine label against
`.duckdb-version` and the extension-ci-tools refs against each other). Miss a location
and `cargo test` fails. The *conditional* rows — which runner gets pinned, and which
engine gets built — are guarded by `tests/build_config.rs`, which drives the real
`Makefile` and the real script in throwaway directories.

The submodule cannot be guarded that way — the correct commit is a *remote* branch tip,
so no offline test can know it. Move it together with the workflows' extension-ci-tools
ref. (Before 2.0 the monitor moved it, and its step had to fail hard when the branch did
not resolve: a version that logged a warning and exited 0 left the submodule on the
v1.5.0 tip across three bumps without anything going red.)

### Steps to Update Manually

**An engine bump** (a new 2.x release):

1. Update `.duckdb-version` to `vX.Y.Z`, and delete `.duckdb-pypi-version` (or, for a
   pre-release, set it to the wheel's version).

2. Update the Python PEP 723 headers (all `# dependencies = ["duckdb==..."]` lines):
   ```bash
   find . -name '*.py' -not -path './extension-ci-tools/*' -not -path './.engine/*' -exec grep -l 'duckdb==' {} \; \
     | xargs sed -i '' 's/duckdb==[0-9][0-9a-z.]*/duckdb==X.Y.Z/g'
   ```
   The leading `[0-9]` matters. A looser pattern (`[^"]*`, or `[0-9.]*`, both of which
   also match the empty string) rewrites `duckdb==` occurrences that are not pins, such
   as an f-string `f"...duckdb=={pyver}..."`.

3. Update both CI workflow files (`BuildAll.yml` and `BuildQuick.yml`): `duckdb_version`
   becomes the release tag, the `duckdb_tag` line goes (it is only for an untagged
   pre-release), and the `currently …` comment follows. If the release starts a new
   release line, also move the `uses:` ref, `ci_tools_version` and the submodule to that
   line's extension-ci-tools branch:
   ```yaml
   uses: duckdb/extension-ci-tools/.github/workflows/_extension_distribution.yml@v2.0-cyanoptera
   ...
     duckdb_version: vX.Y.Z
     ci_tools_version: v2.0-cyanoptera
   ```
   ```bash
   git -C extension-ci-tools fetch --depth=1 origin "+refs/heads/<branch>:refs/remotes/origin/<branch>"
   git -C extension-ci-tools checkout --detach refs/remotes/origin/<branch>
   ```
   The explicit refspec is required — the submodule clone is usually shallow, and
   `--depth` implies `--single-branch`, so a bare `git fetch origin` will not create the
   branch.

4. Build (this builds the new engine first) and run all tests:
   ```bash
   just build && just test-all
   ```

5. Commit all files together.

**A C API bump** is a separate decision (it changes which DuckDB releases accept the
binary at all): update `.duckdb-capi-version`, `MINIMUM_DUCKDB_VERSION` in `src/lib.rs`,
and the `duckdb` / `libduckdb-sys` pins in `Cargo.toml` (the crate version encodes the
DuckDB version as `1.1XY0Z.0`, e.g. DuckDB 1.5.6 = crate 1.10506.0), then build and test. Then run
`cargo deny check advisories`. If the new `libduckdb-sys` no longer pulls in `zip 6`, it
fails on the three zip ignores in `deny.toml` (`unused-ignored-advisory = "deny"`): delete
them and the `zip` entry in `[bans] deny`, and close TECH-DEBT #104.

The `DuckDBVersionMonitor` workflow automates the engine bump: it checks weekly for a DuckDB release newer than `.duckdb-version`, updates the engine pins and their derived locations (steps 1–3, minus a new release line's extension-ci-tools move), then opens a PR. If the build passes, the PR bumps the version. If it fails, the PR tags `@copilot` to attempt an automated fix.

### Version Monitor PAT

The version monitor workflow needs a fine-grained Personal Access Token (PAT) stored as a repository secret named `VERSION_MONITOR_PAT`. This is required because GitHub's default `GITHUB_TOKEN` lacks the `workflow` scope, so it cannot push changes to files under `.github/workflows/`.

**Creating the PAT:**

1. Go to [github.com/settings/personal-access-tokens](https://github.com/settings/personal-access-tokens) (Settings > Developer settings > Fine-grained tokens)
2. Click "Generate new token"
3. Set a descriptive name (e.g., `duckdb-semantic-views version monitor`)
4. Set expiration (recommend 1 year — add a calendar reminder to rotate)
5. Under **Repository access**, select "Only select repositories" and choose `s-layer/duckdb-semantic-views`
6. Under **Permissions**, grant:
   - **Contents:** Read and write
   - **Pull requests:** Read and write
   - **Workflows:** Read and write
7. Click "Generate token" and copy the value

**Saving as a repository secret:**

1. Go to the repo Settings > Secrets and variables > Actions
2. Click "New repository secret"
3. Name: `VERSION_MONITOR_PAT`
4. Value: paste the token
5. Click "Add secret"

If the PAT expires or is revoked, the version monitor will fall back to `GITHUB_TOKEN` and skip workflow file updates (logging a warning). The PR body will note the manual step required.

### Bumping DuckDB on the LTS Branch

The `duckdb/1.4.x` branch follows the same process but targets LTS releases:

1. Check out the LTS branch: `git checkout duckdb/1.4.x`
2. Update `.duckdb-version` to the new LTS version (e.g., `v1.4.5`)
3. Update `Cargo.toml`: `duckdb = { version = "=1.4.5", ... }` and `libduckdb-sys = "=1.4.5"`
4. Update Python PEP 723 headers and CI workflow files (same sed/search as main)
5. Run `just build && just test-all`
6. The Cargo.toml `version` field on LTS uses build metadata: `0.5.4+duckdb1.4`

The DuckDB Version Monitor has separate jobs for latest (`check-latest`) and LTS (`check-lts`)
that automate this process on their respective branches.

## Multi-Version Branching Strategy

The extension supports two DuckDB version tracks via separate branches:

| Branch | DuckDB Version | Purpose | Version Format |
|--------|---------------|---------|----------------|
| `main` | Latest (currently 1.5.x) | Primary development, CE registry `ref` | `0.5.4` |
| `duckdb/1.4.x` | 1.4.x (Andium LTS) | LTS compatibility | `0.5.4+duckdb1.4` |

### Development Workflow

1. **New features**: Develop on milestone branches (e.g., `milestone/v0.5.4`), squash-merge to `main`
2. **Cherry-pick to LTS**: After main is stable, cherry-pick relevant commits to `duckdb/1.4.x`
3. **Version bumps**: Each branch tracks its own DuckDB version in `.duckdb-version`

### Syncing Changes Between Branches

```bash
# Cherry-pick a commit from main to LTS
git checkout duckdb/1.4.x
git cherry-pick <commit-sha>
# Resolve any DuckDB API differences (e.g., parser_extension_compat.hpp)
just test-all
```

### CI Coverage

Both branches get the same CI. `BuildAll` runs on **push to the branch tip** (`main` /
`v1.4-lts`); `BuildQuick` runs on **pull requests only** — it has no `push` trigger, so a
branch pushed with no open PR gets no extension build. `CodeQuality` and
`IntegrationChecks` run on both pull requests and pushes to the branch tip. The DuckDB
Version Monitor checks for new releases of both the latest and LTS version lines (weekly,
Monday 09:00 UTC).

See [CI Workflows](#ci-workflows) for the per-workflow trigger table and for which
platforms actually execute sqllogictest.

## Fuzzing

Fuzzing generates random inputs to find crashes -- like Python's `hypothesis` but for binary data. Instead of testing specific examples, the fuzzer mutates inputs guided by code coverage to find edge cases you would never write by hand.

### Setup

```bash
rustup install nightly        # cargo-fuzz needs the nightly Rust compiler
cargo install cargo-fuzz      # the fuzzing CLI tool
```

The nightly toolchain is only used for fuzzing. All other development uses stable.

### Running

```bash
just fuzz                         # run default target (fuzz_json_parse) for 5 minutes
just fuzz fuzz_sql_expand         # run a specific target for 5 minutes
just fuzz fuzz_sql_expand 10      # run a specific target for 10 seconds
just fuzz-all                     # run every target sequentially (5 min each; nine targets = ~45 min)
just fuzz-all 60                  # run every target for 60 seconds each
cargo +nightly fuzz list          # see available targets
```

### The Fuzz Targets

| Target | What It Fuzzes | What It Catches |
|--------|---------------|-----------------|
| `fuzz_json_parse` | Feeds arbitrary bytes to `SemanticViewDefinition::from_json()` | Panics in JSON parsing, unexpected serde behavior on malformed input |
| `fuzz_yaml_parse` | Feeds arbitrary bytes to the YAML import path | Panics / serde surprises in YAML deserialization |
| `fuzz_ddl_parse` | Arbitrary bytes → `plan_create` (the CREATE planner the grammar's CREATE production calls) | Panics or hangs in DDL parsing on malformed statements |
| `fuzz_keyword_body` | Arbitrary bytes → `parse_keyword_body` (bypasses prefix detection) | Panics in the clause-body parser; asserts anything parsed also renders |
| `fuzz_render_roundtrip` | Generated definitions → normalize once via `parse(render(def))` → assert `render` is idempotent on the parser-produced def | Grammar drift between `render_ddl` and the body parser (dropped field, reordered clause, mis-quoted identifier). Uses the converge-once invariant, not the strong `render(parse(render(def))) == render(def)` fixpoint — that is unsatisfiable on arbitrary defs (a free-form `expr`'s surrounding whitespace is trimmed by the parser and cannot be quote-protected) |
| `fuzz_sql_expand` | Arbitrary `SemanticViewDefinition` + name arrays → `expand()` | Panics/assertion failures in SQL generation; quote/paren imbalance in the emitted SQL |
| `fuzz_query_names` | Fuzzes dimension/metric name strings against a fixed known-good definition | SQL injection via user-supplied column names, quoting bugs, name resolution panics |
| `fuzz_where_predicate` | Arbitrary `SemanticViewDefinition` + an arbitrary `where_clause` predicate → `expand()`, across the metric and fact paths | Corruption of the pre-aggregation predicate splice — the one place arbitrary user text is interpolated verbatim into generated SQL. Same balanced-in/balanced-out oracle as `fuzz_sql_expand`, with the predicate added to the precondition (an unbalanced predicate legitimately yields unbalanced SQL). This is the seam issue #145 came from |
| `fuzz_create_text` | Arbitrary CREATE text (prefixed with `CREATE SEMANTIC VIEW fuzz_view AS ` unless it already is one) → `plan_create`; anything that plans must render, plan again, and render to the same text | Definitions the parser accepts but `GET_DDL` cannot replay (TECH-DEBT #60, #59): the direction `fuzz_render_roundtrip` cannot search. Panics in the CREATE planner |

> **Adding a target:** a fuzz target is registered in three places — the `fuzz/fuzz_targets/<name>.rs` file, a `[[bin]]` entry in `fuzz/Cargo.toml`, and the `Fuzz.yml` matrix. `just fuzz-all` needs no update: it derives its list from `cargo fuzz list` (the manifest). The matrix must be hand-written, because GitHub resolves it before any job starts, so `just check-fuzz-list` (and the matching CodeQuality step) fails the build when the three disagree. That drift is not hypothetical: `fuzz_where_predicate` was in the CI matrix but missing from the old hardcoded `fuzz-all` list from #172 until 2026-08-04, so it fuzzed on every push and never locally.

> **Note:** most targets accumulate a coverage corpus under `fuzz/corpus/<target>/` (gitignored) seeded from `fuzz/seeds/<target>/` (committed). Both directories are passed to libFuzzer — `cargo fuzz run <target> fuzz/corpus/<target> fuzz/seeds/<target> -- …` in `Fuzz.yml` and the `just fuzz` / `just fuzz-all` recipes — so committed seed files ARE used as starting inputs. `Fuzz.yml` creates the (gitignored) dirs before running; the older "corpus/seed wiring is a CI gap" note is resolved (CI-1, #135).

> **Fuzz oracle design (TECH-DEBT #33):** the two struct-domain targets (`fuzz_render_roundtrip`, `fuzz_sql_expand`) and `fuzz_query_names` use hand-rolled *structural* oracles (converge-once render idempotence; balanced quotes/parens) rather than executing SQL, so they stay fast and DuckDB-free. Their preconditions must cover every fragment interpolated verbatim into the output — the recurring bug class was an *incomplete* precondition, not a wrong approach. The heavier "does DuckDB accept/return the right rows for the expanded SQL" oracle lives in the proptests, which execute against in-memory DuckDB: `tests/differential_proptest.rs` covers the single-table aggregation path (including a randomized pre-aggregation `where_clause`, whose members range over dimensions, `LABELS = (FILTER)` members and a CHAINED fact — a fact whose expression names another fact, EXP-23), `tests/star_schema_proptest.rs` covers the two-table join / fan-trap fence and the per-grain path (its metric set includes a DERIVED metric referencing a base metric by its own-qualified name, EXP-24, and its dimension set a fact of each table, which `DIMENSIONS` groups on; since Phase 6.3 a third, ROLE-PLAYED table `p` is reached from the child twice, with `USING` metrics on each path — one written over a same-table metric — and a derived metric over both, so a `pcat` grouping is refused unless every metric names a path and is otherwise oracled one aggregate per path, outer-merged) (a `ManyToOne` star: a parent-table metric grouped by a *child* dimension must be rejected, while every accepted query — including those carrying the parent metric, computed at its own grain — must match a hand-written oracle), `tests/multi_hop_join_proptest.rs` extends that to a three-table `ManyToOne` chain (`t → u → w`) with a per-grain oracle spanning up to three grains at once — only a dimension below some metric's grain is rejected, and selecting the grandparent dimension without the parent forces the resolver to pull in the intermediate table, `tests/semi_additive_proptest.rs` covers the semi-additive (`NON ADDITIVE BY`) snapshot path against an independent `MAX`/`MIN` + `IS NOT DISTINCT FROM` oracle (randomized `ASC`/`DESC`, duplicate timestamps and NULLs; a second property puts the NA dimension on a role-played table reached through the metric's `USING`, with accounts that have no rows of the metric's table), `tests/window_metric_proptest.rs` covers the window-metric partition path against an independent correlated-subquery oracle (randomized `PARTITION BY EXCLUDING` vs explicit `PARTITION BY`, `SUM`/`COUNT`/`MIN`/`MAX`, NULL partition keys), and `tests/child_grain_proptest.rs` covers the one join direction the other two omit — a metric on a CHILD of the base table, which since TECH-DEBT #71 is aggregated from the child table itself — against an oracle that scans the child and looks its parent up by correlated subquery (orphan child rows with a NULL FK counted under a NULL dimension, no row for a childless parent), with the generator producing orphans, childless parents and a child-table dimension to group by, over the whole constant-argument family the phantom-row guard was built for: constant-argument aggregates in every spelling (`COUNT(1)`, `SUM(1)`, `COUNT('x')`, `COUNT(DISTINCT 1)`, `COUNT(1+0)`, `MIN(1)`), NULL-insensitive arguments (`SUM(COALESCE(...))`), a cross-table fact reference aggregated at child grain, a randomized pre-aggregation `where_clause` whose members reach BOTH sides of the join (including the case where the predicate empties a real parent's child set, making it childless at filter time), and — the only harness that does — a row-level `facts := [...]` request with its own row-level oracle (EXP-21, EXP-25/26/28, PBT-8, PBT-13). Finally, `tests/metamorphic_proptest.rs` is the one harness with **no independently-formulated oracle**: it checks self-consistency between two formulations that must agree, which is precisely what frees its generator to range over cells the oracled harnesses cannot reach. Four properties — (a) a derived metric `f(m1…mk)` queried alone must equal `f` over its components queried separately, over the same dimensions and `where_clause`; (b) a metric's per-group values recombined with its own combiner (`sum` for `SUM`/`COUNT`, `min`/`max` for `MIN`/`MAX`) must reproduce the ungrouped total; (d) inserting a childless base row whose dimension columns clone an existing one must leave every child-grain metric and every child-fact row byte-identical; (e) swapping the declaration order of the two role-playing relationships must change no acceptance and no accepted answer (and a window metric partitioned by every queried dimension must equal its inner metric). Its model declares **two named relationships from one source table to one target** (role-playing, PBT-10; `star_schema_proptest` oracles one too since Phase 6.3), with `USING` metrics over a fact on the role-played table itself (TECH-DEBT #69), optionally spelling their relationship twice in different case; every member, physical table and relationship name — the role-playing pair included (#70) — is drawn from a five-way identifier-style axis including quoted, embedded-quote and non-ASCII spellings (PBT-12), derived chains reach depth 3 over `+`/`-`/`*`/`NULLIF` ratio, and `where_clause` and row-level `facts` requests are generated. Eleven anti-vacuity tests assert the generator reaches each axis over a fixed-seed sample — including that the comparisons see non-NULL numbers rather than agreeing about NULLs — so their thresholds shift if a generator changes, by design. Properties (a) and (b) range over mixed-grain metric sets (a base-grain and a child-grain metric together), which is what pins TECH-DEBT #71's "a metric's value never depends on its companions". The `mm1`–`mm4` tests are fixed reproductions of the defects this harness found (#69, #70, #71, #67(4)); each was `#[ignore]`d while its entry was open, stating the CORRECT invariant, and went live when it was fixed. `tests/member_type_proptest.rs` checks the read-side member types that SHOW / DESCRIBE report (TECH-DEBT #51) rather than numbers: over a two-table star with columns of random types, every dimension, fact, aggregate and derived metric must report the type an oracle computes with `typeof()` directly against its table (never through `expand`), and the type it has in a random co-queried request; a drop axis removes a source column so the members reading it must report no type while the rest keep theirs. The exact `parse(render(def)) == def` round-trip lives in `tests/roundtrip_proptest.rs`. A full trust-boundary redesign was considered and declined — see TECH-DEBT #33.

### Corpus Management

The fuzzer saves coverage-increasing inputs to `fuzz/corpus/<target>/`, which is **gitignored** (`.gitignore`: "grows locally, bootstraps from fuzz/seeds/ on fresh clone") — it is regenerated locally and in CI, not committed. The shared base everyone (and CI) starts from is the committed `fuzz/seeds/<target>/`. To add a durable repro or a known-tricky input the whole team should start from, commit it under `fuzz/seeds/<target>/`, **not** the corpus.

```bash
just fuzz-cmin                          # minimize corpus for default target (removes redundant inputs)
just fuzz-cmin fuzz_sql_expand          # minimize a specific target's corpus
```

Over time the corpus grows as the fuzzer discovers new code paths. Minimize periodically to keep it small.

### Interpreting Crashes

When the fuzzer finds a crash, it saves the triggering input to `fuzz/artifacts/<target>/`:

```bash
# Reproduce a crash
cargo +nightly fuzz run fuzz_json_parse fuzz/artifacts/fuzz_json_parse/crash-abc123

# See the debug representation of the crash input
cargo +nightly fuzz fmt fuzz_json_parse fuzz/artifacts/fuzz_json_parse/crash-abc123
```

The `fuzz/artifacts/` directory is gitignored -- crash artifacts are debugging data, not part of the corpus.

### CI Fuzzing

The `Fuzz.yml` workflow runs all nine targets (10 minutes each) on any push that touches `src/**`, `fuzz/**`, or the Cargo manifests. A documentation-only push normally does not match that path filter at all, so the workflow never starts. When it *does* start spuriously — GitHub applies the filter to the commits a push introduces rather than its net effect, so a branch restarted on merged history can match on someone else's `src/**` commits — the cheap `changes` guard job runs and skips the nine-target matrix, which is where the cost is. See the gotcha under [CI Workflows](#ci-workflows). Crash detection works by checking for artifact files (not the fuzzer exit code), so build failures or timeouts do not trigger false positives.

On a real crash:

1. The crash artifact is uploaded as a GitHub Actions artifact
2. A GitHub issue is opened with the `bug` and `fuzzing` labels
3. The job fails (red status)

**Reproducing a CI crash locally:**

```bash
# 1. Go to the Actions run linked in the issue
# 2. Scroll to the bottom of the run page and download the crash artifact zip
# 3. Unzip it into the fuzz artifacts directory:
mkdir -p fuzz/artifacts/fuzz_json_parse
unzip crash-fuzz_json_parse-*.zip -d fuzz/artifacts/fuzz_json_parse/

# 4. Reproduce the crash:
cargo +nightly fuzz run fuzz_json_parse fuzz/artifacts/fuzz_json_parse/crash-*

# 5. Inspect the crash input:
cargo +nightly fuzz fmt fuzz_json_parse fuzz/artifacts/fuzz_json_parse/crash-*
```

You can also download artifacts via the `gh` CLI:

```bash
# List artifacts from a specific run
gh run view <run-id> --repo s-layer/duckdb-semantic-views

# Download the crash artifact
gh run download <run-id> --repo s-layer/duckdb-semantic-views --name crash-fuzz_json_parse-<sha>
```

## Publishing to Community Extension Registry

The extension is published to the [DuckDB Community Extension Registry](https://duckdb.org/community_extensions/development).

### description.yml

The registry descriptor lives at `description.yml` in the repo root. It specifies the extension
metadata, build configuration, and hello_world example that appears on the CE page.

Key fields:
| Field | Value | Notes |
|-------|-------|-------|
| `name` | `semantic_views` | Must match the LOAD name |
| `language` / `build` | `Rust` / `cargo` | CE pipeline runs `cargo build` |
| `license` | `MIT` | Must match LICENSE file |
| `excluded_platforms` | semicolon-separated | Platforms we cannot support (WASM, musl, mingw, etc.) |
| `requires_toolchains` | `rust;python3` | Build-time dependencies for CE CI |
| `ref` | 40-char commit SHA | Commit the registry builds against its default DuckDB |
| `variegata` (and other release-named keys) | 40-char commit SHA | Commit the registry builds against an older DuckDB line it still supports (`variegata` = v1.5.6); read in place of `ref` for that line |

### Submitting a New Release

> **Prefer the `PublishExtension.yml` workflow** (`gh workflow run PublishExtension.yml -f version=X.Y.Z`,
> ideally with `-f dry_run=true` first). It does everything `just release` does *plus* the
> version bump, the `[Unreleased]` → `[X.Y.Z]` changelog roll (`scripts/changelog_release.py`),
> the tag, and the registry-pin gate. `just release` does **none** of those: it publishes
> whatever version `Cargo.toml` already says, so running it before bumping (and tagging)
> reopens the registry PR for the previous release rather than publishing a new one. The workflow pushes the release commits and tag to this
> repo with the job's own `GITHUB_TOKEN`, pushes the release branch to the
> `s-layer/community-extensions` fork with a GitHub App token (`BOT_APP_ID` / `BOT_APP_PRIVATE_KEY`;
> the app must be installed on the fork), and uses `CE_PUBLISH_PAT` (falling back to
> `VERSION_MONITOR_PAT`) only to open the registry PR on `duckdb/community-extensions`. The s-layer
> org refuses personal tokens, for this repo and the fork alike (the first v0.14.0 run failed its
> push with a 403 that way). Pushes made with `GITHUB_TOKEN` don't
> start other workflows, so the release commits get no CI run of their own; they change only
> versions, the changelog and `description.yml`. When the PAT expires, the DuckDB Version
> Monitor, which still checks out and opens its PR with `VERSION_MONITOR_PAT`, fails at
> `actions/checkout` with `could not read Username for 'https://github.com'`.
>
> The app must be installed on the fork with Contents and Workflows set to read and write
> (Workflows because the branch is cut from upstream `main` and can carry workflow-file changes
> the fork lacks). `CE_PUBLISH_PAT` should be a classic PAT with `public_repo`. The v0.14.0 and
> v0.14.1 runs both failed with `Permission to anentropic/community-extensions.git denied` after
> tagging, when the push still used the PAT, so the workflow now checks this first: a
> **preflight** step runs `git push --dry-run` to the fork with the app token before anything is
> committed or tagged, in dry runs too, and fails with that remedy. It also checks the PAT that
> opens the upstream PR: it must be set, accepted by GitHub (not expired), and, for a classic PAT,
> carry `public_repo` or `repo`. If a run tagged the release but
> did not open the registry PR, fix the cause and re-run with `-f registry_pr_only=true`. That
> publishes the version already on `main` from its existing tag, checking that `description.yml`
> points at it, and bumps, rolls and tags nothing. Never re-run a normal publish after its
> "Commit, tag, and push" step succeeded: it would cut another version. If an open PR from the
> fork's `semantic-views` branch already exists, the force-push updates it, the workflow retitles
> it for the new version, and no second PR is opened.
>
> The registry-pin gate and the descriptor update use `scripts/registry_target.py`, guarded by
> `tests/registry_target_script.rs`. The registry builds its default DuckDB (`DUCKDB_LATEST_STABLE`
> in its `build.yml`: `v2.0.0` since DuckDB 2.0.0 GA; before that a 2.0 commit built as the release
> its build job's `duckdb_tag` default names, e.g. `v2.0.0-alpha46057`) from `repo.ref`, and each older line it
> still builds from a release-named key that its `scripts/build.py` reads instead (`variegata` for
> v1.5.6, `andium` for v1.4.5). The gate maps `.duckdb-version` to that key (a pin equal to the
> default's commit, or to the `duckdb_tag` of the job that builds that commit, or a DuckDB tag that
> points at that commit, resolved with `git ls-remote`, is the default) and refuses a pin the
> registry does not build; the release SHA is then written under that key, leaving `ref` as it was.
> `just release` uses the same script, so a manual release writes the same key. The gate does not
> model the registry's `ref_next`: `build.py` reads it for any other DuckDB the registry is asked to
> build (a `workflow_dispatch` run naming a non-default version), so a release meant for such a run
> needs `repo.ref_next` set in `description.yml` by hand. The script never prints that key.
>
> The monitor's changelog bullet comes from `scripts/changelog_add_bullet.py`, guarded by
> `tests/changelog_scripts.rs`. Still read the rolled section in the dry-run diff: it becomes
> the release notes verbatim.

The `just release` recipe automates steps 3-7 below:

1. Complete the milestone on the milestone branch
2. Squash-merge to `main` and tag the release (e.g., `v0.5.4`)
3. Run `just release`

This will:
- Verify you're on `main` with a clean working tree and `gh` CLI installed, then fetch `origin/main` and fast-forward to it (it stops if local `main` has diverged)
- Extract the version from `Cargo.toml` and stop if local `main` has commits `origin/main` does not (`scripts/registry_target.py unpushed`), since the push below would publish them; the one exception is a re-run's own unpushed `description.yml` commit for this version. Fetch its `v<version>` tag from `origin` if it is there (it stops if the local tag differs), and take the commit the tag points at (`scripts/registry_target.py release-sha`; it refuses when the tag is missing or `origin/main` does not contain the tagged commit). Not `HEAD`: `main` may have moved past the release, as it had when v0.14.0's registry PR was first opened
- Update `description.yml` with the tagged commit under the key the registry builds our DuckDB pin from (`ref`, or an older DuckDB line's key such as `variegata`; `scripts/registry_target.py key`) and the new `version`, commit, and push to `main` if that left it ahead of `origin/main` (a re-run for the same release finds it unchanged and goes straight to the registry PR)
- In the CE fork (default: `~/Documents/Dev/Sources/community-extensions`, override with `CE_REPO` env var): ensure an `upstream` remote, `git fetch upstream main`, and **rebranch `semantic-views` off `upstream/main`** so the PR is a clean one-file change, then copy in `description.yml`, commit, and **force-push**
- Open a PR to `duckdb/community-extensions` (or report the existing open one, which the force-push updates in place)

> **Why rebranch off `upstream/main` (not reuse the local `semantic-views` branch)?** The registry's `prepare` job rejects a PR that touches more than one extension's descriptor (`ValueError: cannot have multiple descriptors changed`). If the branch is left pinned to a previous release's base, the ~100 other extensions that updated upstream in the meantime all show as changed and the job fast-fails. Rebranching off current `upstream/main` guarantees the diff is only `extensions/semantic_views/description.yml`. The `PublishExtension.yml` CI workflow does the same thing.

**Prerequisites:**
- A fork of [duckdb/community-extensions](https://github.com/duckdb/community-extensions) cloned locally (an `upstream` remote is added automatically if missing)
- The `gh` CLI authenticated with permissions to create PRs

**After running:**
- Wait for the CE build pipeline to pass (builds across all non-excluded platforms)
- After merge, the extension is installable via:
  ```sql
  INSTALL semantic_views FROM community;
  LOAD semantic_views;
  ```

### Adding LTS Support

To publish builds for DuckDB 1.4.x (Andium LTS), add the `andium` field to `description.yml`:
```yaml
repo:
  github: s-layer/duckdb-semantic-views
  ref: <main-branch-sha>
  andium: <duckdb-1.4.x-branch-sha>
```
The CE pipeline will build from `andium` SHA against DuckDB 1.4.x.

## Worked Examples

### Adding a New DDL Statement

Suppose you want to add an `ALTER SEMANTIC VIEW old_name RENAME TO new_name` statement.

Since v0.5.2, DDL is handled via parser hooks -- the C++ shim detects `CREATE/DROP/ALTER/SHOW/DESCRIBE SEMANTIC VIEW` prefixes and routes them through the Rust FFI trampoline. New DDL statements follow the same pattern.

**1. Add the DDL kind:**

In the `parse` module (the `DdlKind` enum lives in `src/parse/`), extend it:

```rust
pub enum DdlKind {
    Create { or_replace: bool, if_not_exists: bool },
    Drop { if_exists: bool },
    Describe,
    Show,
    AlterRename { if_exists: bool, new_name: String },  // <-- add this
}
```

**2. Update the parse layer:**

Add prefix detection in `src/parse/detect.rs` and the rewrite-to-native-SQL handling in `src/parse/rewrite.rs` for the `ALTER SEMANTIC VIEW … RENAME TO` form.

**3. Add a SQL logic test:**

Create or extend a file in `test/sql/`:

```
statement ok
CREATE SEMANTIC VIEW original AS
  TABLES (t AS my_table PRIMARY KEY (id))
  DIMENSIONS (t.x AS t.x)
  METRICS (t.total AS SUM(t.y));

statement ok
ALTER SEMANTIC VIEW original RENAME TO renamed

query I
SHOW SEMANTIC VIEWS
----
renamed

statement ok
DROP SEMANTIC VIEW renamed
```

**4. Update this architecture section** to document the new DDL kind.

### Adding a New Metric Type

Suppose you want to add a `window` metric type that generates a window function instead of an aggregate. (Window metrics already exist — see `WindowSpec` in `src/model.rs` and `src/expand/window.rs`; this walks the general shape of such a change.)

**1. Update the body parser:**

In `src/body_parser/metrics.rs`, extend the METRICS clause parsing to accept the window modifier:

```sql
METRICS (
    o.running_total WINDOW AS SUM(o.amount),
    o.revenue AS SUM(o.amount)
)
```

**2. Update the expansion layer (`src/expand/`):**

In the SELECT-item generation (`src/expand/sql_gen.rs`), handle window metrics differently:

```rust
for met in &resolved_mets {
    let item = if met.is_window {
        format!("    {} OVER () AS {}", met.expr, quote_ident(&met.name))
    } else {
        format!("    {} AS {}", met.expr, quote_ident(&met.name))
    };
    select_items.push(item);
}
```

**3. Add unit tests:**

```rust
#[test]
fn test_window_metric_type() {
    // Parse a DDL body with WINDOW metric modifier
    let body = r#"
        TABLES (o AS orders PRIMARY KEY (id))
        DIMENSIONS (o.region AS o.region)
        METRICS (o.running_total WINDOW AS SUM(o.amount))
    "#;
    let def = parse_body(body).unwrap();
    assert!(def.metrics[0].is_window);
}
```

**4. Add a proptest property** to verify that `expand()` never panics with arbitrary metric configurations.

**5. Update fuzz targets:** Add a seed file to `fuzz/corpus/fuzz_ddl_parse/` with the new WINDOW modifier so the fuzzer starts from a valid example.

## Troubleshooting

### "built for DuckDB vX and cannot run in DuckDB vY", "Symbol not found" or another ABI mismatch when LOADing

```
semantic_views was built for DuckDB v1.5.6 and cannot run in DuckDB v2.0.0, because it depends on that exact DuckDB release. ...
Error: Catalog Error: Failed to load extension: Symbol not found
```

**Why:** The extension was built against a different DuckDB version than the one loading it. Our C++ shim depends on DuckDB internals, which change between releases, so the extension binary must match exactly. The extension declares the `C_STRUCT` ABI pinned to the v1.5.6 C API, so DuckDB itself accepts it in any host from 1.5.6 on; the first message is our own load-time check (`src/load_guard.rs`) refusing it instead.

**Fix:** Ensure the DuckDB Python package is the engine release the extension was built for (`.duckdb-version`, whose PyPI release is `.duckdb-pypi-version` while it is a pre-release):

```bash
pip show duckdb           # check installed version
cat .duckdb-version .duckdb-pypi-version 2>/dev/null  # check the engine pin
```

If they differ, either update the pin (see [Updating the DuckDB Version Pin](#updating-the-duckdb-version-pin)) or install the matching Python DuckDB version:

```bash
pip install "duckdb==$(cat .duckdb-pypi-version 2>/dev/null || sed 's/^v//' .duckdb-version)"
```

### `cargo test` passes but `just test-sql` fails

**Why:** `cargo test` uses the `bundled` feature, which compiles DuckDB from source into the test binary. This bypasses the extension loading mechanism entirely. The `ddl/` and `query/` modules are not even compiled during `cargo test`. So a passing `cargo test` only validates the pure Rust logic in `model.rs`, `catalog/`, and `expand/`.

`just test-sql` builds the actual extension (with `--features extension`) and loads it in a real DuckDB process. This catches FFI registration bugs, ABI mismatches, and SQL-level errors.

**Fix:** Always treat `just test-sql` as the authoritative test. If it fails but `cargo test` passes, the bug is likely in the DDL/query modules or the FFI entrypoint.

### Clippy pedantic lint failures

```
error: this function could have a #[must_use] attribute
```

**Why:** The project enables `clippy::pedantic` at deny level. Pedantic lints catch real issues but some are noisy.

**Fix:** If the lint is legitimate, fix it. If it is a false positive, add a targeted `#[allow(...)]` attribute:

```rust
#[allow(clippy::needless_pass_by_value)]  // required by FFI signature
fn my_function(val: String) { ... }
```

The priority = -1 pattern in `Cargo.toml` ensures individual lint `#[allow]` directives override the blanket `pedantic` deny. Three lints are globally allowed: `module_name_repetitions`, `missing_errors_doc`, and `missing_panics_doc`.

### Linker errors with "loadable-extension"

```
error: linking with `cc` failed
undefined reference to `duckdb_rs_extension_api_init`
```

**Why:** You are trying to compile test code with the `extension` feature. The `loadable-extension` feature replaces all DuckDB C API calls with function-pointer stubs that are only initialized when DuckDB loads the extension at runtime. A standalone binary (like a test) cannot use these stubs.

**Fix:** Use `cargo test` (default features, which enables `bundled`) for unit tests. Only the Makefile build (`just build`) should use `--features extension`.

### Fuzz target won't compile

```
error: `-Zsanitizer=address` is not a valid flag
```

**Why:** `cargo-fuzz` requires the nightly Rust toolchain for sanitizer support.

**Fix:**

```bash
rustup show          # check which toolchain is active
rustup install nightly
cargo +nightly fuzz run fuzz_json_parse  # must use +nightly for sanitizer flags
```

Do **not** set a directory-level nightly override (`rustup override set nightly`) -- this would break the stable-toolchain builds for everything else.

## CI Workflows

| Workflow | Trigger | What It Does |
|----------|---------|--------------|
| **BuildQuick** | Pull requests (skips doc-only changes) | Fast feedback: extension **build** on Linux x86_64 only, via the DuckDB extension-ci-tools reusable workflow. No `push` trigger (runs on PRs + manual dispatch) — `main` gets the full platform matrix from BuildAll. It invokes `make test_release`, but that runs **no tests** here: `linux_amd64` is on upstream's `SKIP_TESTS` list (see the note below), so this workflow is build-only. Both build workflows call extension-ci-tools' `v2.0-cyanoptera` branch with the engine pinned in `.duckdb-version`; while that is an untagged pre-release they pass its commit as `duckdb_version` plus `duckdb_tag: <release>`, which the Makefile's `set_duckdb_tag` turns into a tag on the clone so the engine is built under its release label. |
| **BuildAll** | Push to `main` (skips doc-only changes) | Full build across 5 platforms: Linux x86_64/arm64, macOS x86_64/arm64, Windows x86_64. Excludes WASM, musl, mingw variants. **Runs the full sqllogictest suite on `windows_amd64` and `osx_arm64`** (release build, ~30–55 s each). The other three do not test: `linux_amd64` and `linux_arm64` are skipped by upstream, and `osx_amd64` is cross-compiled on an arm64 runner so its binary cannot be executed. |
| **CodeQuality** | Push to `main` + pull requests (skips doc-only changes) | `TEST_LIST` sync check; fuzz-target registration sync check (`.rs` files vs `fuzz/Cargo.toml` `[[bin]]` entries vs the `Fuzz.yml` matrix); `cargo fmt --check`; clippy (default **and** `--features extension`); doctests (default + the FFI `compile_fail` ABI guard); extension-feature unit tests; `cargo-deny` (license/advisory audit); 80%-line coverage floor via `cargo-llvm-cov`. Runs as **three parallel jobs** — `Lint & format`, `Doctests & extension unit tests`, `Coverage (80% minimum)` — so their three separate cold compiles of the bundled DuckDB amalgamation (clippy=check, doctests=build, coverage=instrumented) overlap on different runners instead of running serially (~32 min → ~12 min). |
| **IntegrationChecks** | Push to `main` + pull requests (skips doc-only changes) | Four jobs. `Build debug extension` runs `make configure debug` **once** and uploads the binary; the DuckLake CI test and the **sqllogictest suite** `needs:` it and download that artifact instead of rebuilding. The sqllogictest job runs `just test-sql-prebuilt`. The Python integration job builds its own, because every leaf recipe under `just test-integration` declares `build` as a dependency, so its recipe graph re-enters `make debug` regardless; that job also runs **every example** (`just test-examples`) and the sqllogictest-wrapper guards (`just test-sqllogictest-guards`), the multi-statement DDL error check (`just test-multi-statement-error`) and the standard-SQL Python check (`just test-standard-sql-python`), all via `test-integration`. This is the authoritative sqllogictest gate for pull requests. Both building jobs cache `.engine/` (the DuckDB engine build, keyed on the engine pins and `scripts/ensure_engine.py`), so only a pin change pays for an engine build. |
| **DocsCheck** | Pull requests | Sphinx docs build with `-W` (warnings as errors). Deliberately **not** path-filtered, so documentation/text-only changes are still validated when the heavier workflows skip. No `push` trigger (runs on PRs + manual dispatch) — `main` gets the build+deploy from Docs. |
| **Docs** | Push to `main` | Same `-W` Sphinx build, then deploys the site to GitHub Pages. |
| **Fuzz** | Push touching `src/**`, `fuzz/**`, or the Cargo manifests | Runs all nine fuzz targets for 10 minutes each. Detects crashes via artifact files (not exit codes), uploads them, opens a `bug`/`fuzzing` issue, and fails the job on any crash. A `changes` guard job re-checks the push's **net** diff and skips the matrix when the trigger fired only because of the push-range quirk below. |
| **DuckDBVersionMonitor** | Weekly (Monday 09:00 UTC) + manual | Queries the DuckDB GitHub API for the latest / LTS release. If newer than the pin (a release counts as newer than a pre-release of the same version), updates the engine pin and its derived locations (never the C API pin), builds, and tests, then opens a version-bump PR on success or a breakage PR (tagging `@copilot`) on failure. |
| **PublishExtension** | Manual (`workflow_dispatch`) only | Release automation for the Community Extension registry. Inputs: `version`, `dry_run`, and `registry_pr_only` (open the registry PR for the release already tagged on `main`). A preflight checks that the app token can push to the fork before anything is tagged. |

Most workflows also accept a manual `workflow_dispatch` trigger for debugging.

**PR vs push triggers (CI-2).** The PR-validating workflows run on `pull_request` — **every** PR, same-repo **and** fork — so PRs from forks / non-collaborators, which never fire this repo's `push` events, get full CI (they previously got none). `CodeQuality` and `IntegrationChecks` also run on `push` to `main` to validate the merged state; because that `push` is `main`-only, a same-repo PR's branch push does **not** trigger them, so each PR runs exactly **once** via `pull_request` — no fork-detection guard or double-run logic. `BuildQuick` and `DocsCheck` have no `push` trigger (they run on `pull_request` plus manual `workflow_dispatch`), because `main` is already covered by `BuildAll` (full matrix) and `Docs` (build + deploy). `Fuzz` stays `push`-triggered: it needs `issues: write` to file crash issues (which fork PRs cannot grant) and fuzzing untrusted fork code is best avoided, so it runs on same-repo branch pushes and `main`, never on fork PRs. Trade-off: a branch pushed with **no open PR** gets no CI — the check runs the moment a PR exists (and re-runs on every new commit via `pull_request: synchronize`).

**Where sqllogictest actually runs, and where `make test_release` silently does nothing.** `BuildQuick` / `BuildAll` delegate to the extension-ci-tools reusable workflow, whose final step is `make test_release`. Whether that runs anything is decided **per platform** by upstream's `SKIP_TESTS` list in `extension-ci-tools/makefiles/c_api_extensions/base.Makefile`:

| platform | tests? | why |
|---|---|---|
| `linux_amd64` | **no** | upstream `SKIP_TESTS=1` — the Ubuntu runners install the `linux_amd64_gcc4` test runner against the wrong glibc wheel (upstream TODO) |
| `linux_arm64` | **no** | built in Docker; `LINUX_CI_IN_DOCKER=1` sets `SKIP_TESTS=1` |
| `osx_amd64` | **no** | cross-compiled on an arm64 runner, so the binary cannot be executed there |
| `osx_arm64` | **yes** | full suite, release build (~30 s) |
| `windows_amd64` | **yes** | full suite, release build (~55 s) |
| musl / mingw / rtools / wasm | **no** | excluded from our matrix, and on upstream's skip list anyway |

Two consequences worth keeping in mind:

1. **`BuildQuick` builds only `linux_amd64`, so it never tests.** Its `make test_release` step prints `Skipping tests..` and exits 0, which reads as a green build. Before #223 that meant **no** sqllogictest ran on pull requests at all — long enough for an incorrect assertion to reach `main` in #217 (an `IDENT-6` case asserting a quoted name is reported unquoted, corrected in #219). The `sqllogictest` job in `IntegrationChecks` is the authoritative PR gate; do **not** "simplify" it away on the assumption that BuildQuick covers it.
2. **Cross-platform sqllogictest is real, but only on `main`,** via BuildAll's Windows and macOS-arm64 legs. A PR therefore gets Linux-only SQL coverage by design (the suite carries no platform-conditional directives — every file is a plain `require semantic_views`), and platform-specific breakage surfaces on the `main` push instead.

You can tell the two apart in a job log without reading it all: a real run prints `Running RELEASE tests..`, then one `[n/N] test/sql/….test` line per file, then `sqllogictest: N file(s) ran, M statements/queries executed` (from `scripts/run_sqllogictest.py`, which fails the run rather than print it if any listed file skipped `require semantic_views`, never ran, or executed nothing) and `N test files passed (single process)`; a skipped one prints `Skipping tests..` and nothing else. On Windows, a runner that dies of an access violation is run a second time under `cdb` (`scripts/windows_crash_report.py`, which the Makefile puts in front of `TEST_RUNNER` there): the log then holds the registers, native stack and loaded modules between `===== SV CRASH REPORT =====` markers, and the job still fails with the first run's status. The `ignoring old recipe for target 'test_extension_*_internal'` warnings are **not** a symptom of skipping — they appear in runs that do execute the suite, and simply record this project's Makefile overriding upstream's recipe.

**Documentation-only skip.** `BuildAll`, `BuildQuick`, `CodeQuality`, and `IntegrationChecks` carry a shared `paths-ignore` (`**/*.md`, `_notes/**`, `docs/**`, `LICENSE`), so a change that touches only documentation/text does not run the extension build, sqllogictest, Rust lint/coverage, or the integration suites. `Fuzz` achieves the same via its `paths` allowlist. `DocsCheck` is intentionally exempt so prose changes still get the `-W` docs build. A change that also touches any non-doc file runs everything as normal. When editing these triggers, keep the four `paths-ignore` lists in sync.

> **Gotcha:** `push` path filters are evaluated against the commits a push *introduces* (the `before..after` range), not the net diff. This applies to every `push`-triggered workflow — `BuildAll`, `Docs`, `Fuzz`, and the `main`-push runs of `CodeQuality` / `IntegrationChecks`. It bites hardest for **Fuzz**, the only one still triggered on non-`main` branch pushes: a feature branch **force-pushed on top of already-merged history** can trigger it even for a doc-only net change, because the push range then includes earlier `src/**` commits (e.g. a squash-merge). The `pull_request`-triggered workflows evaluate path filters against the PR's net diff, so pre-merge PR validation is unaffected.
>
> **Fuzz guards against this (#168).** Restarting a branch from `main` between PRs — the normal flow for a sequence of stacked changes — hits the quirk every time, costing 8 × 10 minutes of runner time to re-fuzz code `main` already fuzzed. `Fuzz.yml` therefore runs a cheap `changes` job first: it recomputes the **two-dot** (tree-vs-tree) diff for `before..after` and skips the matrix when nothing under `src`, `fuzz`, or the Cargo manifests actually changed. It **fails open** — a new branch (all-zero `before`), an unavailable before-commit (force-push GC), or a `workflow_dispatch` run all fuzz rather than skip, and so does a guard job that errors outright. That last case is why the matrix gates on `!cancelled() && … != 'false'` rather than `== 'true'`: a failed `needs` job skips its dependents unless the `if:` uses a status-check function, so the intuitive `== 'true'` would silently stop fuzzing whenever the guard broke. Only an explicit `false`, or a cancelled run, skips the matrix. Moving `Fuzz` to a `pull_request` trigger would fix the same waste but forfeit the CI-2 properties above (`issues: write`, no fork-code fuzzing), so the guard lives in the job, not the trigger.
