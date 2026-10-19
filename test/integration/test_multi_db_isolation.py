#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
Multi-database isolation regression test for the semantic_views extension.

Loading the extension into more than one database in the same process must
keep each database's DDL routing isolated. Pre-fix the C++ shim held a
single file-static `sv_ddl_conn`; the second LOAD overwrote it, so any
DESCRIBE / SHOW SEMANTIC * statement on the first database silently
executed against the second database's connection.

After the fix, the per-load `ddl_conn` lives on `SemanticViewsParserInfo`
and is threaded through `TableFunction::function_info` to `sv_ddl_bind`,
so every DDL routes to the correct database.

Test scenario:
  1. Create two file-backed databases with distinct semantic views.
  2. Run DESCRIBE / list_semantic_views on each connection.
  3. Assert each connection sees its own view and only its own view.

Without the fix, DESCRIBE on the first connection would either return the
second DB's view metadata or raise a confusing error.

Exit codes:
    0 = all tests passed
    1 = at least one test failed
"""

import sys
import tempfile
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path


EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()


def make_connection(db_path: str):
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


def setup_db(conn, view_name: str, region_value: str):
    conn.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, "
        "amount DECIMAL(10,2), region VARCHAR)"
    )
    conn.execute(f"INSERT INTO orders VALUES (1, 100.00, '{region_value}')")
    conn.execute(
        f"""
        CREATE SEMANTIC VIEW {view_name} AS
          TABLES (o AS orders PRIMARY KEY (id))
          DIMENSIONS (o.region AS o.region)
          METRICS (o.total AS SUM(o.amount))
        """
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


def test_multi_db_isolation():
    with tempfile.TemporaryDirectory() as tmpdir:
        db1_path = str(Path(tmpdir) / "db1.duckdb")
        db2_path = str(Path(tmpdir) / "db2.duckdb")

        con1 = make_connection(db1_path)
        con2 = make_connection(db2_path)
        try:
            setup_db(con1, "view_db1", "US")
            setup_db(con2, "view_db2", "EU")

            # list_semantic_views on each connection should only see its own view.
            db1_views = {row[0] for row in con1.execute(
                "SELECT name FROM list_semantic_views()"
            ).fetchall()}
            db2_views = {row[0] for row in con2.execute(
                "SELECT name FROM list_semantic_views()"
            ).fetchall()}
            assert db1_views == {"view_db1"}, (
                f"db1 should see only view_db1, got: {db1_views}"
            )
            assert db2_views == {"view_db2"}, (
                f"db2 should see only view_db2, got: {db2_views}"
            )

            # DESCRIBE is rewritten on the parser_override success path
            # (v0.8.0 Phase 59 unification) into a SELECT against the
            # read-side table function. The original cross-DB-routing bug
            # this test pins lived in the per-call function_info lookup;
            # the fix is verified by both DBs returning their own view.
            db1_desc = con1.execute("DESCRIBE SEMANTIC VIEW view_db1").fetchall()
            assert len(db1_desc) > 0, "db1 DESCRIBE returned no rows"

            db2_desc = con2.execute("DESCRIBE SEMANTIC VIEW view_db2").fetchall()
            assert len(db2_desc) > 0, "db2 DESCRIBE returned no rows"

            # Cross-DB DESCRIBE must error: each view is local to its own DB.
            try:
                con1.execute("DESCRIBE SEMANTIC VIEW view_db2").fetchall()
                raise AssertionError(
                    "con1 should not see view_db2 (cross-DB leak)"
                )
            except AssertionError:
                raise
            except Exception:
                pass  # expected — view_db2 doesn't exist in db1

            try:
                con2.execute("DESCRIBE SEMANTIC VIEW view_db1").fetchall()
                raise AssertionError(
                    "con2 should not see view_db1 (cross-DB leak)"
                )
            except AssertionError:
                raise
            except Exception:
                pass

            # Issuing SHOW SEMANTIC VIEWS alternately on both connections
            # exercises per-call routing on the parser_override success path
            # (rewritten to SELECT against the read-side table function),
            # making sure each connection's SHOW resolves against its own DB.
            # Column 1 of the SHOW result is `name` (column 0 is `created_on`).
            for _ in range(3):
                db1_now = {row[1] for row in con1.execute(
                    "SHOW SEMANTIC VIEWS"
                ).fetchall()}
                db2_now = {row[1] for row in con2.execute(
                    "SHOW SEMANTIC VIEWS"
                ).fetchall()}
                assert db1_now == {"view_db1"}, (
                    f"db1 SHOW should see only view_db1, got: {db1_now}"
                )
                assert db2_now == {"view_db2"}, (
                    f"db2 SHOW should see only view_db2, got: {db2_now}"
                )
        finally:
            con1.close()
            con2.close()


def test_seventeen_dbs_sequential_create():
    """B15 expanded — open 17 in-memory DBs sequentially, CREATE on each.

    Pre-Phase-62: LRU evicts DB #1 on the 17th open and the next CREATE
    on its handle surfaces 'catalog context for this database has been
    evicted'. Post-Phase-62: every CREATE succeeds because OverrideContext
    is attached per-DBConfig and never evicted.
    """
    import duckdb

    connections = []
    try:
        for i in range(17):
            conn = duckdb.connect(
                ":memory:",
                config={
                    "allow_unsigned_extensions": "true",
                    "extension_directory": EXT_DIR,
                },
            )
            conn.execute(f"FORCE INSTALL '{EXT_PATH}'")
            conn.execute("LOAD semantic_views")
            conn.execute(f"CREATE TABLE t_{i}(id INTEGER PRIMARY KEY)")
            conn.execute(
                f"CREATE SEMANTIC VIEW v_{i} AS "
                f"TABLES (t_{i} AS t_{i} PRIMARY KEY (id)) "
                f"DIMENSIONS (t_{i}.id AS id) "
                f"METRICS (t_{i}.c AS COUNT(*))"
            )
            connections.append(conn)
        # Verify oldest DB still has its view (pre-Phase-62 the LRU would
        # have evicted DB #0's catalog context on the 17th open and this
        # DESCRIBE would surface 'catalog context evicted').
        rows = connections[0].execute("DESCRIBE SEMANTIC VIEW v_0").fetchall()
        assert len(rows) > 0, "DB #0 lost its view to LRU eviction"
    finally:
        for c in connections:
            try:
                c.close()
            except Exception:
                pass


def test_fifty_db_open_close_rss_bounded():
    """B16 — sequentially open+close 50 in-memory DBs each running a CREATE.

    Asserts RSS does not grow pathologically. The catalog connection
    leaks one Connection object per DB (~few KB) — bounded by workload,
    not by an LRU cap (see Phase 62 RESEARCH.md §Q2 for the leak rationale).

    Threshold is intentionally loose (500 MB): in addition to our ~few KB
    leak per DB, DuckDB's own per-load extension cache and per-DB
    metadata structures persist across this loop on macOS (ru_maxrss
    tracks the high-water mark, not deltas). 500 MB still flags a true
    leak (which would compound to GB on a 50-iteration loop) without
    failing on platform-driven baseline overhead.
    """
    import duckdb
    import platform
    import resource

    start_rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss

    for i in range(50):
        conn = duckdb.connect(
            ":memory:",
            config={
                "allow_unsigned_extensions": "true",
                "extension_directory": EXT_DIR,
            },
        )
        conn.execute(f"FORCE INSTALL '{EXT_PATH}'")
        conn.execute("LOAD semantic_views")
        conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY)")
        conn.execute(
            "CREATE SEMANTIC VIEW v AS "
            "TABLES (t AS t PRIMARY KEY (id)) "
            "DIMENSIONS (t.id AS id) "
            "METRICS (t.c AS COUNT(*))"
        )
        conn.close()

    end_rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # macOS reports ru_maxrss in BYTES, Linux/BSD in KB. Detect by platform.
    if platform.system() == "Darwin":
        delta_mb = (end_rss - start_rss) / (1024 * 1024)
    else:
        delta_mb = (end_rss - start_rss) / 1024
    print(f"  RSS delta over 50 iterations: {delta_mb:.1f} MB")
    assert delta_mb < 500, (
        f"RSS delta too large: {delta_mb:.1f} MB — possible per-DB retention bug"
    )


def main():
    print(f"Extension path: {EXT_PATH}")
    print(f"Extension dir:  {EXT_DIR}")

    if not Path(EXT_PATH).exists():
        print(f"ERROR: Extension not found at {EXT_PATH}", file=sys.stderr)
        print("Run `just build` first.", file=sys.stderr)
        sys.exit(1)

    results = [
        run_test("multi-DB DDL isolation", test_multi_db_isolation),
        # Phase 62 Wave 3 — 17-DB and 50-DB tests are now active.
        run_test(
            "seventeen DBs sequential CREATE (B15)",
            test_seventeen_dbs_sequential_create,
        ),
        run_test(
            "fifty DB open-close RSS bounded (B16)",
            test_fifty_db_open_close_rss_bounded,
        ),
    ]

    print(f"\n{'=' * 60}")
    print(f"SUMMARY: {sum(results)}/{len(results)} passed")
    print(f"{'=' * 60}")
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
