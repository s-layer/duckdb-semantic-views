#!/usr/bin/env python3
# /// script
# dependencies = [
#   "duckdb==2.0.0",
#   "adbc-driver-manager>=1.10",
#   "pyarrow>=16",
# ]
# requires-python = ">=3.10"
# ///
# Note: `import adbc_driver_duckdb` resolves to the module bundled inside
# the `duckdb` wheel (see its dist-info/RECORD). There is no
# separate `adbc-driver-duckdb` package on PyPI, so it does not appear in
# the dependencies list above.
"""
ADBC end-to-end transactional DDL test for the semantic_views extension.

Verifies that CREATE / DROP / ALTER SEMANTIC VIEW participate in the caller's
transaction when the connection is driven via ADBC's DBAPI 2.0 facade. ADBC's
DBAPI defaults to ``autocommit=False`` — every statement runs inside an
implicit transaction that is finalised by an explicit ``commit()`` or
``rollback()``. This was the original motivating bug for v0.8.0: pre-v0.8.0
the extension persisted DDL on its own auto-commit ``persist_conn`` /
``ddl_conn``, so an ADBC client that issued a DDL statement followed by
``rollback()`` would still find the view in the catalog after rollback.

Since v0.8.0 DDL lowers to native SQL that runs on the caller's connection
(through the grammar productions since Phase 3 of the PEG epic), so commit/rollback honour the transaction
boundary.

This test exercises:
    1. CREATE inline AS-body, then rollback() — view absent
    2. CREATE inline AS-body, then commit() — view present
    3. CREATE FROM YAML FILE, then rollback() — view absent
    4. CREATE FROM YAML FILE, then commit() — view present
    5. ALTER RENAME, then rollback() — original name still present
    6. DROP, then rollback() — view still present

Usage:
    just test-adbc

Exit codes:
    0 = all assertions passed
    1 = test failure
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

# The `duckdb` module wires up extension loading in the shared library it and
# the ADBC driver live in. On DuckDB 2.0 the driver does not do that itself, so
# without this import INSTALL and LOAD fail with "this build does not link the
# loadable_extensions library" (TECH-DEBT #89).
import duckdb  # noqa: F401
import adbc_driver_duckdb
import adbc_driver_manager
import adbc_driver_manager.dbapi


def _connect_adbc(db_path: str, extension_dir: str):
    """
    Open an ADBC DBAPI connection to a DuckDB file with ``allow_unsigned_extensions``
    and a project-local extension directory pre-set on the underlying DBConfig.

    The high-level ``adbc_driver_duckdb.dbapi.connect`` does not expose DBConfig
    options, so we drop down to ``adbc_driver_manager.AdbcDatabase`` directly and
    pass arbitrary keyword arguments — DuckDB's ADBC driver routes any key it
    does not recognise specifically through ``duckdb_set_config``.

    ``autocommit=False`` is the ADBC DBAPI default; we set it explicitly here so
    the intent is visible in the test.
    """
    db = adbc_driver_manager.AdbcDatabase(
        driver=adbc_driver_duckdb.driver_path(),
        entrypoint="duckdb_adbc_init",
        path=db_path,
        allow_unsigned_extensions="true",
        extension_directory=extension_dir,
    )
    conn = adbc_driver_manager.AdbcConnection(db)
    return adbc_driver_manager.dbapi.Connection(db, conn, autocommit=False)


def _execute(conn, sql: str) -> None:
    with conn.cursor() as cur:
        cur.execute(sql)


def _scalar(conn, sql: str):
    with conn.cursor() as cur:
        cur.execute(sql)
        row = cur.fetchone()
    return None if row is None else row[0]


def run_tests() -> int:
    extension_path = get_extension_path()
    if not extension_path.exists():
        print(f"ERROR: extension not found at {extension_path}")
        print("Run `just build` first.")
        return 1

    ext_dir = get_ext_dir()
    passed = 0
    failed = 0

    with tempfile.TemporaryDirectory(prefix="sv_adbc_") as tmp:
        tmp_path = Path(tmp)
        db_path = str(tmp_path / "adbc.duckdb")
        yaml_path = tmp_path / "view.yaml"
        yaml_path.write_text(
            "tables:\n"
            "  - alias: o\n"
            "    table: adbc_orders\n"
            "    pk_columns:\n"
            "      - id\n"
            "dimensions:\n"
            "  - name: region\n"
            "    expr: o.region\n"
            "    source_table: o\n"
            "metrics:\n"
            "  - name: total\n"
            "    expr: SUM(o.amount)\n"
            "    source_table: o\n"
        )

        conn = _connect_adbc(db_path, ext_dir)
        try:
            # Load the extension via SQL. The connection is in a transaction
            # the moment we issue any statement; commit so subsequent DDL/DML
            # tests start from a clean transactional slate.
            _execute(conn, f"FORCE INSTALL '{extension_path}'")
            _execute(conn, "LOAD semantic_views")
            _execute(
                conn,
                "CREATE TABLE adbc_orders (id INTEGER PRIMARY KEY, "
                "amount DECIMAL(10,2), region VARCHAR)",
            )
            _execute(
                conn,
                "INSERT INTO adbc_orders VALUES (1, 100.00, 'US'), (2, 200.00, 'EU')",
            )
            conn.commit()

            create_inline = """
                CREATE SEMANTIC VIEW adbc_inline AS
                  TABLES (o AS adbc_orders PRIMARY KEY (id))
                  DIMENSIONS (o.region AS o.region)
                  METRICS (o.total AS SUM(o.amount))
            """

            # ---- Test 1: CREATE inline + rollback ----
            print("Test 1: CREATE (inline) + rollback() — view must not persist")
            try:
                _execute(conn, create_inline)
                conn.rollback()
                count = _scalar(
                    conn,
                    "SELECT count(*) FROM list_semantic_views() WHERE name = 'adbc_inline'",
                )
                assert count == 0, f"expected 0 views after rollback, got {count}"
                conn.commit()
                print("  PASS")
                passed += 1
            except Exception as e:
                print(f"  FAIL: {e}")
                conn.rollback()
                failed += 1

            # ---- Test 2: CREATE inline + commit ----
            print("Test 2: CREATE (inline) + commit() — view must persist")
            try:
                _execute(conn, create_inline)
                conn.commit()
                count = _scalar(
                    conn,
                    "SELECT count(*) FROM list_semantic_views() WHERE name = 'adbc_inline'",
                )
                assert count == 1, f"expected 1 view after commit, got {count}"
                conn.commit()
                print("  PASS")
                passed += 1
            except Exception as e:
                print(f"  FAIL: {e}")
                conn.rollback()
                failed += 1

            create_yaml = (
                f"CREATE SEMANTIC VIEW adbc_yaml FROM YAML FILE '{yaml_path}'"
            )

            # ---- Test 3: CREATE FROM YAML FILE + rollback ----
            print("Test 3: CREATE FROM YAML FILE + rollback() — view must not persist")
            try:
                _execute(conn, create_yaml)
                conn.rollback()
                count = _scalar(
                    conn,
                    "SELECT count(*) FROM list_semantic_views() WHERE name = 'adbc_yaml'",
                )
                assert count == 0, f"expected 0 views after rollback, got {count}"
                conn.commit()
                print("  PASS")
                passed += 1
            except Exception as e:
                print(f"  FAIL: {e}")
                conn.rollback()
                failed += 1

            # ---- Test 4: CREATE FROM YAML FILE + commit ----
            print("Test 4: CREATE FROM YAML FILE + commit() — view must persist")
            try:
                _execute(conn, create_yaml)
                conn.commit()
                count = _scalar(
                    conn,
                    "SELECT count(*) FROM list_semantic_views() WHERE name = 'adbc_yaml'",
                )
                assert count == 1, f"expected 1 view after commit, got {count}"
                conn.commit()
                print("  PASS")
                passed += 1
            except Exception as e:
                print(f"  FAIL: {e}")
                conn.rollback()
                failed += 1

            # ---- Test 5: ALTER RENAME + rollback ----
            print("Test 5: ALTER RENAME + rollback() — original name must remain")
            try:
                _execute(
                    conn, "ALTER SEMANTIC VIEW adbc_inline RENAME TO adbc_renamed"
                )
                conn.rollback()
                src = _scalar(
                    conn,
                    "SELECT count(*) FROM list_semantic_views() WHERE name = 'adbc_inline'",
                )
                dst = _scalar(
                    conn,
                    "SELECT count(*) FROM list_semantic_views() WHERE name = 'adbc_renamed'",
                )
                assert src == 1 and dst == 0, (
                    f"expected src=1, dst=0 after rollback; got src={src}, dst={dst}"
                )
                conn.commit()
                print("  PASS")
                passed += 1
            except Exception as e:
                print(f"  FAIL: {e}")
                conn.rollback()
                failed += 1

            # ---- Test 6: DROP + rollback ----
            print("Test 6: DROP + rollback() — view must remain")
            try:
                _execute(conn, "DROP SEMANTIC VIEW adbc_inline")
                conn.rollback()
                count = _scalar(
                    conn,
                    "SELECT count(*) FROM list_semantic_views() WHERE name = 'adbc_inline'",
                )
                assert count == 1, f"expected 1 view after rollback, got {count}"
                conn.commit()
                print("  PASS")
                passed += 1
            except Exception as e:
                print(f"  FAIL: {e}")
                conn.rollback()
                failed += 1
        finally:
            conn.close()

    print()
    print(f"Results: {passed} passed, {failed} failed")
    return 0 if failed == 0 else 1


def main() -> None:
    sys.exit(run_tests())


if __name__ == "__main__":
    main()
