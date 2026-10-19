.PHONY: clean clean_all

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXTENSION_NAME=semantic_views

# Two DuckDB versions matter, and since DuckDB 2.0 they differ:
#
# TARGET_DUCKDB_VERSION is the C API version the extension is built against
# (.duckdb-capi-version). extension-ci-tools stamps it into the binary's
# metadata, and duckdb-rs declares it to the loader; DuckDB 2.0 loads a C_STRUCT
# extension pinned to the v1.5.6 C API (see UNSTABLE_C_API_FLAG below). The name
# is extension-ci-tools', which reads it.
TARGET_DUCKDB_VERSION=$(shell cat .duckdb-capi-version)
#
# ENGINE_DUCKDB_VERSION is the DuckDB release the extension runs in
# (.duckdb-version): the C++ shim is compiled against that engine, and the
# extension refuses to load anywhere else.
ENGINE_DUCKDB_VERSION=$(shell cat .duckdb-version)
# Its `duckdb` PyPI release: the version without its `v`, unless
# .duckdb-pypi-version names a pre-release wheel (2.0.0.devN for an alpha).
ENGINE_PYPI_VERSION=$(shell cat .duckdb-pypi-version 2>/dev/null || sed 's/^v//' .duckdb-version)

# Pin the test-runner DuckDB pip package to the engine the extension is built for.
# base.Makefile defaults to latest PyPI.
#
# Scoped to builds against the upstream release we target. A downstream
# distribution builds community extensions against its own engine, and its
# ci-tools fork installs its own runner package under its own versioning
# (Haybarn: `haybarn-cli`, published as pre-releases only). Pinning our release
# number there asks pip for a version that exists in no index and fails `make
# configure` on every platform, so leave the choice to the harness.
#
# DUCKDB_GIT_VERSION is the engine ref being built against: empty locally,
# `vX.Y.Z` or a commit in upstream CI, `haybarn-v1.5.5-rc1` (or `main`, for a
# nightly) when it is something else. DUCKDB_TAG, when set, is the release label
# the harness gave that commit (extension-ci-tools' `duckdb_tag` input; see
# set_duckdb_tag below), and names the engine instead. `?=` so an explicit value
# from the environment wins either way.
SV_ENGINE_REF := $(or $(DUCKDB_TAG),$(DUCKDB_GIT_VERSION),$(ENGINE_DUCKDB_VERSION))
ifeq ($(SV_ENGINE_REF),$(ENGINE_DUCKDB_VERSION))
DUCKDB_TEST_VERSION ?= $(ENGINE_PYPI_VERSION)
endif

all: configure debug

# Include makefiles from DuckDB
include extension-ci-tools/makefiles/c_api_extensions/base.Makefile
include extension-ci-tools/makefiles/c_api_extensions/rust.Makefile

# Set UNSTABLE_C_API_FLAG AFTER the include (base.Makefile resets it).
# C_STRUCT ABI pinned to the TARGET_DUCKDB_VERSION (v1.5.6) C API, with a C++ helper for
# the parser hooks (Option A): Rust owns the entry point
# (semantic_views_init_c_api), the C++ helper registers the hooks.
#
# Not C_STRUCT_UNSTABLE: DuckDB 2.0 loads an unstable-ABI extension only through
# its new `_init_c_api_v2` entry point, while a C_STRUCT one pinned to the v1.5.6
# C API loads through the entry point we already have. The cost is that DuckDB
# no longer refuses a host whose version differs from ours. Our C++ reads host
# objects by layout, so it needs that exact match, and the extension checks it
# itself at load time (sv_compiled_duckdb_version in cpp/src/shim.cpp, compared
# in src/lib.rs).
UNSTABLE_C_API_FLAG=--abi-type C_STRUCT

# Run sqllogictest through a strict wrapper instead of the bare
# `python -m duckdb_sqllogictest` base.Makefile sets (overridden AFTER the
# include, and BEFORE the `:=` TEST_RUNNER_FILE_LIST_* definitions below, which
# capture it). The bare runner exits 0 when `require semantic_views` is
# SKIPPED -- a missing or renamed extension file skipped all 115 files and
# printed "115 test files passed" -- and also when a file cannot be parsed or
# runs zero statements. scripts/run_sqllogictest.py takes the same arguments
# and fails each of those; test/integration/test_sqllogictest_guards.py
# (`just test-sqllogictest-guards`) pins every case. The extension-ci-tools
# submodule is left untouched.
#
# The wrapper also fails any file that hits an error the runner silently
# ignores ("HTTP" / "Unable to connect" count as a PASS upstream), under
# SV_OFFLINE_EXTENSIONS=1 too. Offline runs excuse nothing here: the files in
# OFFLINE_EXTENSION_TESTS never reach the wrapper, because prepare_test_list
# below leaves them out of the list and prints their SKIPPED lines. (The
# wrapper's own --offline-excused flag is for running it directly.)
TEST_RUNNER=$(SV_CRASH_REPORT) $(PYTHON_VENV_BIN) scripts/run_sqllogictest.py --required-extension semantic_views

# On Windows a crash of the runner (an access violation) is re-run under cdb so
# the log shows the native stack and the faulting module; the exit status stays
# the crash's. See scripts/windows_crash_report.py. Empty elsewhere.
ifeq ($(OS),Windows_NT)
SV_CRASH_REPORT=$(PYTHON_VENV_BIN) scripts/windows_crash_report.py --
else
SV_CRASH_REPORT=
endif

# The DuckDB engine the C++ shim is compiled and linked against. build.rs links a
# static build of it INTO the extension binary (the parser hook the shim
# registers is C++ internals with no C-API equivalent), so it is an ABI contract
# with whatever engine loads the result -- the extension refuses any other.
#
# ENGINE_SRC_DIR: an engine source tree supplied by the harness. A distribution
#   that builds community extensions against its own engine clones it into
#   ./duckdb (upstream community-extensions and Haybarn's fork both do); when it
#   is there, that IS the engine the extension will be loaded into, so it is
#   built as the harness labels it.
# Otherwise the release pinned in .duckdb-version is built from the PyPI sdist of
#   the matching `duckdb` package. Gitignored, cached under .engine/<id>/ so it
#   survives branch switches.
#
# See scripts/ensure_engine.py for the selection rules and the cache layout.
ENGINE_SRC_DIR ?= duckdb

.PHONY: ensure_engine
ensure_engine:
	@$(PYTHON_BIN) scripts/ensure_engine.py --src-dir "$(ENGINE_SRC_DIR)"

# extension-ci-tools runs this after cloning the engine into ./duckdb when the
# caller passes `duckdb_tag` (base.Makefile's own rule does nothing for C API
# extensions). Our build workflows pass it while the engine we target is a
# pre-release with no release tag upstream: `duckdb_version` names its commit,
# and this tag gives the clone the release label (.duckdb-version), which
# scripts/ensure_engine.py then builds it under, as the wheel was.
# A re-run in the same clone finds the tag already there; it is left alone if
# it is at HEAD, and the run fails rather than move a tag that is not.
set_duckdb_tag:
	cd duckdb && if git rev-parse -q --verify "refs/tags/$(DUCKDB_TAG)" >/dev/null; then \
	  test "$$(git rev-parse "$(DUCKDB_TAG)^{commit}")" = "$$(git rev-parse HEAD)" \
	    || { echo "set_duckdb_tag: $(DUCKDB_TAG) exists and is not HEAD" >&2; exit 1; }; \
	else git tag $(DUCKDB_TAG); fi

configure: venv platform extension_version

debug: build_extension_library_debug build_extension_with_metadata_debug
release: build_extension_library_release build_extension_with_metadata_release

test: test_debug
test_debug: test_extension_debug
test_release: test_extension_release

clean: clean_build clean_rust
clean_all: clean_configure clean

# Override Rust build targets to pass --no-default-features --features extension.
#
# The `default` feature enables duckdb/bundled, which compiles DuckDB from source
# and links it into the binary — this is used for `cargo test` to enable
# Connection::open_in_memory() in unit tests.
#
# For the actual DuckDB extension binary we do NOT want bundled DuckDB;
# the extension must use the function-pointer stubs (duckdb/loadable-extension)
# so that it links against the DuckDB that loads it at runtime.
# --no-default-features removes duckdb/bundled; --features extension adds
# duckdb/loadable-extension + duckdb/vscalar.
build_extension_library_debug: check_configure ensure_engine
	DUCKDB_EXTENSION_NAME=$(EXTENSION_NAME) DUCKDB_EXTENSION_MIN_DUCKDB_VERSION=$(TARGET_DUCKDB_VERSION) cargo build $(CARGO_OVERRIDE_DUCKDB_RS_FLAG) $(TARGET_INFO) --no-default-features --features extension
	$(PYTHON_VENV_BIN) -c "from pathlib import Path;Path('$(EXTENSION_BUILD_PATH)/debug/extension/$(EXTENSION_NAME)').mkdir(parents=True, exist_ok=True)"
	$(PYTHON_VENV_BIN) -c "import shutil;shutil.copyfile('$(TARGET_PATH)/debug$(IS_EXAMPLE)/$(RUST_LIBNAME)', '$(EXTENSION_BUILD_PATH)/debug/$(EXTENSION_LIB_FILENAME)')"

build_extension_library_release: check_configure ensure_engine
	DUCKDB_EXTENSION_NAME=$(EXTENSION_NAME) DUCKDB_EXTENSION_MIN_DUCKDB_VERSION=$(TARGET_DUCKDB_VERSION) cargo build $(CARGO_OVERRIDE_DUCKDB_RS_FLAG) --release $(TARGET_INFO) --no-default-features --features extension
	$(PYTHON_VENV_BIN) -c "from pathlib import Path;Path('$(EXTENSION_BUILD_PATH)/release/extension/$(EXTENSION_NAME)').mkdir(parents=True, exist_ok=True)"
	$(PYTHON_VENV_BIN) -c "import shutil;shutil.copyfile('$(TARGET_PATH)/release$(IS_EXAMPLE)/$(RUST_LIBNAME)', '$(EXTENSION_BUILD_PATH)/release/$(EXTENSION_LIB_FILENAME)')"

# Patch installed duckdb_sqllogictest to add notwindows/windows platform detection.
# Idempotent. Remove once extension-ci-tools updates its pinned sqllogictest commit.
.PHONY: patch-runner
patch-runner: check_configure
	@$(PYTHON_VENV_BIN) scripts/patch_sqllogictest.py

# Use an explicit file list to control which tests run.
# test/sql/TEST_LIST enumerates the tests that are stable with the Python
# sqllogictest runner + external extension. phase2_restart.test is excluded
# because the Python runner cannot reload an external extension after the
# `restart` directive in a file-backed database (the extension's init_catalog
# may deadlock during reload). Restart persistence is verified separately via
# `cargo test` (catalog::tests::tc6_restart_persistence_survives_reopen:
# open -> persist -> close -> reopen -> init_catalog -> lookup).
TEST_LIST_PATH := test/sql/TEST_LIST

# Offline mode for sandboxes that cannot reach extensions.duckdb.org (LOCAL USE
# ONLY). A test that needs a DuckDB extension that is neither bundled in the pip
# `duckdb` wheel nor in this repo makes DuckDB download it on first use. Behind
# a proxy that blocks the download, such a file fails and, because the runner
# stops at the first failing file, halts the whole suite. List those files in
# OFFLINE_EXTENSION_TESTS: with SV_OFFLINE_EXTENSIONS=1 they are left out of the
# run and each is reported with a loud SKIPPED line; every other file still runs.
# None needs one today. test/sql/peg_compat.test did while it called
# `enable_peg_parser()` (which autoloads `autocomplete`); DuckDB 2.0 removed it.
#
# Deliberately an explicit opt-in, NOT a sqllogictest `require autocomplete`:
# `require` skips silently whenever the extension cannot be loaded, CI
# included, so a broken download in CI would turn this coverage off without a
# red. Without the variable nothing changes -- CI runs the file and fails hard
# if it cannot load the extension -- and the variable is refused outright when
# `CI` is set (GitHub Actions sets CI=true), so it cannot leak into CI.
OFFLINE_EXTENSION_TESTS :=
ifeq ($(SV_OFFLINE_EXTENSIONS),1)
ifneq ($(CI),)
$(error SV_OFFLINE_EXTENSIONS=1 skips tests and is for local offline runs only; refusing because CI='$(CI)')
endif
EFFECTIVE_TEST_LIST := build/test_list_offline
else
EFFECTIVE_TEST_LIST := $(TEST_LIST_PATH)
endif

# Writes $(EFFECTIVE_TEST_LIST) in offline mode and prints one SKIPPED line per
# left-out file. A no-op otherwise.
.PHONY: prepare_test_list
prepare_test_list:
ifeq ($(SV_OFFLINE_EXTENSIONS),1)
	@mkdir -p $(dir $(EFFECTIVE_TEST_LIST))
	@$(if $(OFFLINE_EXTENSION_TESTS),grep -vxF $(foreach t,$(OFFLINE_EXTENSION_TESTS),-e $(t)),cat) $(TEST_LIST_PATH) > $(EFFECTIVE_TEST_LIST)
	@for t in $(OFFLINE_EXTENSION_TESTS); do \
	  echo "SKIPPED (SV_OFFLINE_EXTENSIONS=1): $$t -- needs a DuckDB extension downloaded from extensions.duckdb.org; NOT run"; \
	done
endif

# --test-dir is passed alongside --file-list so the runner can resolve __TEST_DIR__
# (used by tests that create file-backed databases). The file list controls which
# tests are actually executed.
TEST_RUNNER_FILE_LIST_DEBUG  := $(TEST_RUNNER) --test-dir test/sql --file-list $(EFFECTIVE_TEST_LIST) $(EXTRA_EXTENSIONS_PARAM) --external-extension build/debug/$(EXTENSION_NAME).duckdb_extension
TEST_RUNNER_FILE_LIST_RELEASE := $(TEST_RUNNER) --test-dir test/sql --file-list $(EFFECTIVE_TEST_LIST) $(EXTRA_EXTENSIONS_PARAM) --external-extension build/release/$(EXTENSION_NAME).duckdb_extension

# Override base.Makefile test targets to patch runner before tests.
# SKIP_TESTS platforms (musl, mingw) resolve to tests_skipped before reaching these
# targets, so patch-runner is never called on those platforms — which is correct.
#
# HISTORY (TC-10): these targets used to run each .test file in its OWN
# sqllogictest process. DuckDB 1.5.0 changed the parser-extension lifecycle
# (ExtensionCallbackManager) such that one process creating and destroying
# several databases in sequence segfaulted, so the per-file loop was the only
# way to get a green suite. `probe_isolation_debug_internal` existed to detect
# the upstream fix rather than let the workaround outlive its cause.
#
# The probe fired on DuckDB v1.5.5 (2026-08-09): the single-process run no
# longer crashes. Both targets are back on the single-process
# TEST_RUNNER_FILE_LIST_* path, which is simpler and stops spawning one process
# per file. Evidence: 3/3 local runs at 111/111 SUCCESS (~33 s vs ~39 s for the
# loop), plus a workflow_dispatch BuildAll run confirming windows_amd64 and
# osx_arm64 — the two platforms that actually execute the release suite.
#
# If the crash ever returns, the loop is in this file's history; restore it
# along with the probe rather than papering over an intermittent segfault.
#
# The `@echo` after each run is deliberate: without it a passing single-process
# run prints only per-file SUCCESS lines, and the "did the suite actually run,
# or did it silently skip?" question this project has been bitten by twice
# (see MAINTAINER.md, CI Workflows) needs a greppable end-of-run marker.
test_extension_debug_internal: patch-runner prepare_test_list
	@echo "Running DEBUG tests.."
	@$(TEST_RUNNER_FILE_LIST_DEBUG)
	@echo "$$(wc -l < $(EFFECTIVE_TEST_LIST) | tr -d ' ') test files passed (single process)$(if $(filter 1,$(SV_OFFLINE_EXTENSIONS)), -- $(words $(OFFLINE_EXTENSION_TESTS)) SKIPPED offline (SV_OFFLINE_EXTENSIONS=1),)"

test_extension_release_internal: patch-runner prepare_test_list
	@echo "Running RELEASE tests.."
	@$(TEST_RUNNER_FILE_LIST_RELEASE)
	@echo "$$(wc -l < $(EFFECTIVE_TEST_LIST) | tr -d ' ') test files passed (single process)$(if $(filter 1,$(SV_OFFLINE_EXTENSIONS)), -- $(words $(OFFLINE_EXTENSION_TESTS)) SKIPPED offline (SV_OFFLINE_EXTENSIONS=1),)"

