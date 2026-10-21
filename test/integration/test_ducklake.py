#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
DuckLake/Iceberg integration test for the semantic_views extension.

Verifies that semantic_view works correctly against DuckLake-managed
tables (which use the Iceberg table format under the hood). This proves
the extension handles non-standard table sources end-to-end.

Uses native CREATE SEMANTIC VIEW ... AS ... DDL syntax exclusively.
Query interface (semantic_view() / explain_semantic_view()) is unchanged.

Test cases:
    1. Define semantic view over DuckLake/Iceberg table (native DDL)
    2. Query the DuckLake-backed semantic view with dimension
    3. Metrics-only query (global aggregate)
    4. Explain on DuckLake-backed view
    5. Typed BIGINT output — count(*) returns Python int, not str
    6. Date dimension with date_trunc — ordered_at returns datetime.date values

Prerequisites:
    Run `just setup-ducklake` first to download jaffle-shop data and
    create the DuckLake catalog.

Usage:
    uv run test/integration/test_ducklake.py

    Or via Justfile:
    just test-iceberg

Exit codes:
    0 = all assertions passed
    1 = test failure or setup error

Requirement: TEST-04 (integration test with Apache Iceberg table source)
"""

import datetime
import sys
from pathlib import Path

# Add test/integration to path for helpers import
sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import (
    attach_ducklake,
    get_ext_dir,
    get_extension_path,
    get_project_root,
    load_extension,
)

PROJECT_ROOT = get_project_root()

# DuckLake catalog paths (must match setup_ducklake.py)
DATA_DIR = PROJECT_ROOT / "test" / "data"
CATALOG_DB = DATA_DIR / "test_catalog.duckdb"
DUCKLAKE_FILE = DATA_DIR / "jaffle.ducklake"
JAFFLE_DATA_DIR = DATA_DIR / "jaffle_data"

# Extension path (resolved via helpers)
EXTENSION_PATH = get_extension_path()


def check_prerequisites():
    """Verify all required files exist."""
    missing = []
    if not CATALOG_DB.exists():
        missing.append(str(CATALOG_DB))
    if not DUCKLAKE_FILE.exists():
        missing.append(str(DUCKLAKE_FILE))
    if not EXTENSION_PATH.exists():
        missing.append(str(EXTENSION_PATH))

    if missing:
        print("ERROR: Missing prerequisites:")
        for m in missing:
            print(f"  - {m}")
        print()
        print("Run the following first:")
        print("  just build          # Build the extension")
        print("  just setup-ducklake # Download data and create DuckLake catalog")
        sys.exit(1)


def run_tests():
    """Run the DuckLake integration tests."""
    import duckdb

    print(f"DuckDB version: {duckdb.__version__}")
    print(f"Extension: {EXTENSION_PATH}")
    print(f"DuckLake catalog: {DUCKLAKE_FILE}")
    print()

    # Connect to the catalog database (which has DuckLake already set up).
    ext_dir = get_ext_dir()
    con = duckdb.connect(
        str(CATALOG_DB),
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": ext_dir,
        },
    )

    # Load extensions and attach DuckLake catalog
    load_extension(con, EXTENSION_PATH)
    attach_ducklake(con, str(DUCKLAKE_FILE), str(JAFFLE_DATA_DIR) + "/", alias="jaffle")

    # Verify DuckLake tables are accessible
    tables = con.execute(
        "SELECT table_name FROM information_schema.tables "
        "WHERE table_catalog = 'jaffle' ORDER BY table_name"
    ).fetchall()
    table_names = [t[0] for t in tables]
    print(f"DuckLake tables: {table_names}")
    assert "raw_orders" in table_names, "raw_orders table not found in DuckLake"
    assert "raw_customers" in table_names, "raw_customers table not found in DuckLake"

    passed = 0
    failed = 0

    # ---- Test 1: Define semantic view over DuckLake table ----
    # Actual jaffle-shop raw_orders columns: id, customer, ordered_at,
    # store_id, subtotal, tax_paid, order_total
    print()
    print("Test 1: Define semantic view over DuckLake/Iceberg table")
    try:
        con.execute(
            """
            CREATE SEMANTIC VIEW jaffle_orders AS
            TABLES (o AS jaffle.raw_orders PRIMARY KEY (id))
            DIMENSIONS (
                o.store_id AS store_id,
                o.ordered_at AS date_trunc('day', ordered_at)
            )
            METRICS (
                o.order_count AS count(*),
                o.total_revenue AS sum(order_total)
            )
            """
        )
        print("  PASS: View defined successfully")
        passed += 1
    except Exception as e:
        print(f"  FAIL: {e}")
        failed += 1

    # ---- Test 2: Query the DuckLake-backed semantic view ----
    print()
    print("Test 2: Query DuckLake-backed semantic view")
    try:
        result = con.execute(
            """
            SELECT * FROM semantic_view(
                'jaffle_orders',
                dimensions := ['store_id'],
                metrics := ['order_count']
            )
            """
        ).fetchall()
        print(f"  Result: {result}")
        assert len(result) > 0, "Expected at least one row"
        store_ids = {row[0] for row in result}
        print(f"  Store IDs found: {len(store_ids)}")
        assert len(store_ids) > 0, "Expected at least one distinct store_id"
        print("  PASS: Query returned correct results")
        passed += 1
    except Exception as e:
        print(f"  FAIL: {e}")
        failed += 1

    # ---- Test 3: Metrics-only query (global aggregate) ----
    print()
    print("Test 3: Global aggregate over DuckLake table")
    try:
        result = con.execute(
            """
            SELECT * FROM semantic_view(
                'jaffle_orders',
                metrics := ['order_count', 'total_revenue']
            )
            """
        ).fetchall()
        print(f"  Result: {result}")
        assert len(result) == 1, f"Expected 1 row, got {len(result)}"
        order_count = int(result[0][0])
        total_revenue = int(result[0][1])
        assert order_count > 0, f"Expected positive order_count, got {order_count}"
        assert total_revenue > 0, f"Expected positive total_revenue, got {total_revenue}"
        print(f"  Total orders: {order_count}, Total revenue: {total_revenue}")
        print("  PASS: Global aggregate returned correct results")
        passed += 1
    except Exception as e:
        print(f"  FAIL: {e}")
        failed += 1

    # ---- Test 4: Explain on DuckLake-backed view ----
    print()
    print("Test 4: Explain on DuckLake-backed semantic view")
    try:
        result = con.execute(
            """
            SELECT * FROM explain_semantic_view(
                'jaffle_orders',
                dimensions := ['store_id'],
                metrics := ['order_count']
            )
            """
        ).fetchall()
        lines = [row[0] for row in result]
        explain_text = "\n".join(lines)
        assert "jaffle_orders" in explain_text, "Expected view name in explain output"
        assert "raw_orders" in explain_text, "Expected base table in explain output"
        print(f"  Explain output: {len(lines)} lines")
        print("  PASS: Explain output contains expected metadata")
        passed += 1
    except Exception as e:
        print(f"  FAIL: {e}")
        failed += 1

    # ---- Test 5: Typed BIGINT output ----
    print()
    print("Test 5: Typed BIGINT output for count(*) metric")
    try:
        result = con.execute(
            """
            SELECT * FROM semantic_view('jaffle_orders', metrics := ['order_count'])
            """
        ).fetchall()
        assert len(result) == 1, f"Expected 1 row, got {len(result)}"
        order_count_val = result[0][0]
        assert isinstance(order_count_val, int), (
            f"Expected int (BIGINT) for order_count, got "
            f"{type(order_count_val).__name__}: {order_count_val!r}"
        )
        print(f"  order_count type: {type(order_count_val).__name__} = {order_count_val}")
        print("  PASS: count(*) returns BIGINT (Python int)")
        passed += 1
    except Exception as e:
        print(f"  FAIL: {e}")
        failed += 1

    # ---- Test 6: Date dimension with date_trunc ----
    print()
    print("Test 6: Date dimension with date_trunc('day', ...)")
    try:
        result = con.execute(
            """
            SELECT * FROM semantic_view(
                'jaffle_orders',
                dimensions := ['ordered_at'],
                metrics := ['order_count']
            )
            """
        ).fetchall()
        assert len(result) > 0, "Expected at least one row"
        # All date values should be datetime.date instances
        for row in result:
            assert isinstance(row[0], datetime.date), (
                f"Expected datetime.date for ordered_at, got {type(row[0]).__name__}: {row[0]!r}"
            )
        print(f"  Date dimension rows: {len(result)} distinct days")
        print("  PASS: ordered_at returns datetime.date values")
        passed += 1
    except Exception as e:
        print(f"  FAIL: {e}")
        failed += 1

    # ---- Cleanup ----
    print()
    print("Cleanup: dropping semantic view")
    try:
        con.execute("DROP SEMANTIC VIEW jaffle_orders")
    except Exception:
        pass  # Best-effort cleanup

    con.close()

    # ---- Summary ----
    print()
    print(f"Results: {passed} passed, {failed} failed, {passed + failed} total")
    if failed > 0:
        print("FAILED")
        sys.exit(1)
    else:
        print("ALL PASSED")


def main():
    check_prerequisites()
    run_tests()


if __name__ == "__main__":
    main()
