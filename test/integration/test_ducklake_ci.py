#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
DuckLake CI integration test for the semantic_views extension.

Verifies that semantic_view works correctly against DuckLake-managed
tables using entirely inline synthetic data. No jaffle-shop data download
or setup step required -- suitable for CI/CD environments.

Uses native CREATE SEMANTIC VIEW ... AS ... DDL syntax exclusively.
Query interface (semantic_view() / explain_semantic_view()) is unchanged.

Creates a DuckLake catalog in a temp directory, inserts known synthetic
rows, and asserts specific expected outputs (including typed BIGINT output
and date dimension with date_trunc).

Usage:
    uv run test/integration/test_ducklake_ci.py

    Or with a custom extension path:
    SEMANTIC_VIEWS_EXTENSION_PATH=target/debug/semantic_views.duckdb_extension \
        uv run test/integration/test_ducklake_ci.py

Exit codes:
    0 = all assertions passed
    1 = test failure or setup error

Test cases:
    1. Define semantic view over DuckLake table (native DDL)
    2. Query with dimension (store_id)
    3. Global aggregate (order_count, total_revenue)
    4. Explain on DuckLake-backed view
    5. Typed BIGINT output — count(*) returns Python int, not str
    6. Date dimension with date_trunc — ordered_at truncated to known dates

Requirement: DuckLake integration test (CI variant, inline synthetic data)
"""

import datetime
import os
import shutil
import sys
import tempfile
from pathlib import Path

# Add test/integration to path for helpers import
sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import (
    attach_ducklake,
    exit_skipped_if_offline,
    get_ext_dir,
    get_extension_path,
    load_extension,
)


def setup_synthetic_ducklake(ext_dir: str) -> tuple:
    """
    Create a DuckLake catalog in a temp directory with synthetic jaffle-shop data.

    Mirrors the raw_orders schema from the real jaffle-shop dataset with
    5 rows of known values for deterministic assertions.

    Returns:
        Tuple of (tmpdir, catalog_path, ducklake_file, data_dir)
        where data_dir ends with '/'.
    """
    import duckdb

    tmpdir = tempfile.mkdtemp()
    ducklake_file = os.path.join(tmpdir, "test.ducklake")
    data_dir = os.path.join(tmpdir, "data") + "/"
    os.makedirs(data_dir, exist_ok=True)
    catalog_path = os.path.join(tmpdir, "catalog.duckdb")

    # Create the DuckLake catalog with synthetic data
    catalog_con = duckdb.connect(
        catalog_path,
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": ext_dir,
        },
    )

    try:
        try:
            catalog_con.execute("INSTALL ducklake")
        except duckdb.IOException as e:
            catalog_con.close()
            shutil.rmtree(tmpdir, ignore_errors=True)
            exit_skipped_if_offline("test/integration/test_ducklake_ci.py", e)
            raise
        catalog_con.execute("LOAD ducklake")
        catalog_con.execute(f"ATTACH 'ducklake:{ducklake_file}' AS jaffle (DATA_PATH '{data_dir}')")

        # Mirror real jaffle-shop raw_orders schema
        catalog_con.execute(
            """
            CREATE TABLE jaffle.raw_orders (
                id INTEGER,
                customer VARCHAR,
                ordered_at DATE,
                store_id INTEGER,
                subtotal INTEGER,
                tax_paid INTEGER,
                order_total INTEGER
            )
            """
        )

        # 5 rows with known values — 3 distinct dates, 2 distinct store IDs
        # ordered_at dates: 2024-01-15 (2 rows), 2024-02-10 (2 rows), 2024-03-05 (1 row)
        catalog_con.execute(
            """
            INSERT INTO jaffle.raw_orders VALUES
                (1, 'alice',   '2024-01-15', 1, 1000,  80, 1080),
                (2, 'bob',     '2024-01-15', 2, 2000, 160, 2160),
                (3, 'alice',   '2024-02-10', 1,  500,  40,  540),
                (4, 'charlie', '2024-02-10', 2, 1500, 120, 1620),
                (5, 'bob',     '2024-03-05', 1,  800,  64,  864)
            """
        )
    finally:
        catalog_con.close()

    return (tmpdir, catalog_path, ducklake_file, data_dir)


def run_tests() -> None:
    """Run the DuckLake CI integration tests with inline synthetic data."""
    import duckdb

    print(f"DuckDB version: {duckdb.__version__}")

    ext_dir = get_ext_dir()
    ext_path = get_extension_path()
    print(f"Extension: {ext_path}")
    print()

    tmpdir, catalog_path, ducklake_file, data_dir = setup_synthetic_ducklake(ext_dir)
    con = None

    try:
        con = duckdb.connect(
            catalog_path,
            config={
                "allow_unsigned_extensions": "true",
                "extension_directory": ext_dir,
            },
        )
        load_extension(con, ext_path)
        attach_ducklake(con, ducklake_file, data_dir, alias="jaffle")

        # Sanity check: raw_orders should be visible
        tables = con.execute(
            "SELECT table_name FROM information_schema.tables "
            "WHERE table_catalog = 'jaffle' ORDER BY table_name"
        ).fetchall()
        table_names = [t[0] for t in tables]
        assert "raw_orders" in table_names, f"raw_orders not found in DuckLake: {table_names}"
        print(f"DuckLake tables: {table_names}")

        passed = 0
        failed = 0

        # ---- Test 1: Define semantic view over DuckLake table ----
        print()
        print("Test 1: Define semantic view over DuckLake/synthetic table")
        try:
            con.execute(
                """
                CREATE SEMANTIC VIEW ci_orders AS
                TABLES (o AS jaffle.raw_orders PRIMARY KEY (id))
                DIMENSIONS (
                    o.store_id AS store_id,
                    o.customer AS customer,
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

        # ---- Test 2: Query with dimension (store_id) ----
        print()
        print("Test 2: Query DuckLake-backed semantic view with dimension")
        try:
            result = con.execute(
                """
                SELECT * FROM semantic_view(
                    'ci_orders',
                    dimensions := ['store_id'],
                    metrics := ['order_count']
                )
                """
            ).fetchall()
            assert len(result) > 0, "Expected at least one row"
            store_ids = {row[0] for row in result}
            assert len(store_ids) >= 2, f"Expected at least 2 distinct store_ids, got: {store_ids}"
            print(f"  Result: {result}")
            print(f"  Store IDs found: {store_ids}")
            print("  PASS: Query returned correct results")
            passed += 1
        except Exception as e:
            print(f"  FAIL: {e}")
            failed += 1

        # ---- Test 3: Global aggregate ----
        print()
        print("Test 3: Global aggregate over DuckLake table")
        try:
            result = con.execute(
                """
                SELECT * FROM semantic_view(
                    'ci_orders',
                    metrics := ['order_count', 'total_revenue']
                )
                """
            ).fetchall()
            assert len(result) == 1, f"Expected 1 row, got {len(result)}"
            order_count = int(result[0][0])
            assert order_count == 5, f"Expected 5 rows (all synthetic rows), got {order_count}"
            total_revenue = result[0][1]
            assert total_revenue, f"Expected positive total_revenue, got {total_revenue}"
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
                    'ci_orders',
                    dimensions := ['store_id'],
                    metrics := ['order_count']
                )
                """
            ).fetchall()
            lines = [row[0] for row in result]
            explain_text = "\n".join(lines)
            assert "ci_orders" in explain_text, "Expected view name in explain output"
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
                SELECT * FROM semantic_view('ci_orders', metrics := ['order_count'])
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
                    'ci_orders',
                    dimensions := ['ordered_at'],
                    metrics := ['order_count']
                )
                """
            ).fetchall()
            assert len(result) > 0, "Expected at least one row"

            # DuckDB >= 1.5.0: date_trunc returns TIMESTAMP (datetime.datetime).
            # DuckDB < 1.5.0: date_trunc on DATE returned DATE (datetime.date).
            # Accept both datetime.date and datetime.datetime for forward compat.
            for row in result:
                assert isinstance(row[0], (datetime.date, datetime.datetime)), (
                    f"Expected datetime.date or datetime.datetime for ordered_at, got "
                    f"{type(row[0]).__name__}: {row[0]!r}"
                )

            # Normalize all values to datetime.datetime for comparison.
            # datetime.date(2024,1,15) -> datetime.datetime(2024,1,15,0,0)
            dates = set()
            for row in result:
                val = row[0]
                if isinstance(val, datetime.datetime):
                    dates.add(val)
                else:
                    dates.add(datetime.datetime(val.year, val.month, val.day))

            # 5 rows with 3 distinct dates:
            #   2024-01-15: 2 rows -> 1 group
            #   2024-02-10: 2 rows -> 1 group
            #   2024-03-05: 1 row  -> 1 group
            assert datetime.datetime(2024, 1, 15) in dates, f"Expected 2024-01-15 in {dates}"
            assert datetime.datetime(2024, 2, 10) in dates, f"Expected 2024-02-10 in {dates}"
            assert datetime.datetime(2024, 3, 5) in dates, f"Expected 2024-03-05 in {dates}"
            assert len(result) == 3, f"Expected 3 distinct day groups, got {len(result)}: {result}"

            print(f"  Date dimension rows: {result}")
            print("  PASS: ordered_at returns date/datetime values with correct day truncation")
            passed += 1
        except Exception as e:
            print(f"  FAIL: {e}")
            failed += 1

        # ---- Cleanup ----
        print()
        print("Cleanup: dropping semantic view")
        try:
            con.execute("DROP SEMANTIC VIEW ci_orders")
        except Exception:
            pass  # Best-effort cleanup

    finally:
        if con is not None:
            con.close()
        shutil.rmtree(tmpdir, ignore_errors=True)

    # ---- Summary ----
    print()
    print(f"Results: {passed} passed, {failed} failed, {passed + failed} total")
    if failed > 0:
        print("FAILED")
        sys.exit(1)
    else:
        print("ALL PASSED")


def main() -> None:
    run_tests()


if __name__ == "__main__":
    main()
