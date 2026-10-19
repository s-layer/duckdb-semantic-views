#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
Read-only database LOAD regression tests for the semantic_views extension.

Phase 63 (v0.9.0) adds support for `LOAD semantic_views` against a read-only
DuckDB database. Prior to this milestone, LOAD failed at init_catalog's
`CREATE SCHEMA IF NOT EXISTS semantic_layer` with DuckDB's read-only error,
making it impossible to query a previously-defined semantic view from a
read-only DB (Iceberg snapshots, archival files, etc.).

Test scenarios (cover RO-01 through RO-05):
  (a) test_fresh_readonly_empty_list      — fresh read-only file, no
      _definitions table; LOAD succeeds; list_semantic_views() -> [];
      missing-view lookups produce a graceful "does not exist" / "not found"
      error (substring match — see RESEARCH §4 acceptance flexibility).
      Covers: RO-01, RO-03, RO-04.
  (b) test_bootstrapped_readonly_query_works — bootstrap a view writable,
      close, reopen read-only; LOAD succeeds; list/describe/semantic_view
      all return correct results.
      Covers: RO-01, RO-02.
  (c) test_readonly_ddl_fails             — bootstrapped read-only DB;
      CREATE/DROP/ALTER each fail with DuckDB's "read-only" error.
      Covers: RO-05.

Note on bootstrap-then-RO pattern: scenarios (b) and (c) bootstrap the DB
in a SUBPROCESS rather than the main process. Phase 62's OverrideContext
attaches a long-lived catalog connection to DBConfig that keeps the
DuckDB Database alive past the user's connection close, which prevents
reopening the same DB read-only in the SAME process (the open hangs on
the in-process database manager). Real users separate bootstrap and
read-only phases across process boundaries (CI build vs query runtime),
so the subprocess pattern reflects realistic usage. The same-process
RW-then-RO regression is logged in deferred-items.md as a follow-up
(Phase 64+ candidate).

Exit codes:
    0 = all tests passed
    1 = at least one test failed
"""

import gc
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path


EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()


def open_writable(db_path: str):
    """Open a writable DuckDB connection with the extension installed and loaded."""
    import duckdb

    conn = duckdb.connect(
        db_path,
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": EXT_DIR,
        },
    )
    conn.execute(f"FORCE INSTALL '{EXT_PATH}'")
    conn.execute("LOAD semantic_views")
    return conn


def open_readonly(db_path: str):
    """Open a read-only DuckDB connection and LOAD the extension.

    Note: FORCE INSTALL is unnecessary here — the extension is already
    installed by open_writable() in the same EXT_DIR cache.
    """
    import duckdb

    conn = duckdb.connect(
        db_path,
        read_only=True,
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": EXT_DIR,
        },
    )
    # Phase 63: this is the line under test. Pre-fix, this raised
    # `Cannot execute statement of type "CREATE_SCHEMA" ... in read-only mode`.
    conn.execute("LOAD semantic_views")
    return conn


def bootstrap_in_subprocess(db_path: str, ddl_statements: list[str]) -> None:
    """Bootstrap a DuckDB file in a subprocess so the parent stays clean.

    Phase 62's OverrideContext is attached per-DBConfig and keeps the
    catalog connection alive until process exit (intentional leak — see
    Phase 62 RESEARCH §Q2). After loading the extension into a writable
    in-process connection, the same DB cannot be reopened read-only in
    the SAME process (the open hangs because the Database is still
    referenced). Running bootstrap in a subprocess sidesteps this by
    letting the OS reclaim the DBConfig at process exit, releasing the
    file lock so the parent can reopen RO cleanly. This mirrors real
    deployments where bootstrap (CI/build job) and read-only query
    (production worker) are different processes.
    """
    script_lines = [
        "import duckdb",
        f"conn = duckdb.connect({db_path!r}, config={{"
        f'"allow_unsigned_extensions": "true", '
        f'"extension_directory": {str(EXT_DIR)!r}}})',
        f"conn.execute(\"FORCE INSTALL '{str(EXT_PATH)}'\")",
        "conn.execute('LOAD semantic_views')",
    ]
    for stmt in ddl_statements:
        script_lines.append(f"conn.execute({stmt!r})")
    script_lines.append("conn.close()")
    script = "\n".join(script_lines) + "\n"
    result = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"bootstrap subprocess failed (exit={result.returncode}):\n"
            f"STDOUT: {result.stdout}\nSTDERR: {result.stderr}"
        )


def _connect_with_watchdog(path: str, watchdog_seconds: float = 5.0, **kwargs):
    """Open a DuckDB connection inside a daemon thread with a wall-clock budget.

    Phase 65 (v0.9.1) — root cause: the extension's long-lived `catalog_conn`
    / `query_conn` (opened in `init_extension` at `src/lib.rs:493-508`) hold
    `shared_ptr<DatabaseInstance>` references that survive the caller's
    `close()`. The next in-process `duckdb.connect(path, read_only=True)` (or
    the reverse direction) busy-spins forever in
    `DBInstanceCache::GetInstanceInternal` (`cpp/include/duckdb.cpp:278022-278024`,
    `while (!weak_cache_entry.expired())` — see Phase 65 RESEARCH §2 and
    `65-01-SPIKES.md` for the captured lldb backtrace).

    The busy-spin is on CPU (uninterruptible from Python — there is no
    safe point inside the tight C++ loop where the GIL could be re-acquired
    or a signal handler could fire). The daemon thread approach lets the
    test thread fail-fast on a 5 s wall-clock budget instead of hanging the
    whole suite. **Caveat: on the v0.9.0 baseline the daemon thread leaks
    for the rest of the process lifetime** — it cannot be killed. Acceptable
    for a fail-once regression test; the B1..B4 + B11 tests below are
    therefore registered LAST in `main()` so the leaked thread does not
    interfere with earlier subprocess-style tests (B5 group). After Plans
    02/03 land the busy-spin is removed and these threads exit cleanly.

    Returns `(connection, elapsed_seconds)` on success. Raises:
      * `TimeoutError` if the `duckdb.connect` call did not return within
        `watchdog_seconds` (the Phase 65 regression signal).
      * Any exception raised by `duckdb.connect` itself (re-raised on
        the calling thread).
    """
    result: dict = {"conn": None, "exc": None}

    def _do():
        try:
            import duckdb as _ddb

            result["conn"] = _ddb.connect(path, **kwargs)
        except BaseException as e:  # noqa: BLE001 — must capture KeyboardInterrupt too
            result["exc"] = e

    t = threading.Thread(target=_do, daemon=True)
    start = time.monotonic()
    t.start()
    t.join(timeout=watchdog_seconds)
    elapsed = time.monotonic() - start
    if t.is_alive():
        # Phase 65: busy-spin in C++ — we cannot kill the thread, but
        # marking it daemon means it dies at process exit.
        raise TimeoutError(
            f"duckdb.connect({path!r}, **{kwargs!r}) did not return within "
            f"{watchdog_seconds}s — likely the in-process RW<->RO busy-spin "
            f"in DBInstanceCache::GetInstanceInternal (Phase 65 regression). "
            f"See 65-01-SPIKES.md A4 for the diagnosis."
        )
    if result["exc"] is not None:
        raise result["exc"]
    return result["conn"], elapsed


# Standard config passed to duckdb.connect for the in-process tests. Mirrors
# open_writable / open_readonly so the watchdog tests load the same extension
# build.
def _connect_config():
    return {
        "allow_unsigned_extensions": "true",
        "extension_directory": str(EXT_DIR),
    }


# Minimal valid CREATE SEMANTIC VIEW used across B1, B2, B11. Kept inline to
# avoid coupling test data to fixtures.
def _minimal_create_sql(view_name: str = "v") -> str:
    return (
        f"CREATE SEMANTIC VIEW {view_name} AS "
        f"  TABLES (t1 AS t PRIMARY KEY (i)) "
        f"  DIMENSIONS (t1.i AS t1.i) "
        f"  METRICS (t1.c AS COUNT(*))"
    )


def run_test(name, test_fn):
    print(f"\n{'=' * 60}")
    print(f"TEST: {name}")
    print(f"{'=' * 60}")
    try:
        test_fn()
        print("  RESULT: PASS")
        return True
    except AssertionError as e:
        print(f"  RESULT: FAIL\n  {e}")
        return False
    except Exception as e:
        print(f"  RESULT: ERROR\n  {type(e).__name__}: {e}")
        traceback.print_exc()
        return False


def _missing_view_error_substrings():
    """Acceptable substrings for "view not found" errors across reader paths.

    Per RESEARCH §4 (Coverage strategy — RO-05 acceptance flexibility),
    tests must not pin to one DuckDB error sentence. The reader paths use
    two distinct phrasings:
      - `describe_semantic_view`, SHOW commands, GET_DDL: "does not exist"
        (10 sites confirmed in src/ddl/* — see RESEARCH §3 Q4).
      - `semantic_view(...)` table function (src/query/error.rs:42):
        "Semantic view 'X' not found."
    Both are acceptable as a graceful catalog-miss surfacing.
    """
    return ("does not exist", "not found")


def _assert_missing_view_error(err_msg: str):
    subs = _missing_view_error_substrings()
    assert any(s in err_msg for s in subs), (
        f"expected one of {subs} in error, got: {err_msg}"
    )


def test_fresh_readonly_empty_list():
    """RO-01 / RO-03 / RO-04: fresh read-only DB, no _definitions table."""
    import duckdb

    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "fresh.duckdb")
        # RESEARCH §9 Risk 5: empty zero-byte files cannot be opened
        # read-only. Bootstrap a valid DuckDB header by opening writable
        # and closing — but do NOT load the extension, so semantic_layer
        # schema is never created.
        bootstrap = duckdb.connect(db)
        bootstrap.execute("SELECT 1")
        bootstrap.close()

        ro = open_readonly(db)
        try:
            # RO-03: list_semantic_views() returns empty (not a catalog error).
            rows = ro.execute("SELECT name FROM list_semantic_views()").fetchall()
            assert rows == [], f"expected empty list, got: {rows}"

            # RO-04: describe_semantic_view('missing') -> graceful catalog miss.
            try:
                ro.execute("FROM describe_semantic_view('missing')").fetchall()
                raise AssertionError("describe_semantic_view should have failed for missing view")
            except duckdb.Error as e:
                _assert_missing_view_error(str(e))

            # RO-04 sub: FROM semantic_view('missing', ...) -> graceful catalog miss.
            try:
                ro.execute(
                    "SELECT * FROM semantic_view('missing', dimensions := ['x'])"
                ).fetchall()
                raise AssertionError("semantic_view should have failed for missing view")
            except duckdb.Error as e:
                _assert_missing_view_error(str(e))
        finally:
            ro.close()


def test_bootstrapped_readonly_query_works():
    """RO-01 / RO-02: bootstrap writable, reopen read-only, query works.

    Bootstraps in a SUBPROCESS so the parent's in-process DBConfig leak
    (Phase 62 OverrideContext) doesn't keep the file locked. See module
    docstring + bootstrap_in_subprocess() for full rationale.
    """
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "bootstrapped.duckdb")
        bootstrap_in_subprocess(
            db,
            [
                "CREATE TABLE orders ("
                "  id INTEGER PRIMARY KEY,"
                "  region VARCHAR,"
                "  amount DECIMAL(10,2)"
                ")",
                "INSERT INTO orders VALUES (1, 'US', 100), (2, 'EU', 200), (3, 'US', 50)",
                "CREATE SEMANTIC VIEW v AS "
                "  TABLES (o AS orders PRIMARY KEY (id)) "
                "  DIMENSIONS (o.region AS o.region) "
                "  METRICS (o.total AS SUM(o.amount))",
            ],
        )

        ro = open_readonly(db)
        try:
            # RO-02: list_semantic_views() returns the bootstrapped view.
            names = [r[0] for r in ro.execute(
                "SELECT name FROM list_semantic_views()"
            ).fetchall()]
            assert names == ["v"], f"expected ['v'], got: {names}"

            # RO-02: describe_semantic_view returns rows.
            desc = ro.execute("FROM describe_semantic_view('v')").fetchall()
            assert len(desc) > 0, "describe_semantic_view should return metadata rows"

            # RO-02: semantic_view returns aggregated rows.
            rows = ro.execute(
                "SELECT region, total FROM semantic_view("
                "  'v', dimensions := ['region'], metrics := ['total']"
                ") ORDER BY region"
            ).fetchall()
            regions = {r[0] for r in rows}
            assert regions == {"EU", "US"}, f"expected {{EU, US}}, got: {regions}"
        finally:
            ro.close()


def test_readonly_ddl_fails():
    """RO-05: CREATE / DROP / ALTER on bootstrapped read-only DB -> DuckDB read-only error.

    Bootstraps in a SUBPROCESS so the parent's in-process DBConfig leak
    (Phase 62 OverrideContext) doesn't keep the file locked. See module
    docstring + bootstrap_in_subprocess() for full rationale.
    """
    import duckdb

    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "ddl_ro.duckdb")
        bootstrap_in_subprocess(
            db,
            [
                "CREATE TABLE orders (id INTEGER PRIMARY KEY, amount DECIMAL(10,2))",
                "CREATE SEMANTIC VIEW v AS "
                "  TABLES (o AS orders PRIMARY KEY (id)) "
                "  DIMENSIONS (o.id AS o.id) "
                "  METRICS (o.t AS SUM(o.amount))",
            ],
        )

        ro = open_readonly(db)
        try:
            # RO-05: DROP on existing view → DELETE on caller fails read-only.
            try:
                ro.execute("DROP SEMANTIC VIEW v")
                raise AssertionError("DROP should fail on read-only DB")
            except duckdb.Error as e:
                msg = str(e).lower()
                assert "read-only" in msg, f"expected 'read-only' substring, got: {e}"

            # RO-05: ALTER RENAME on existing view → UPDATE fails read-only.
            try:
                ro.execute("ALTER SEMANTIC VIEW v RENAME TO w")
                raise AssertionError("ALTER should fail on read-only DB")
            except duckdb.Error as e:
                msg = str(e).lower()
                assert "read-only" in msg, f"expected 'read-only' substring, got: {e}"

            # RO-05: ALTER SET COMMENT on existing view → UPDATE fails read-only.
            try:
                ro.execute("ALTER SEMANTIC VIEW v SET COMMENT = 'hi'")
                raise AssertionError("ALTER SET COMMENT should fail on read-only DB")
            except duckdb.Error as e:
                msg = str(e).lower()
                assert "read-only" in msg, f"expected 'read-only' substring, got: {e}"

            # RO-05: CREATE new view → INSERT fails read-only.
            try:
                ro.execute(
                    "CREATE SEMANTIC VIEW w AS "
                    "  TABLES (o AS orders PRIMARY KEY (id)) "
                    "  DIMENSIONS (o.id AS o.id) "
                    "  METRICS (o.c AS COUNT(*))"
                )
                raise AssertionError("CREATE should fail on read-only DB")
            except duckdb.Error as e:
                msg = str(e).lower()
                assert "read-only" in msg, f"expected 'read-only' substring, got: {e}"
        finally:
            ro.close()


# ---------------------------------------------------------------------------
# Phase 65 in-process regression tests (LIFE-01 / LIFE-03 / D-09)
# ---------------------------------------------------------------------------
#
# These five tests are EXPECTED TO FAIL on the v0.9.0 baseline of
# milestone/v0.9.1 — they are the "fails on baseline" half of LIFE-03's
# success criterion 3 ("fails on v0.9.0 baseline AND passes on v0.9.1").
# Plans 02/03 remove the OverrideContext / QueryState long-lived
# duckdb_connection leak; Plan 04 reruns this suite to flip them green.
#
# Each test uses `_connect_with_watchdog` so the suite fails fast on a 5 s
# budget rather than hanging the runner forever in the C++ busy-spin.
# See `_connect_with_watchdog` docstring for the thread-leak caveat and
# the rationale for registering these tests LAST in main().


def test_in_process_bootstrap_then_readonly_fresh():
    """B1 (LIFE-01): freshly bootstrapped DB → close → RO reopen in same process.

    Bootstrap an empty DuckDB file in the SAME process (writable connection
    via `open_writable`), define a minimal semantic view, close, gc, then
    reopen read-only via `_connect_with_watchdog`. On v0.9.0 baseline this
    raises TimeoutError after 5 s (the busy-spin). On v0.9.1 it returns
    within milliseconds and `list_semantic_views()` returns the bootstrapped
    view.
    """
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "fresh.duckdb")
        w = open_writable(db)
        w.execute("CREATE TABLE t (i INT)")
        w.execute(_minimal_create_sql("v"))
        w.close()
        del w
        gc.collect()

        ro, elapsed = _connect_with_watchdog(
            db,
            watchdog_seconds=5.0,
            read_only=True,
            config=_connect_config(),
        )
        try:
            assert elapsed < 5.0, f"RO reopen took {elapsed:.2f}s (>=5.0)"
            ro.execute("LOAD semantic_views")
            names = [
                r[0]
                for r in ro.execute(
                    "SELECT name FROM list_semantic_views()"
                ).fetchall()
            ]
            assert names == ["v"], f"expected ['v'], got: {names}"
        finally:
            ro.close()


def test_in_process_bootstrap_then_readonly_existing():
    """B2 (LIFE-01): previously-bootstrapped DB → in-process RW LOAD → close → RO reopen.

    Bootstrap via the EXISTING subprocess helper (so the file is built
    cleanly), then in-process open writable, execute LOAD only, close,
    gc, and try to reopen read-only. Distinct from B1 because the
    extension state we leak here came from the in-process LOAD on an
    already-populated DB rather than from a CREATE in the same session.
    """
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "existing.duckdb")
        bootstrap_in_subprocess(
            db,
            [
                "CREATE TABLE t (i INT)",
                _minimal_create_sql("v"),
            ],
        )

        w = open_writable(db)
        # LOAD has already been executed inside open_writable; touching
        # the DB once more keeps the connection live until close().
        w.execute("SELECT 1").fetchall()
        w.close()
        del w
        gc.collect()

        ro, elapsed = _connect_with_watchdog(
            db,
            watchdog_seconds=5.0,
            read_only=True,
            config=_connect_config(),
        )
        try:
            assert elapsed < 5.0, f"RO reopen took {elapsed:.2f}s (>=5.0)"
            ro.execute("LOAD semantic_views")
            names = [
                r[0]
                for r in ro.execute(
                    "SELECT name FROM list_semantic_views()"
                ).fetchall()
            ]
            assert names == ["v"], f"expected ['v'], got: {names}"
        finally:
            ro.close()


def test_in_process_load_only_then_readonly():
    """B3 (LIFE-01 isolation): LOAD only (no CREATE) → close → RO reopen.

    Isolates the leak to `LOAD semantic_views` itself rather than to
    `CREATE SEMANTIC VIEW`. If this test fails the same way as B1/B2 on
    baseline, the root cause is in `init_extension`'s
    `catalog_conn`/`query_conn` allocation (RESEARCH §2.2 framing), not
    in the CREATE pipeline.
    """
    import duckdb

    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "load_only.duckdb")
        # Bootstrap an empty valid DuckDB file without the extension so
        # the RO reopen has a header to read.
        seed = duckdb.connect(db)
        seed.execute("SELECT 1")
        seed.close()
        del seed
        gc.collect()

        # In-process LOAD only — no CREATE SEMANTIC VIEW.
        w = open_writable(db)  # FORCE INSTALL + LOAD inside helper
        w.close()
        del w
        gc.collect()

        _, elapsed = _connect_with_watchdog(
            db,
            watchdog_seconds=5.0,
            read_only=True,
            config=_connect_config(),
        )
        # No further assertions on the RO connection beyond "it opened
        # within the budget" — B3 is the isolation test.
        assert elapsed < 5.0, f"RO reopen took {elapsed:.2f}s (>=5.0)"


def test_in_process_readonly_then_readwrite():
    """B4 / D-09: bootstrap → in-process RO open → close → RW reopen.

    Reverse direction of B1/B2. Same root cause (shared
    `DBInstanceCache` key — RESEARCH §5.3) so this should fail on
    baseline and pass after the same fix. Documenting it here pins the
    behaviour so a future regression that re-introduces the leak in only
    one direction is caught.
    """
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "ro_then_rw.duckdb")
        bootstrap_in_subprocess(
            db,
            [
                "CREATE TABLE t (i INT)",
                _minimal_create_sql("v"),
            ],
        )

        ro = open_readonly(db)
        ro.close()
        del ro
        gc.collect()

        rw, elapsed = _connect_with_watchdog(
            db,
            watchdog_seconds=5.0,
            config=_connect_config(),
        )
        try:
            assert elapsed < 5.0, f"RW reopen took {elapsed:.2f}s (>=5.0)"
        finally:
            rw.close()


def test_in_process_bootstrap_then_readonly_semantic_view_select():
    """D-03b #1 (LIFE-01 / criterion 3): post-reopen semantic_view() SELECT.

    B1 prologue (in-process bootstrap, close, watchdog-wrapped RO reopen)
    plus a post-reopen ``semantic_view('v', dimensions := [...], metrics
    := [...])`` SELECT. Exercises the read-side
    ``semantic_view_bind`` / exec callbacks against an RO connection that
    was reopened in the same process — the exact failure shape on the
    v0.9.0 baseline (LIFE-01 hang in DBInstanceCache::GetInstanceInternal).
    """
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "sv_select.duckdb")
        w = open_writable(db)
        w.execute("CREATE TABLE t (i INT, j INT)")
        w.execute("INSERT INTO t VALUES (1, 10), (2, 20)")
        w.execute(
            "CREATE SEMANTIC VIEW v AS "
            "  TABLES (t1 AS t PRIMARY KEY (i)) "
            "  DIMENSIONS (t1.i AS t1.i) "
            "  METRICS (t1.s AS SUM(t1.j))"
        )
        w.close()
        del w
        gc.collect()

        ro, elapsed = _connect_with_watchdog(
            db,
            watchdog_seconds=5.0,
            read_only=True,
            config=_connect_config(),
        )
        try:
            assert elapsed < 5.0, f"RO reopen took {elapsed:.2f}s (>=5.0)"
            ro.execute("LOAD semantic_views")
            rows = ro.execute(
                "SELECT i, s FROM semantic_view("
                "  'v', dimensions := ['t1.i'], metrics := ['t1.s']"
                ") ORDER BY i"
            ).fetchall()
            assert rows == [(1, 10), (2, 20)], f"unexpected rows: {rows}"
        finally:
            ro.close()


def test_in_process_bootstrap_then_readonly_describe():
    """D-03b #2 (LIFE-01 / criterion 3): post-reopen describe_semantic_view().

    Exercises the ``describe_semantic_view`` bind callback (Plan 05
    migration target) against an in-process RW->RO reopened connection.
    """
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "sv_describe.duckdb")
        w = open_writable(db)
        w.execute("CREATE TABLE t (i INT)")
        w.execute(_minimal_create_sql("v"))
        w.close()
        del w
        gc.collect()

        ro, elapsed = _connect_with_watchdog(
            db,
            watchdog_seconds=5.0,
            read_only=True,
            config=_connect_config(),
        )
        try:
            assert elapsed < 5.0, f"RO reopen took {elapsed:.2f}s (>=5.0)"
            ro.execute("LOAD semantic_views")
            rows = ro.execute(
                "FROM describe_semantic_view('v')"
            ).fetchall()
            assert len(rows) > 0, "describe_semantic_view returned no rows"
        finally:
            ro.close()


def test_in_process_bootstrap_then_readonly_show_dimensions():
    """D-03b #3 (LIFE-01 / criterion 3): post-reopen SHOW SEMANTIC DIMENSIONS.

    Representative SHOW command — exercises the SHOW dispatch path's
    bind callback against an in-process RW->RO reopened connection.
    """
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "sv_show.duckdb")
        w = open_writable(db)
        w.execute("CREATE TABLE t (i INT)")
        w.execute(_minimal_create_sql("v"))
        w.close()
        del w
        gc.collect()

        ro, elapsed = _connect_with_watchdog(
            db,
            watchdog_seconds=5.0,
            read_only=True,
            config=_connect_config(),
        )
        try:
            assert elapsed < 5.0, f"RO reopen took {elapsed:.2f}s (>=5.0)"
            ro.execute("LOAD semantic_views")
            rows = ro.execute("SHOW SEMANTIC DIMENSIONS IN v").fetchall()
            assert len(rows) > 0, "SHOW SEMANTIC DIMENSIONS returned no rows"
            # The dimension we declared is `t1.i AS t1.i` — look for `i`
            # in the row tuples regardless of column position.
            assert any("i" in str(r) for r in rows), (
                f"expected 'i' in SHOW DIMENSIONS rows, got: {rows}"
            )
        finally:
            ro.close()


def test_in_process_bootstrap_then_readonly_get_ddl():
    """D-03b #4 (LIFE-01 / criterion 3): post-reopen get_ddl() round-trip.

    Exercises the ``get_ddl`` scalar function (Plan 05 migration target)
    against an in-process RW->RO reopened connection.
    """
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "sv_getddl.duckdb")
        w = open_writable(db)
        w.execute("CREATE TABLE t (i INT)")
        w.execute(_minimal_create_sql("v"))
        w.close()
        del w
        gc.collect()

        ro, elapsed = _connect_with_watchdog(
            db,
            watchdog_seconds=5.0,
            read_only=True,
            config=_connect_config(),
        )
        try:
            assert elapsed < 5.0, f"RO reopen took {elapsed:.2f}s (>=5.0)"
            ro.execute("LOAD semantic_views")
            # get_ddl takes (kind, name) — kind 'SEMANTIC_VIEW' selects
            # the semantic-view DDL emitter.
            ddl = ro.execute("SELECT get_ddl('SEMANTIC_VIEW', 'v')").fetchone()[0]
            assert "CREATE OR REPLACE SEMANTIC VIEW" in ddl, (
                f"expected CREATE OR REPLACE SEMANTIC VIEW in ddl, got: {ddl}"
            )
            assert "v" in ddl, f"expected view name 'v' in ddl, got: {ddl}"
        finally:
            ro.close()


def test_repeated_load_close_no_busy_spin():
    """B11 (LIFE-02 audit): 50 sequential file-backed bootstrap+close cycles.

    Each iteration creates a fresh tempfile DB, opens writable,
    LOAD + CREATE, closes, then reopens read-only via the watchdog
    (assert < 5 s). On v0.9.0 baseline the FIRST iteration's RO reopen
    busy-spins and triggers TimeoutError — so this test fails fast and
    leaks one watchdog thread, then aborts the loop. On v0.9.1 all 50
    iterations complete in well under 10 s total.

    Registered LAST in main() so the post-failure thread leak does not
    interfere with subsequent tests.
    """
    import duckdb

    iterations = 50
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        for i in range(iterations):
            db = str(tmp_path / f"loop_{i}.duckdb")
            # Seed empty valid DuckDB file so RO open has a header.
            seed = duckdb.connect(db)
            seed.execute("SELECT 1")
            seed.close()

            w = open_writable(db)
            w.execute("CREATE TABLE t (i INT)")
            w.execute(_minimal_create_sql(f"v_{i}"))
            w.close()
            del w
            gc.collect()

            ro, elapsed = _connect_with_watchdog(
                db,
                watchdog_seconds=5.0,
                read_only=True,
                config=_connect_config(),
            )
            try:
                assert elapsed < 5.0, (
                    f"iter {i}: RO reopen took {elapsed:.2f}s (>=5.0)"
                )
            finally:
                ro.close()


if __name__ == "__main__":
    results = [
        # Existing subprocess-style tests (B5) — must remain green on baseline
        # and after the fix. Run first so the post-failure thread leaks from
        # the in-process tests below do not contaminate them.
        run_test("test_fresh_readonly_empty_list", test_fresh_readonly_empty_list),
        run_test("test_bootstrapped_readonly_query_works", test_bootstrapped_readonly_query_works),
        run_test("test_readonly_ddl_fails", test_readonly_ddl_fails),
        # Phase 65 in-process regression tests — EXPECTED TO FAIL on the
        # v0.9.0 baseline of milestone/v0.9.1 (proves the bug is real). Plans
        # 02/03 land the fix; Plan 04 verifies these flip green. Watchdog
        # leaks daemon threads on failure — see `_connect_with_watchdog`
        # docstring. B11 is last because its post-failure leak is the
        # heaviest (each loop iteration leaks an unkillable C++ spin
        # thread until the first failure aborts).
        run_test("test_in_process_bootstrap_then_readonly_fresh",   test_in_process_bootstrap_then_readonly_fresh),
        run_test("test_in_process_bootstrap_then_readonly_existing", test_in_process_bootstrap_then_readonly_existing),
        run_test("test_in_process_load_only_then_readonly",          test_in_process_load_only_then_readonly),
        run_test("test_in_process_readonly_then_readwrite",          test_in_process_readonly_then_readwrite),
        # Plan 06 D-03b post-reopen tests — exercise the read-side bind
        # callbacks (semantic_view / describe / SHOW / get_ddl) against an
        # in-process RW->RO reopened connection. All four must pass on
        # milestone/v0.10.0 (LIFE-01 / ROADMAP success criterion 3).
        run_test("test_in_process_bootstrap_then_readonly_semantic_view_select",
                 test_in_process_bootstrap_then_readonly_semantic_view_select),
        run_test("test_in_process_bootstrap_then_readonly_describe",
                 test_in_process_bootstrap_then_readonly_describe),
        run_test("test_in_process_bootstrap_then_readonly_show_dimensions",
                 test_in_process_bootstrap_then_readonly_show_dimensions),
        run_test("test_in_process_bootstrap_then_readonly_get_ddl",
                 test_in_process_bootstrap_then_readonly_get_ddl),
        run_test("test_repeated_load_close_no_busy_spin",            test_repeated_load_close_no_busy_spin),
    ]
    passed = sum(results)
    total = len(results)
    print(f"\n{'=' * 60}")
    print(f"SUMMARY: {passed}/{total} tests passed")
    print(f"{'=' * 60}")
    sys.exit(0 if passed == total else 1)
