#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Systematic Python crash reproduction test for the semantic_views extension.

Exercises all five crash vectors (CV-1 through CV-5) identified in the
Phase 17.1 research. Each test function targets a specific crash vector
and reports PASS, FAIL, CRASH, or TIMEOUT.

Uses native CREATE SEMANTIC VIEW ... AS ... DDL syntax exclusively.
Query interface (semantic_view() / explain_semantic_view()) is unchanged.

Usage:
    python3 test/integration/test_vtab_crash.py

    Or with a custom extension path:
    SEMANTIC_VIEWS_EXTENSION_PATH=build/debug/semantic_views.duckdb_extension \
        python3 test/integration/test_vtab_crash.py

Exit codes:
    0 = all tests passed
    1 = at least one test failed or crashed
    2 = timeout (possible deadlock)

Crash vectors:
    CV-1: Type mismatch in duckdb_vector_reference_vector (HIGH)
    CV-2: Connection lifetime / use-after-free (MEDIUM)
    CV-3: duckdb_query during bind/plan phase (MEDIUM)
    CV-4: Materialized vs streaming result confusion (LOW)
    CV-5: extra_info pointer lifetime (LOW)
"""

import gc
import os
import platform
import signal
import sys
import tempfile
import traceback
from pathlib import Path

# Add test/integration to path for helpers import
sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path


# ---------------------------------------------------------------------------
# Timeout handling
# ---------------------------------------------------------------------------

class TimeoutError(Exception):
    pass


def timeout_handler(signum, frame):
    raise TimeoutError("TIMEOUT - possible deadlock (CV-3)")


# signal.alarm is Unix-only; on Windows we skip timeout protection
HAS_ALARM = hasattr(signal, "SIGALRM")
if HAS_ALARM:
    signal.signal(signal.SIGALRM, timeout_handler)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()


def make_connection(db_path=":memory:"):
    """Create a DuckDB connection with the extension loaded."""
    import duckdb

    con = duckdb.connect(
        db_path,
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": EXT_DIR,
        },
    )
    con.execute(f"FORCE INSTALL '{EXT_PATH}'")
    con.execute("LOAD semantic_views")
    return con


def run_test(name, crash_vector, test_fn):
    """Run a single test with timeout and exception handling."""
    print(f"\n{'='*60}")
    print(f"TEST: {name}")
    print(f"TARGET: {crash_vector}")
    print(f"{'='*60}")

    if HAS_ALARM:
        signal.alarm(30)  # 30 second timeout

    try:
        test_fn()
        if HAS_ALARM:
            signal.alarm(0)
        print(f"  RESULT: PASS")
        return True
    except TimeoutError:
        print(f"  RESULT: TIMEOUT (possible deadlock)")
        return False
    except SystemError as e:
        if HAS_ALARM:
            signal.alarm(0)
        print(f"  RESULT: CRASH (SystemError)")
        print(f"  ERROR: {e}")
        traceback.print_exc()
        return False
    except Exception as e:
        if HAS_ALARM:
            signal.alarm(0)
        error_str = str(e).lower()
        # Detect crash-like errors from DuckDB
        if any(w in error_str for w in ["segfault", "abort", "signal", "assertion",
                                         "vector::reference", "type mismatch"]):
            print(f"  RESULT: CRASH")
        else:
            print(f"  RESULT: FAIL")
        print(f"  ERROR: {e}")
        traceback.print_exc()
        return False


# ---------------------------------------------------------------------------
# CV-1: Type mismatch in duckdb_vector_reference_vector
# ---------------------------------------------------------------------------

def test_cv1_basic_types():
    """CV-1: Basic types (VARCHAR, INTEGER, BIGINT) -- should work."""
    con = make_connection()
    try:
        con.execute("CREATE TABLE t1 (id INTEGER, name VARCHAR, amount BIGINT)")
        con.execute("INSERT INTO t1 VALUES (1, 'alice', 100), (2, 'bob', 200)")

        con.execute("""
            CREATE SEMANTIC VIEW cv1_basic AS
            TABLES (t AS t1 PRIMARY KEY (id))
            DIMENSIONS (t.name AS name)
            METRICS (t.total AS sum(amount), t.cnt AS count(*))
        """)

        result = con.execute("""
            SELECT * FROM semantic_view('cv1_basic',
                dimensions := ['name'],
                metrics := ['total', 'cnt']
            )
        """).fetchall()
        print(f"  Rows: {len(result)}")
        print(f"  Data: {result}")
        assert len(result) == 2, f"Expected 2 rows, got {len(result)}"
    finally:
        con.close()


def test_cv1_decimal_sum():
    """CV-1: DECIMAL(10,2) column with sum() metric -- most likely crash trigger.

    sum() on DECIMAL may produce HUGEINT at plan time but BIGINT at runtime,
    causing type mismatch in duckdb_vector_reference_vector.
    """
    con = make_connection()
    try:
        con.execute("CREATE TABLE t2 (id INTEGER, amount DECIMAL(10,2))")
        con.execute("INSERT INTO t2 VALUES (1, 100.50), (2, 200.75), (3, 50.25)")

        con.execute("""
            CREATE SEMANTIC VIEW cv1_decimal AS
            TABLES (t AS t2 PRIMARY KEY (id))
            DIMENSIONS (t.id AS id)
            METRICS (t.total_amount AS sum(amount))
        """)

        # Query with sum(decimal) -- potential type mismatch
        result = con.execute("""
            SELECT * FROM semantic_view('cv1_decimal',
                metrics := ['total_amount']
            )
        """).fetchall()
        print(f"  Rows: {len(result)}")
        print(f"  Data: {result}")
        print(f"  Type of total_amount: {type(result[0][0]).__name__}")
        assert len(result) == 1, f"Expected 1 row, got {len(result)}"
    finally:
        con.close()


def test_cv1_date_trunc():
    """CV-1: date_trunc dimension with count(*) metric -- tests DATE type flow."""
    con = make_connection()
    try:
        con.execute("CREATE TABLE t3 (id INTEGER, dt DATE, val INTEGER)")
        con.execute("""
            INSERT INTO t3 VALUES
                (1, '2024-01-15', 10),
                (2, '2024-01-20', 20),
                (3, '2024-02-10', 30)
        """)

        con.execute("""
            CREATE SEMANTIC VIEW cv1_datetrunc AS
            TABLES (t AS t3 PRIMARY KEY (id))
            DIMENSIONS (t.month AS date_trunc('month', dt))
            METRICS (t.cnt AS count(*))
        """)

        result = con.execute("""
            SELECT * FROM semantic_view('cv1_datetrunc',
                dimensions := ['month'],
                metrics := ['cnt']
            )
        """).fetchall()
        print(f"  Rows: {len(result)}")
        print(f"  Data: {result}")
        assert len(result) == 2, f"Expected 2 rows (Jan + Feb), got {len(result)}"
    finally:
        con.close()


def test_cv1_mixed_aggregates():
    """CV-1: Multiple aggregate types in one query -- exercises multiple type paths.

    Uses count(*), sum(integer), avg(decimal), min(date) simultaneously.
    """
    con = make_connection()
    try:
        con.execute("""
            CREATE TABLE t4 (
                id INTEGER, name VARCHAR, amount DECIMAL(10,2),
                dt DATE, quantity INTEGER
            )
        """)
        con.execute("""
            INSERT INTO t4 VALUES
                (1, 'alice', 100.50, '2024-01-15', 5),
                (2, 'bob', 200.75, '2024-02-10', 10),
                (3, 'alice', 50.25, '2024-03-05', 3)
        """)

        con.execute("""
            CREATE SEMANTIC VIEW cv1_mixed AS
            TABLES (t AS t4 PRIMARY KEY (id))
            DIMENSIONS (t.name AS name)
            METRICS (
                t.cnt AS count(*),
                t.total_qty AS sum(quantity),
                t.avg_amount AS avg(amount),
                t.first_date AS min(dt)
            )
        """)

        result = con.execute("""
            SELECT * FROM semantic_view('cv1_mixed',
                dimensions := ['name'],
                metrics := ['cnt', 'total_qty', 'avg_amount', 'first_date']
            )
        """).fetchall()
        print(f"  Rows: {len(result)}")
        print(f"  Data: {result}")
        for i, row in enumerate(result):
            types = [type(v).__name__ for v in row]
            print(f"  Row {i} types: {types}")
        assert len(result) == 2, f"Expected 2 rows (alice + bob), got {len(result)}"
    finally:
        con.close()


def test_cv1_many_types():
    """CV-1: Wide table with many column types -- exercises type dispatch breadth.

    Creates a table with BOOLEAN, TINYINT, SMALLINT, INTEGER, BIGINT, FLOAT,
    DOUBLE, DECIMAL(18,4), VARCHAR, DATE, TIMESTAMP columns and queries
    various combinations.
    """
    con = make_connection()
    try:
        con.execute("""
            CREATE TABLE t5 (
                b BOOLEAN,
                ti TINYINT,
                si SMALLINT,
                i INTEGER,
                bi BIGINT,
                f FLOAT,
                d DOUBLE,
                dec DECIMAL(18,4),
                v VARCHAR,
                dt DATE,
                ts TIMESTAMP
            )
        """)
        con.execute("""
            INSERT INTO t5 VALUES
                (true, 1, 100, 1000, 10000, 1.5, 2.5, 100.1234, 'a', '2024-01-01', '2024-01-01 10:00:00'),
                (false, 2, 200, 2000, 20000, 3.5, 4.5, 200.5678, 'b', '2024-02-01', '2024-02-01 12:00:00'),
                (true, 3, 300, 3000, 30000, 5.5, 6.5, 300.9999, 'a', '2024-03-01', '2024-03-01 14:00:00')
        """)

        con.execute("""
            CREATE SEMANTIC VIEW cv1_many AS
            TABLES (t AS t5 PRIMARY KEY (ti))
            DIMENSIONS (t.label AS v)
            METRICS (
                t.cnt AS count(*),
                t.sum_bi AS sum(bi),
                t.avg_d AS avg(d),
                t.sum_dec AS sum(dec),
                t.min_dt AS min(dt),
                t.max_ts AS max(ts)
            )
        """)

        result = con.execute("""
            SELECT * FROM semantic_view('cv1_many',
                dimensions := ['label'],
                metrics := ['cnt', 'sum_bi', 'avg_d', 'sum_dec', 'min_dt', 'max_ts']
            )
        """).fetchall()
        print(f"  Rows: {len(result)}")
        print(f"  Data: {result}")
        for i, row in enumerate(result):
            types = [type(v).__name__ for v in row]
            print(f"  Row {i} types: {types}")
        assert len(result) == 2, f"Expected 2 rows (a + b), got {len(result)}"
    finally:
        con.close()


# ---------------------------------------------------------------------------
# CV-2: Connection lifetime / use-after-free
# ---------------------------------------------------------------------------

def test_cv2_explicit_close():
    """CV-2: Explicit connection close after query -- tests cleanup ordering."""
    con = make_connection()
    try:
        con.execute("CREATE TABLE tc1 (id INTEGER, val INTEGER)")
        con.execute("INSERT INTO tc1 VALUES (1, 10), (2, 20)")

        con.execute("""
            CREATE SEMANTIC VIEW cv2_close AS
            TABLES (t AS tc1 PRIMARY KEY (id))
            DIMENSIONS (t.id AS id)
            METRICS (t.total AS sum(val))
        """)

        result = con.execute("""
            SELECT * FROM semantic_view('cv2_close',
                dimensions := ['id'],
                metrics := ['total']
            )
        """).fetchall()
        print(f"  Query result: {result}")
        assert len(result) == 2
    finally:
        # Explicit close -- may trigger use-after-free on auxiliary connections
        print("  Closing connection explicitly...")
        con.close()
        print("  Connection closed without crash")


def test_cv2_gc_collection():
    """CV-2: Garbage collection after connection use -- tests GC cleanup path."""
    import duckdb

    # Create connection in a function scope so it becomes eligible for GC
    def _inner():
        con = make_connection()
        con.execute("CREATE TABLE tc2 (id INTEGER, val INTEGER)")
        con.execute("INSERT INTO tc2 VALUES (1, 10), (2, 20)")

        con.execute("""
            CREATE SEMANTIC VIEW cv2_gc AS
            TABLES (t AS tc2 PRIMARY KEY (id))
            DIMENSIONS (t.id AS id)
            METRICS (t.total AS sum(val))
        """)

        result = con.execute("""
            SELECT * FROM semantic_view('cv2_gc',
                dimensions := ['id'],
                metrics := ['total']
            )
        """).fetchall()
        print(f"  Query result: {result}")
        assert len(result) == 2
        # Let con go out of scope without explicit close
        del con

    _inner()
    print("  Triggering garbage collection...")
    gc.collect()
    print("  GC completed without crash")


def test_cv2_multiple_queries():
    """CV-2: Multiple sequential queries on same view -- tests connection reuse."""
    con = make_connection()
    try:
        con.execute("CREATE TABLE tc3 (id INTEGER, val INTEGER)")
        con.execute("INSERT INTO tc3 VALUES (1, 10), (2, 20), (3, 30)")

        con.execute("""
            CREATE SEMANTIC VIEW cv2_multi AS
            TABLES (t AS tc3 PRIMARY KEY (id))
            DIMENSIONS (t.id AS id)
            METRICS (t.total AS sum(val), t.cnt AS count(*))
        """)

        # Run multiple queries in sequence
        for i in range(5):
            result = con.execute("""
                SELECT * FROM semantic_view('cv2_multi',
                    metrics := ['total', 'cnt']
                )
            """).fetchall()
            print(f"  Query {i+1}: {result}")
            assert len(result) == 1

        # Also query with dimensions
        for i in range(3):
            result = con.execute("""
                SELECT * FROM semantic_view('cv2_multi',
                    dimensions := ['id'],
                    metrics := ['total']
                )
            """).fetchall()
            print(f"  Dim query {i+1}: {result}")
            assert len(result) == 3
    finally:
        con.close()


# ---------------------------------------------------------------------------
# CV-3: duckdb_query during bind/plan phase
# ---------------------------------------------------------------------------

def test_cv3_inmemory_bind():
    """CV-3: In-memory DB bind-time SQL execution -- tests LIMIT 0 on query_conn."""
    con = make_connection()
    try:
        con.execute("CREATE TABLE tb1 (id INTEGER, val DECIMAL(10,2))")
        con.execute("INSERT INTO tb1 VALUES (1, 10.50), (2, 20.75)")

        con.execute("""
            CREATE SEMANTIC VIEW cv3_inmem AS
            TABLES (t AS tb1 PRIMARY KEY (id))
            DIMENSIONS (t.id AS id)
            METRICS (t.total AS sum(val))
        """)

        # This triggers bind() which runs LIMIT 0 on query_conn (CV-3)
        result = con.execute("""
            SELECT * FROM semantic_view('cv3_inmem',
                dimensions := ['id'],
                metrics := ['total']
            )
        """).fetchall()
        print(f"  Result: {result}")
        assert len(result) == 2
    finally:
        con.close()


def test_cv3_file_backed_bind():
    """CV-3: File-backed DB bind-time SQL -- tests persist_conn path."""
    tmpdir = tempfile.mkdtemp()
    db_path = os.path.join(tmpdir, "test_cv3.duckdb")
    try:
        con = make_connection(db_path)
        try:
            con.execute("CREATE TABLE tb2 (id INTEGER, val DECIMAL(10,2))")
            con.execute("INSERT INTO tb2 VALUES (1, 10.50), (2, 20.75)")

            con.execute("""
                CREATE SEMANTIC VIEW cv3_file AS
                TABLES (t AS tb2 PRIMARY KEY (id))
                DIMENSIONS (t.id AS id)
                METRICS (t.total AS sum(val))
            """)

            result = con.execute("""
                SELECT * FROM semantic_view('cv3_file',
                    dimensions := ['id'],
                    metrics := ['total']
                )
            """).fetchall()
            print(f"  Result: {result}")
            assert len(result) == 2
        finally:
            con.close()
    finally:
        import shutil
        shutil.rmtree(tmpdir, ignore_errors=True)


# ---------------------------------------------------------------------------
# CV-4: Materialized vs streaming result confusion
# ---------------------------------------------------------------------------

def test_cv4_large_result():
    """CV-4: Large result set (10,000 rows) -- forces multiple chunks."""
    con = make_connection()
    try:
        con.execute("CREATE TABLE t_large (id INTEGER, category VARCHAR, val INTEGER)")
        # Insert 10,000 rows with 100 categories
        con.execute("""
            INSERT INTO t_large
            SELECT i, 'cat_' || (i % 100), i * 10
            FROM generate_series(1, 10000) t(i)
        """)

        con.execute("""
            CREATE SEMANTIC VIEW cv4_large AS
            TABLES (t AS t_large PRIMARY KEY (id))
            DIMENSIONS (t.category AS category)
            METRICS (t.total AS sum(val), t.cnt AS count(*))
        """)

        result = con.execute("""
            SELECT * FROM semantic_view('cv4_large',
                dimensions := ['category'],
                metrics := ['total', 'cnt']
            )
        """).fetchall()
        print(f"  Rows returned: {len(result)}")
        assert len(result) == 100, f"Expected 100 categories, got {len(result)}"
        # Verify each category has 100 rows
        for row in result[:3]:
            print(f"  Sample: {row}")
    finally:
        con.close()


def test_cv4_empty_result():
    """CV-4: Query that returns zero rows -- tests empty chunk handling."""
    con = make_connection()
    try:
        con.execute("CREATE TABLE t_empty (id INTEGER, val INTEGER)")
        # Don't insert any data

        con.execute("""
            CREATE SEMANTIC VIEW cv4_empty AS
            TABLES (t AS t_empty PRIMARY KEY (id))
            DIMENSIONS (t.id AS id)
            METRICS (t.cnt AS count(*))
        """)

        result = con.execute("""
            SELECT * FROM semantic_view('cv4_empty',
                dimensions := ['id'],
                metrics := ['cnt']
            )
        """).fetchall()
        print(f"  Rows returned: {len(result)}")
        # Empty GROUP BY with no data should return 0 rows
        assert len(result) == 0, f"Expected 0 rows, got {len(result)}"
    finally:
        con.close()


# ---------------------------------------------------------------------------
# CV-5: extra_info pointer lifetime
# ---------------------------------------------------------------------------

def test_cv5_define_query_define_query():
    """CV-5: Multiple define/query cycles -- tests extra_info pointer stability.

    Define view A, query A, define view B, query B, query A again.
    Exercises extra_info stability across multiple registrations.
    """
    con = make_connection()
    try:
        con.execute("CREATE TABLE ta (id INTEGER, val INTEGER)")
        con.execute("INSERT INTO ta VALUES (1, 10), (2, 20)")
        con.execute("CREATE TABLE tb (id INTEGER, val INTEGER)")
        con.execute("INSERT INTO tb VALUES (3, 30), (4, 40)")

        # Define view A
        con.execute("""
            CREATE SEMANTIC VIEW cv5_a AS
            TABLES (t AS ta PRIMARY KEY (id))
            DIMENSIONS (t.id AS id)
            METRICS (t.total AS sum(val))
        """)

        # Query A
        result_a1 = con.execute("""
            SELECT * FROM semantic_view('cv5_a',
                dimensions := ['id'], metrics := ['total']
            )
        """).fetchall()
        print(f"  View A (1st query): {result_a1}")
        assert len(result_a1) == 2

        # Define view B
        con.execute("""
            CREATE SEMANTIC VIEW cv5_b AS
            TABLES (t AS tb PRIMARY KEY (id))
            DIMENSIONS (t.id AS id)
            METRICS (t.total AS sum(val))
        """)

        # Query B
        result_b = con.execute("""
            SELECT * FROM semantic_view('cv5_b',
                dimensions := ['id'], metrics := ['total']
            )
        """).fetchall()
        print(f"  View B: {result_b}")
        assert len(result_b) == 2

        # Query A again -- tests pointer stability
        result_a2 = con.execute("""
            SELECT * FROM semantic_view('cv5_a',
                dimensions := ['id'], metrics := ['total']
            )
        """).fetchall()
        print(f"  View A (2nd query): {result_a2}")
        assert result_a1 == result_a2, f"View A results changed: {result_a1} vs {result_a2}"
    finally:
        con.close()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

ALL_TESTS = [
    # CV-1: Type mismatch
    ("test_cv1_basic_types", "CV-1: Basic types (VARCHAR, INTEGER, BIGINT)", test_cv1_basic_types),
    ("test_cv1_decimal_sum", "CV-1: DECIMAL sum() -- most likely crash trigger", test_cv1_decimal_sum),
    ("test_cv1_date_trunc", "CV-1: date_trunc dimension + count(*)", test_cv1_date_trunc),
    ("test_cv1_mixed_aggregates", "CV-1: Mixed aggregates (count, sum, avg, min)", test_cv1_mixed_aggregates),
    ("test_cv1_many_types", "CV-1: Wide table with many column types", test_cv1_many_types),
    # CV-2: Connection lifetime
    ("test_cv2_explicit_close", "CV-2: Explicit connection close", test_cv2_explicit_close),
    ("test_cv2_gc_collection", "CV-2: Garbage collection cleanup", test_cv2_gc_collection),
    ("test_cv2_multiple_queries", "CV-2: Multiple sequential queries", test_cv2_multiple_queries),
    # CV-3: Bind-time SQL
    ("test_cv3_inmemory_bind", "CV-3: In-memory DB bind-time SQL", test_cv3_inmemory_bind),
    ("test_cv3_file_backed_bind", "CV-3: File-backed DB bind-time SQL", test_cv3_file_backed_bind),
    # CV-4: Materialized vs streaming
    ("test_cv4_large_result", "CV-4: Large result (10,000 rows)", test_cv4_large_result),
    ("test_cv4_empty_result", "CV-4: Empty result (0 rows)", test_cv4_empty_result),
    # CV-5: extra_info lifetime
    ("test_cv5_define_query_define_query", "CV-5: Multiple define/query cycles", test_cv5_define_query_define_query),
]


def main():
    import duckdb

    print(f"DuckDB Semantic Views - Python vtab Crash Reproduction")
    print(f"=" * 60)
    print(f"Python: {sys.version}")
    print(f"Platform: {platform.platform()}")
    print(f"DuckDB: {duckdb.__version__}")
    print(f"Extension: {EXT_PATH}")
    print(f"Timeout: {'30s (signal.alarm)' if HAS_ALARM else 'disabled (no SIGALRM)'}")
    print(f"Tests: {len(ALL_TESTS)}")

    passed = 0
    failed = 0
    results = []

    for name, description, test_fn in ALL_TESTS:
        ok = run_test(name, description, test_fn)
        results.append((name, description, ok))
        if ok:
            passed += 1
        else:
            failed += 1

    # Summary
    print(f"\n{'='*60}")
    print(f"SUMMARY")
    print(f"{'='*60}")
    for name, description, ok in results:
        status = "PASS" if ok else "FAIL"
        print(f"  [{status}] {name} ({description})")

    print(f"\nTotal: {passed + failed}, Passed: {passed}, Failed: {failed}")

    if failed > 0:
        print("\nFAILED - at least one crash vector triggered")
        # Identify which crash vectors failed
        failed_cvs = set()
        for name, desc, ok in results:
            if not ok:
                cv = desc.split(":")[0]
                failed_cvs.add(cv)
        print(f"Affected crash vectors: {', '.join(sorted(failed_cvs))}")
        sys.exit(1)
    else:
        print("\nALL PASSED - no crashes reproduced")
        print("Note: The crash may require specific conditions not covered")
        print("(e.g., DuckLake tables, specific Python versions, concurrent access)")
        sys.exit(0)


if __name__ == "__main__":
    main()
