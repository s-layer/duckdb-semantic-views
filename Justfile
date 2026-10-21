# DuckDB Semantic Views — developer task runner
# Run `just` to see available commands

# Show available commands (default)
default:
    @just --list

# Set up complete local dev environment (one-time, new contributors)
# Downloads pinned DuckDB binary, installs dev tools, wires cargo-husky hooks
setup:
    @echo "Installing dev tools..."
    cargo install cargo-nextest --locked
    cargo install cargo-deny --locked
    cargo install cargo-llvm-cov --locked
    cargo install cargo-fuzz --locked
    cargo install cargo-sweep --locked
    git submodule update --init --recursive
    make configure
    @echo "Running cargo test to install cargo-husky hooks..."
    cargo test
    @echo "Setup complete. Run 'just build' to build the extension."

# Build debug extension
build:
    make debug

# Build release extension
build-release:
    make release

# Run tests via SQLLogicTest (exercises actual LOAD mechanism)
test:
    make test_debug

# Run Rust unit tests only (does NOT test LOAD — use 'just test' for that).
# nextest cannot run doctests, so they run separately (T-12): the default set,
# plus the `extension`-gated FFI doctests (the load-bearing `compile_fail`
# ABI-safety guard in src/ddl/read_ffi.rs). The FFI run type-checks only —
# SV_SKIP_CPP_BUILD skips the C++ shim and engine link, and a `compile_fail` doctest
# never links — and is scoped to `read_ffi` because other doctests assume the
# default `duckdb` feature.
test-rust:
    cargo nextest run
    cargo test --doc
    SV_SKIP_CPP_BUILD=1 cargo test --doc --no-default-features --features extension read_ffi
    # `cargo nextest run` above uses default features, so #[cfg(feature =
    # "extension")] UNIT tests are otherwise never compiled or run — two such
    # tests silently rotted (a stale assertion + a signature-change compile
    # break) before this step was added. --lib scopes to the library's own
    # tests (the integration crates assume the default `duckdb` feature);
    # SV_SKIP_CPP_BUILD skips the C++ build, the pure-Rust tests still run.
    SV_SKIP_CPP_BUILD=1 cargo test --lib --no-default-features --features extension

# Run all lints (authoritative: full default-features clippy, mirrors CI). The
# `cargo clippy` step compiles the ~25 MB bundled DuckDB amalgamation, so a cold
# run takes ~10 min — see `lint-fast` for the quick pre-commit-equivalent check.
lint:
    cargo fmt --check
    cargo clippy -- -D warnings
    cargo deny check

# Fast lint — what the pre-commit hook runs. The extension-feature clippy skips
# the C++ shim build and engine link (SV_SKIP_CPP_BUILD) and lints the same production
# code (every `cfg(not(feature = "extension"))` item in src/ is test-only or
# lint-suppressed): ~1 min cold, seconds warm. `just lint` / CI remain the
# authoritative full-coverage gate.
lint-fast:
    cargo fmt --check
    SV_SKIP_CPP_BUILD=1 cargo clippy --no-default-features --features extension -- -D warnings

# Format code in place
fmt:
    cargo fmt

# Check code coverage (requires cargo-llvm-cov)
coverage:
    cargo llvm-cov nextest --fail-under-lines 80

# Ensure the Python venv exists and its duckdb pip package is the engine release
# the extension is built for (.duckdb-pypi-version, else .duckdb-version without
# its `v`). Called automatically by test recipes that use the venv runner.
#
# The `make configure` guard makes those recipes self-contained. `test-sql`
# reaches the venv via `build` (which fails loudly on `check_configure`), but
# `test-sql-prebuilt` and `test-ducklake-ci` do not build first, so without this
# a fresh checkout hit a confusing `configure/venv/bin/python: not found` from
# the version probe below instead of the real cause. `make configure` is a no-op
# when configure/venv already exists, so this costs nothing on the common path.
[private]
_ensure-test-deps:
    @if [ ! -d configure/venv ]; then \
      echo "configure/venv missing — running make configure first..."; \
      make configure; \
    fi
    @VER=$(cat .duckdb-pypi-version 2>/dev/null || sed 's/^v//' .duckdb-version); \
    INSTALLED=$(configure/venv/bin/python -c "import duckdb; print(duckdb.__version__)" 2>/dev/null || echo ""); \
    if [ "$INSTALLED" = "$VER" ]; then \
      exit 0; \
    fi; \
    echo "duckdb pip version mismatch (have=${INSTALLED:-none}, want=$VER), reinstalling..."; \
    configure/venv/bin/pip install -q "duckdb==$VER"

# Run SQL logic tests for Phase 2 DDL via the SQLLogicTest runner.
#
# There is no standalone DuckDB CLI available in this project; SQL logic tests
# are run via the Python-based duckdb_sqllogictest runner (installed by
# `make configure` into its Python venv).  This recipe builds the debug extension
# and delegates to `make test_debug`.
#
# Only the files ENUMERATED IN test/sql/TEST_LIST are executed — the runner is
# invoked with `--file-list`, not a directory glob. A new .test file that is not
# added to TEST_LIST is silently skipped: never run, never red. `just
# check-test-list` (and the CodeQuality job that calls it) exists to make that a
# hard error rather than a quiet gap.
#
# The test/sql/phase2_ddl.test file exercises the full DDL round-trip:
#   define_semantic_view, list_semantic_views, describe_semantic_view, drop_semantic_view.
test-sql: build test-sql-prebuilt

# The body of `test-sql`, minus the build.
#
# CI's `SQL logic tests` job downloads a debug extension built once by an
# earlier job rather than building its own, so it needs an entry point that
# does NOT re-enter `make debug`. Splitting the recipe rather than letting CI
# call `make test_debug` directly keeps `_ensure-test-deps` on both paths: it
# reinstalls the venv's `duckdb` pip package when it is not the engine release
# the extension is built for, without which the runner can test against a different
# DuckDB than the extension was built for. Local `just test-sql` is unchanged —
# it still builds first and then runs exactly this.
#
# Requires build/debug/semantic_views.duckdb_extension to exist already.
test-sql-prebuilt: _ensure-test-deps
    make test_debug

# Run the SQL logic tests with every statement the grammar productions cover
# (src/grammar/) parsed by the grammar instead of the parser override. Both
# implementations coexist during Phase 3 of the DuckDB 2.0 epic; this holds the
# grammar to the same tests, the DDL corpus included.
test-sql-grammar: build _ensure-test-deps
    SV_ROUTE_TO_GRAMMAR=1 make test_debug

# Download jaffle-shop data and create DuckLake/Iceberg catalog for integration tests.
# Idempotent — safe to run multiple times. Data files are gitignored.
# Uses uv to run the script with its declared dependencies (PEP 723).
setup-ducklake:
    uv run configure/setup_ducklake.py

# Download jaffle-shop data (if needed) and run the local DuckLake integration test.
# Convenience wrapper — safe to run repeatedly; setup-ducklake is idempotent.
test-ducklake: setup-ducklake build
    uv run test/integration/test_ducklake.py

# Run DuckLake CI integration test (uses inline synthetic data, no setup required).
# Tests semantic_view against an in-memory DuckLake catalog with known synthetic rows.
# Set SEMANTIC_VIEWS_EXTENSION_PATH to override the extension path (default: build/debug/).
test-ducklake-ci: _ensure-test-deps
    uv run test/integration/test_ducklake_ci.py

# Run Python vtab crash reproduction tests against the built extension.
# Exercises 5 crash vectors (13 tests) for type mismatch, connection lifetime,
# bind-time execution, chunking, and pointer stability.
test-vtab-crash: build
    uv run test/integration/test_vtab_crash.py

# Run Python caret position integration tests against the built extension.
# Verifies that DuckDB error caret (^) renders at the correct character position
# when malformed DDL flows through the extension's parser hook pipeline.
test-caret: build
    uv run test/integration/test_caret_position.py

# Run ADBC end-to-end transactional DDL tests against the built extension.
# Exercises CREATE / DROP / ALTER SEMANTIC VIEW under an ADBC autocommit=False
# connection — proves the v0.8.0 transactional-DDL fix works for the original
# motivating bug.
test-adbc: build
    uv run test/integration/test_adbc_transactions.py

# Run ADBC end-to-end query tests against the built extension.
# Exercises SELECT ... FROM semantic_view(...) through adbc_driver_duckdb
# across main expand path, FACTS, semi-additive, window, materialization
# routing, non-default-schema base tables, and multi-DB ATTACH. Regression
# guard for EXPAND-CTX-01..03 (v0.10.0). All scenarios run: the
# qualify_and_quote_table_ref migration of the FACTS/semi-additive/window/
# materialization sites landed in v0.10.0.
test-adbc-queries: build
    uv run test/integration/test_adbc_queries.py

# Run regression test for the v0.8.0 silent-truncation FFI buffer bug.
# Creates a semantic view large enough that the rewritten INSERT exceeds
# the legacy 64 KB shim buffer; pre-fix this would have produced a
# misleading "Parser Error: syntax error" instead of succeeding.
test-large-view: build
    uv run test/integration/test_large_view_rewrite.py

# Multi-DB DDL isolation regression: load the extension into two databases
# in the same process and verify DESCRIBE / SHOW route to the right database.
# Pre-fix the C++ shim held a global sv_ddl_conn that the second LOAD would
# overwrite, causing the first DB's DESCRIBE/SHOW to silently target the
# second DB's connection.
test-multi-db: build
    uv run test/integration/test_multi_db_isolation.py

# Read-only database LOAD regression: bootstrap a view writable, reopen
# the file read-only via duckdb.connect(path, read_only=True), and
# verify LOAD succeeds, list_semantic_views() returns the bootstrapped
# view, semantic_view(...) queries work, and CREATE/DROP/ALTER fail
# with DuckDB's standard read-only error. Phase 63 (v0.9.0). See
# test/sql/readonly_load.test for the writable-side smoke fixture and
# the explanation of why full read-only coverage lives here rather
# than in sqllogictest.
test-readonly: build
    uv run test/integration/test_readonly_load.py

# Concurrent CREATE on the same view name from two threads. PK constraint
# on _definitions(name) serializes the inserts; exactly one must succeed.
# Also indirectly exercises the v0.8.0 race-guard pattern for DROP/ALTER.
# Phase 65.1 adds two per-call Connection regressions covering the
# read-side (concurrent SHOW SEMANTIC ... reads) and write-side
# (concurrent CREATE/DROP/ALTER on overlapping names) of the per-call
# Connection model — both must run in CI so the borrow contract is
# guarded under contention.
test-concurrent: build
    uv run test/integration/test_concurrent_ddl.py
    uv run test/integration/test_concurrent_reads_per_call_conn.py
    uv run test/integration/test_concurrent_writes_per_call_conn.py

# The wheel pin (.duckdb-pypi-version) must be the build of the engine pin
# (.duckdb-version) and, while the workflows pin the engine by commit, of that
# commit: the engine is built from that wheel's sdist under the engine pin's
# label, and nothing in the source could contradict a wrong pair. Needs no build.
test-engine-pins:
    uv run test/integration/test_engine_pins.py

# `LOAD semantic_views` twice in one process must be idempotent (parser-hook
# re-registration guard, WR-09 D-21). Behavioral half of the structural test
# in tests/parser_hook_idempotent.rs — both must stay wired.
test-load-idempotent: build
    uv run test/integration/test_load_extension_twice_idempotent.py

# A DuckDB other than the one we build for must refuse `LOAD semantic_views`
# with the load-time version check's message, not crash (see src/load_guard.rs).
# Runs under DuckDB 1.5.6, the last 1.5 release: its C API is the one we declare
# (.duckdb-capi-version), so DuckDB's own loader accepts the binary and only our
# check refuses it. That host pin lives here and not in the script's PEP 723
# header on purpose: every header pin must equal .duckdb-pypi-version
# (tests::duckdb_derived_pins_agree) and the version monitor rewrites them all,
# while this one must differ from it. When the build moves to a new DuckDB, keep
# it on a release other than the new one.
test-load-version-guard: build
    uv run --with duckdb==1.5.6 test/integration/test_load_version_guard.py

# CREATE SEMANTIC VIEW ... FROM YAML FILE under the v0.10.0 read-elimination
# architecture (Phase 65 Plan 04 D-11): OR REPLACE / IF NOT EXISTS, friendly
# error surfaces, YAML size cap, transactional rollback, get_ddl round-trip.
test-yaml-file-create: build
    uv run test/integration/test_create_from_yaml_v010.py

# FF-3: the v0.1.0 companion-file migration must never touch an ATTACHed DB
# (in-memory primary + file-backed ATTACH used to migrate+delete its companion).
test-attach-migration: build
    uv run test/integration/test_attach_companion_migration.py

# CAT-1: the schema-scoping migration must record the CATALOG's spelling of a
# schema, not the one a legacy row carries -- otherwise CREATE OR REPLACE
# duplicates the row and reads keep returning the stale definition.
test-legacy-schema-spelling: build
    uv run test/integration/test_legacy_catalog_schema_spelling.py

# DROP/ALTER SEMANTIC VIEW on a fresh read-only DB that was never bootstrapped
# must surface the canonical "semantic view 'X' does not exist" wording, not a
# raw `_definitions` catalog leak (Phase 65.1 Plan 04, WR-03 D-18).
test-readonly-fresh-drop: build
    uv run test/integration/test_drop_on_fresh_readonly_clear_error.py

# Chunked-emission regression (MS-1, code-review 2026-07-02): bind-materialized
# TFs (list/show/describe family) must emit >2048-row result sets across
# multiple DataChunks instead of overflowing one chunk's capacity.
test-chunk-boundary: build
    uv run test/integration/test_chunk_boundary.py

# Differential harness (TC-2, code-review 2026-07-02): semantic_view() results
# vs independent hand-written SQL over a seeded randomized star schema, every
# bounded dims×metrics combination. R1 fix PRs extend it with the scenarios
# they repair.
test-differential: build
    uv run test/integration/test_differential.py

# SHOW ... IN SCHEMA / IN DATABASE must fold case (TECH-DEBT #25). The stored
# schema_name is stamped from current_schema(), which DuckDB returns as the
# spelling the caller last USE-d — so two views in one schema can be stamped
# differently and no single spelling of the filter returned both. Lives here
# rather than in sqllogictest because the assertion is on returned rows and
# list_semantic_views leads with a live created_on.
test-show-scope-case: build
    uv run test/integration/test_show_scope_case_insensitive.py

# SHOW ... IN SCHEMA <db>.<schema> — Snowflake's qualified scope form. It parsed
# and was then rejoined with a dot into `schema_name = 'db.schema'`, matching
# nothing with no error. Needs a second ATTACHed database to prove the database
# half is really applied (a last-part-only fix passes every single-DB test), so
# it cannot live in sqllogictest.
test-show-scope-qualified: build
    uv run test/integration/test_show_scope_qualified_name.py

# GET_DDL's third argument (use_fully_qualified_names) — the dump/restore round
# trip. The DDL is produced by one statement and must become the TEXT of a
# later one, which sqllogictest cannot express, so the actual replay (and the
# negative control showing the unqualified form relocating the view) lives here.
test-get-ddl-qualified: build
    uv run test/integration/test_get_ddl_qualified_name.py

# Query cancellation: con.interrupt() must stop a running semantic_view()
# query, not be noticed only once the inner query has finished. The inner
# query runs on a per-call Connection whose ClientContext holds its own
# interrupt flag, so this needs a real interrupt fired from a second thread
# mid-query — timing that sqllogictest cannot express.
test-interrupt: build
    uv run test/integration/test_query_interrupt.py

# Run every examples/*.py against the debug extension and fail on any error.
# Each example runs under `uv run --script`, so it gets the dependencies its own
# PEP 723 header declares (transactional_ddl.py: adbc-driver-manager, pyarrow).
# Nothing is skipped silently: scripts/run_examples.py prints PASS / FAIL /
# SKIPPED / XFAIL / XPASS for every file, and its two allow-lists are explicit:
#   - NEEDS_DOWNLOADED_EXTENSION (examples that download a DuckDB extension at
#     run time; none at present): run normally; SKIPPED loudly only with
#     SV_OFFLINE_EXTENSIONS=1, which is refused when CI is set;
#   - EXPECTED_FAILURES: a known failure must fail with its recorded error and
#     cite a TECH-DEBT entry; any other error is a FAIL.
# Pass example file names to run a subset: `just test-examples basic_ddl_and_query.py`.
# Run every examples/*.py against the debug extension (fails on any error)
test-examples *examples: build
    python3 scripts/run_examples.py {{examples}}

# Guards the sqllogictest wrapper (scripts/run_sqllogictest.py) that `just
# test-sql` runs through: a skipped `require semantic_views`, a file that
# executes zero statements, and a file the runner cannot parse must each fail
# the run, which the bare runner reports as success. Uses the configure/venv
# runner, so it needs `make configure` (done by `build`).
# Check that the sqllogictest wrapper fails runs that silently skip
test-sqllogictest-guards: build _ensure-test-deps
    make patch-runner
    configure/venv/bin/python test/integration/test_sqllogictest_guards.py

# Semantic-view DDL inside a multi-statement query string (TECH-DEBT #85) must
# fail with the "one statement per call" message rather than the misleading
# "parser_override is not active" one; the real override-off case keeps its
# message. Exercises the whole parser_override -> default parser ->
# parse_function chain, which a unit test cannot.
# Check the error for semantic-view DDL in a multi-statement string
test-multi-statement-error: build
    uv run test/integration/test_multi_statement_ddl_error.py

# All Python integration suites against the built extension. This is the
# exact set the `python-integration` CI job runs (IntegrationChecks.yml) —
# keep the two in sync by editing THIS recipe, not the workflow.
# (test-ducklake-ci is excluded: it has its own dedicated CI job.)
test-integration: test-engine-pins test-legacy-schema-spelling test-vtab-crash test-caret test-adbc test-adbc-queries test-large-view test-multi-db test-readonly test-concurrent test-load-idempotent test-load-version-guard test-yaml-file-create test-attach-migration test-readonly-fresh-drop test-chunk-boundary test-differential test-show-scope-case test-show-scope-qualified test-get-ddl-qualified test-interrupt test-examples test-sqllogictest-guards test-multi-statement-error

# Run all tests: Rust unit tests + SQL logic tests + DuckLake integration + all Python integration suites
# Note: test-iceberg requires `just setup-ducklake` first. test-ducklake-ci uses synthetic data.
# _ensure-test-deps runs early to catch pip version mismatches before slow builds.
test-all: _ensure-test-deps test-rust test-sql test-sql-grammar test-ducklake-ci test-integration

# Check that fuzz targets compile (requires nightly)
check-fuzz:
    cargo +nightly check --manifest-path fuzz/Cargo.toml

# Verify test/sql/TEST_LIST matches the .test files on disk. The sqllogictest
# runner executes ONLY files named in TEST_LIST, so a .test file missing from
# it is silently skipped — this check makes that a hard error (CI runs it too).
check-test-list:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! diff <(find test/sql -maxdepth 1 -name '*.test' | sort) <(sort test/sql/TEST_LIST); then
      echo "ERROR: test/sql/TEST_LIST is out of sync with test/sql/*.test" >&2
      echo "  (diff above: '<' = on disk but not in TEST_LIST, '>' = in TEST_LIST but not on disk)" >&2
      exit 1
    fi

# Verify the three places a fuzz target has to be registered agree:
# fuzz/fuzz_targets/*.rs on disk, the [[bin]] entries in fuzz/Cargo.toml, and
# the Fuzz.yml matrix. `just fuzz-all` derives its list from the manifest, but
# GitHub needs the matrix before any job starts, so that copy cannot be derived
# and is guarded here instead — the same shape as `check-test-list`.
#
# Pure text: no nightly, no cargo-fuzz, so it runs in the CodeQuality job
# alongside the TEST_LIST check.
check-fuzz-list:
    #!/usr/bin/env bash
    set -euo pipefail
    disk=$(find fuzz/fuzz_targets -maxdepth 1 -name '*.rs' -exec basename {} .rs \; | sort)
    manifest=$(sed -n 's|^path = "fuzz_targets/\(.*\)\.rs"$|\1|p' fuzz/Cargo.toml | sort)
    matrix=$(sed -n 's/^ *target: \[\(.*\)\]$/\1/p' .github/workflows/Fuzz.yml \
             | tr ',' '\n' | tr -d ' ' | grep . | sort)
    rc=0
    if ! diff <(echo "$disk") <(echo "$manifest"); then
      echo "ERROR: fuzz/fuzz_targets/*.rs and fuzz/Cargo.toml [[bin]] entries disagree" >&2
      echo "  ('<' = file on disk with no [[bin]] entry, '>' = [[bin]] entry with no file)" >&2
      rc=1
    fi
    if ! diff <(echo "$manifest") <(echo "$matrix"); then
      echo "ERROR: fuzz/Cargo.toml [[bin]] entries and the Fuzz.yml matrix disagree" >&2
      echo "  ('<' = target that CI never fuzzes, '>' = matrix entry with no target)" >&2
      rc=1
    fi
    exit $rc

# Run the full CI suite locally (lint + list-sync gates + test + fuzz check)
ci: lint check-test-list check-fuzz-list test-all check-fuzz docs-check

# Run a single fuzz target (default: fuzz_json_parse, 5 min timeout)
fuzz target="fuzz_json_parse" time="300":
    cargo +nightly fuzz run {{target}} fuzz/corpus/{{target}} fuzz/seeds/{{target}} -- -max_total_time={{time}}

# Run EVERY fuzz target sequentially (5 min each by default).
#
# The list is derived from `cargo fuzz list` — i.e. fuzz/Cargo.toml's [[bin]]
# entries — so adding a target picks it up here automatically. It used to be a
# hardcoded copy, and `fuzz_where_predicate` was left out of it from #172
# (2026-07-29) until 2026-08-04: CI fuzzed it on every push while `just
# fuzz-all` silently skipped it. `check-fuzz-list` guards the one copy that
# still has to be written by hand (the Fuzz.yml matrix, which GitHub needs
# before any job starts).
#
# The `mkdir -p` mirrors Fuzz.yml: libFuzzer refuses to start when the corpus /
# seeds directories are absent, and corpus/ is gitignored.
fuzz-all time="300":
    #!/usr/bin/env bash
    set -euo pipefail
    for t in $(cargo +nightly fuzz list); do
      echo "── $t ──"
      mkdir -p "fuzz/corpus/$t" "fuzz/seeds/$t"
      cargo +nightly fuzz run "$t" "fuzz/corpus/$t" "fuzz/seeds/$t" -- -max_total_time={{time}}
    done

# Minimize corpus for a fuzz target (removes inputs that don't add coverage)
fuzz-cmin target="fuzz_json_parse":
    cargo +nightly fuzz cmin {{target}}

# Rebuild the DuckDB engine the C++ shim links against, even when it is cached.
# `just build` builds it on first use and whenever the pins change, so this is
# only for a cache entry you no longer trust. The engine is the release pinned
# in .duckdb-version, built from the PyPI sdist of its `duckdb` package, or an
# engine source tree in ./duckdb when a CI harness supplied one (see
# scripts/ensure_engine.py). Cached under .engine/<id>/; cpp/engine.manifest
# points build.rs at it.
update-engine:
    @python3 scripts/ensure_engine.py --force

# Clean build artifacts
clean:
    make clean
    cargo clean

# Reclaim stale target artifacts older than 14 days without invalidating current build cache.
# Intended for milestone completion (see CLAUDE.md "Milestone Completion"). Auto-installs cargo-sweep on first run.
clean-stale:
    @command -v cargo-sweep >/dev/null 2>&1 || cargo install cargo-sweep
    @echo "Sweeping target/ artifacts not touched in 14 days..."
    cargo sweep --time 14
    @echo "Done. Current build cache preserved; next incremental build remains fast."

# Check Sphinx documentation builds without warnings (mirrors CI)
docs-check:
    uv run --project docs sphinx-build -b html -W docs docs/_build/html

# Build Sphinx documentation to docs/_build/html (clean build)
docs-build:
    rm -rf docs/_build
    uv run --project docs sphinx-build -b html docs docs/_build/html

# Serve docs locally with live-reload (http://127.0.0.1:8000)
docs-serve:
    uv run --project docs sphinx-autobuild docs docs/_build/html

# NOTE: sed -i '' is macOS-specific (BSD sed). This matches the dev environment (darwin).
# Set CE_REPO env var to override the CE fork path (default: ~/Documents/Dev/Sources/community-extensions).
# Automate CE registry release: update description.yml, copy to CE fork, open PR
release:
    #!/usr/bin/env bash
    set -euo pipefail
    # --- Precondition checks ---
    BRANCH=$(git branch --show-current)
    if [ "$BRANCH" != "main" ]; then
      echo "ERROR: must be on 'main' branch (currently on '$BRANCH')" >&2
      exit 1
    fi
    if ! command -v gh &>/dev/null; then
      echo "ERROR: 'gh' CLI not found. Install: https://cli.github.com/" >&2
      exit 1
    fi
    if ! git diff --quiet || ! git diff --cached --quiet; then
      echo "ERROR: working tree is not clean. Commit or stash changes first." >&2
      exit 1
    fi
    # Start from the latest main. A stale local main would get its push
    # rejected below, after description.yml was already committed.
    git fetch origin main
    if ! git merge --ff-only origin/main; then
      echo "ERROR: local main has diverged from origin/main. Reconcile them first." >&2
      exit 1
    fi
    # --- Extract values ---
    VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    if [ -z "$VERSION" ]; then
      echo "ERROR: could not extract version from Cargo.toml" >&2
      exit 1
    fi
    # --ff-only also passes when local main is ahead, and the push below would
    # publish those commits with the release. Only a re-run's own unpushed
    # description.yml commit may be there.
    python3 scripts/registry_target.py unpushed "$VERSION" origin/main
    # Take the release tag from origin when it is there (it may still be local
    # only). Just this tag: fetching all of them would stop on any unrelated
    # local tag that differs from origin's.
    if git ls-remote --exit-code --tags origin "refs/tags/v$VERSION" >/dev/null; then
      if ! git fetch origin "refs/tags/v$VERSION:refs/tags/v$VERSION"; then
        echo "ERROR: local tag v$VERSION differs from origin's. Delete it (git tag -d v$VERSION) and re-run." >&2
        exit 1
      fi
    fi
    # The tagged release, not HEAD: main may have moved on since the tag. The
    # tag must be on origin/main, or the registry would build an unmerged commit.
    SHA=$(python3 scripts/registry_target.py release-sha "$VERSION" origin/main)
    # --- Which description.yml key the registry builds our DuckDB pin from ---
    TMP=$(mktemp -d)
    gh api repos/duckdb/community-extensions/contents/.github/workflows/build.yml --jq '.content' | base64 -d > "$TMP/build.yml"
    gh api repos/duckdb/community-extensions/contents/scripts/build.py --jq '.content' | base64 -d > "$TMP/build.py"
    PIN=$(cat .duckdb-version)
    # The commit the pin's tag points at, for a registry default named by commit.
    PIN_COMMIT=$(git ls-remote --tags https://github.com/duckdb/duckdb \
      | awk -v t="refs/tags/$PIN" '$2==t{c=$1} $2==t"^{}"{p=$1} END{print (p?p:c)}')
    KEY=$(python3 scripts/registry_target.py key "$PIN" "$TMP/build.yml" "$TMP/build.py" "$PIN_COMMIT")
    rm -rf "$TMP"
    echo "Releasing v$VERSION (repo.$KEY: $SHA)"
    # --- Update description.yml in this repo ---
    python3 scripts/registry_target.py set description.yml "$KEY" "$SHA"
    sed -i '' "s/^  version: .*/  version: $VERSION/" description.yml
    git add description.yml
    # A re-run for the same release (e.g. to reopen the registry PR) finds
    # description.yml already up to date; go straight to the registry step.
    if git diff --cached --quiet; then
      echo "description.yml already pins v$VERSION; nothing to commit"
    else
      # registry_target.py unpushed recognises a re-run by this subject.
      git commit -m "chore: update description.yml for v$VERSION release"
    fi
    # Outside the if: a re-run after a failed push still has that commit to push.
    if [ -n "$(git rev-list origin/main..HEAD)" ]; then
      git push origin main
    fi
    echo "description.yml for v$VERSION is on origin/main"
    DESC_SRC="$PWD/description.yml"
    # --- Copy to CE fork and open PR ---
    CE_REPO="${CE_REPO:-$HOME/Documents/Dev/Sources/community-extensions}"
    if [ ! -d "$CE_REPO" ]; then
      echo "ERROR: CE fork directory not found at '$CE_REPO'" >&2
      echo "  Clone your fork of duckdb/community-extensions there, or set CE_REPO env var." >&2
      exit 1
    fi
    cd "$CE_REPO"
    # The CE fork must have a clean working tree: we rewrite its branch and
    # force-push below, which would fail partway (or carry stray edits) otherwise.
    if ! git diff --quiet || ! git diff --cached --quiet; then
      echo "ERROR: CE fork working tree at '$CE_REPO' is not clean. Commit or stash there first." >&2
      exit 1
    fi
    # Rebranch off CURRENT upstream main so the PR is a clean one-file change.
    # Reusing a stale semantic-views branch makes the PR diff *every* other
    # extension that updated upstream since the last release, which trips the
    # registry's "cannot have multiple descriptors changed" gate (fast-fails the
    # `prepare` job). Mirrors PublishExtension.yml. See MAINTAINER.md.
    UPSTREAM_URL="https://github.com/duckdb/community-extensions.git"
    if git remote get-url upstream &>/dev/null; then
      git remote set-url upstream "$UPSTREAM_URL"
    else
      git remote add upstream "$UPSTREAM_URL"
    fi
    git fetch upstream main
    # Refresh the remote-tracking ref so --force-with-lease below has a current
    # lease baseline (the branch is absent on the very first release -> ignore).
    git fetch origin semantic-views 2>/dev/null || true
    git checkout -B semantic-views upstream/main
    mkdir -p extensions/semantic_views
    cp "$DESC_SRC" extensions/semantic_views/description.yml
    git add extensions/semantic_views/description.yml
    git commit -m "Update semantic_views to v$VERSION"
    # --force-with-lease: reset the fork branch to our clean rebranch, but refuse
    # if someone else pushed to it since the fetch above.
    git push --force-with-lease origin semantic-views
    echo "Pushed clean semantic-views branch (rebased on upstream/main) to CE fork"
    # Derive the fork owner from the CE fork's origin remote so the recipe works
    # from any account/org fork, not just anentropic's.
    FORK_OWNER=$(basename "$(dirname "$(git remote get-url origin)")")
    HEAD_REF="$FORK_OWNER:semantic-views"
    # Open the PR, or report the existing open one (force-push updates it in place).
    PR_URL=$(gh pr list --repo duckdb/community-extensions \
      --head "$HEAD_REF" --base main --state open --json url --jq '.[0].url // empty')
    if [ -z "$PR_URL" ]; then
      PR_URL=$(gh pr create \
        --repo duckdb/community-extensions \
        --head "$HEAD_REF" \
        --base main \
        --title "Update semantic_views to v$VERSION" \
        --body "Update semantic_views extension to v$VERSION (ref: $SHA)")
    else
      echo "Existing open PR updated in place: $PR_URL"
    fi
    echo ""
    echo "=== Release Summary ==="
    echo "  Version: v$VERSION"
    echo "  SHA:     $SHA"
    echo "  PR:      $PR_URL"
