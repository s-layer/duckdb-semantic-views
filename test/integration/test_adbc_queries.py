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
ADBC end-to-end SELECT FROM semantic_view(...) regression test (EXPAND-CTX-02).

This file exercises seven scenarios that together cover every code path which
emits a physical table reference during semantic-view expansion. Phase 66
migrated the previously unmigrated call sites in
``src/expand/{sql_gen,semi_additive,window,materialization}.rs`` from
``quote_table_ref`` to ``qualify_and_quote_table_ref``; this test is the
regression guard against re-introduction of unqualified emission.

Scenarios (per Phase 66 CONTEXT.md D-08):

    1. main expansion path, default schema (memory.main)
    2. main expansion path, non-default schema (staging)
    3. FACTS feature path, non-default schema base table
    4. semi-additive metric, non-default schema base table
    5. window metric, non-default schema base table
    6. materialization routing to non-default-schema target
    7. multi-DB ATTACH + FACTS metric on attached DB's table

Sandbox note (CLAUDE.md Rule 2)
--------------------------------
``tempfile.TemporaryDirectory(prefix="sv_adbc_q_")`` writes to
``/var/folders/.../T/`` on macOS, which may trigger
``mktemp: mkstemp failed ... Operation not permitted`` under the sandbox.
The pre-approved bypass for ``uv run test/integration/*.py`` applies; see
CLAUDE.md Rule 2 for the literal pattern.

Usage:
    just test-adbc-queries

Exit codes:
    0 = all scenarios passed
    1 = at least one scenario failed
"""

from __future__ import annotations

import sys
import tempfile
import traceback
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

    Mirrors ``test_adbc_transactions.py::_connect_adbc`` (lines 60-81).
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


def _bootstrap_extension(conn, extension_path: Path) -> None:
    """Install + load the extension on a fresh ADBC connection, then commit."""
    # IN-02: escape single-quotes for SQL string literal (path may contain '
    # though project-internal paths today don't). DuckDB-recognised escape
    # inside a single-quoted string literal is doubling the single quote.
    extension_path_sql = str(extension_path).replace("'", "''")
    _execute(conn, f"FORCE INSTALL '{extension_path_sql}'")
    _execute(conn, "LOAD semantic_views")
    conn.commit()


# --------------------------------------------------------------------------
# Scenarios
# --------------------------------------------------------------------------


def test_main_path_default_schema(extension_path: Path, ext_dir: str, tmp_path: Path) -> None:
    """Scenario 1 — main expand path, default schema (memory.main). ACTIVE.

    Baseline: a semantic view created in memory.main referencing a base table
    in memory.main. Both qualified and unqualified emission resolve here, so
    this scenario PASSES regardless of migration state. It guards against
    regressions in the v0.9.0-wired main expand path (sql_gen.rs:499,530,550).
    """
    db_path = str(tmp_path / "scenario1.duckdb")
    conn = _connect_adbc(db_path, ext_dir)
    try:
        _bootstrap_extension(conn, extension_path)

        _execute(
            conn,
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, "
            "region VARCHAR, amount DECIMAL(10,2))",
        )
        _execute(
            conn,
            "INSERT INTO orders VALUES (1, 'US', 100.00), (2, 'EU', 200.00)",
        )
        _execute(
            conn,
            """
            CREATE SEMANTIC VIEW v_default AS
              TABLES (o AS orders PRIMARY KEY (id))
              DIMENSIONS (o.region AS o.region)
              METRICS (o.total AS SUM(o.amount))
            """,
        )
        conn.commit()

        rows = _scalar(
            conn,
            "SELECT COUNT(*) FROM semantic_view('v_default', "
            "dimensions := ['region'], metrics := ['total'])",
        )
        assert rows == 2, f"expected 2 rows, got {rows}"
    finally:
        conn.close()


def test_main_path_non_default_schema(extension_path: Path, ext_dir: str, tmp_path: Path) -> None:
    """Scenario 2 — main expand path, non-default schema base table. ACTIVE.

    The base table lives in ``staging`` rather than ``main``. The main expand
    path was wired to ``qualify_and_quote_table_ref`` by Phase 64
    (sql_gen.rs:499,530,550), so this passes today.
    """
    db_path = str(tmp_path / "scenario2.duckdb")
    conn = _connect_adbc(db_path, ext_dir)
    try:
        _bootstrap_extension(conn, extension_path)

        _execute(conn, "CREATE SCHEMA staging")
        _execute(
            conn,
            "CREATE TABLE staging.t (id INTEGER PRIMARY KEY, "
            "region VARCHAR, amount DECIMAL(10,2))",
        )
        _execute(
            conn,
            "INSERT INTO staging.t VALUES (1, 'US', 100.00), (2, 'EU', 200.00)",
        )
        _execute(
            conn,
            """
            CREATE SEMANTIC VIEW v_staging AS
              TABLES (x AS staging.t PRIMARY KEY (id))
              DIMENSIONS (x.region AS x.region)
              METRICS (x.cnt AS COUNT(*))
            """,
        )
        conn.commit()

        rows = _scalar(
            conn,
            "SELECT COUNT(*) FROM semantic_view('v_staging', "
            "dimensions := ['region'], metrics := ['cnt'])",
        )
        assert rows == 2, f"expected 2 rows, got {rows}"
    finally:
        conn.close()


def test_facts_non_default_schema(extension_path: Path, ext_dir: str, tmp_path: Path) -> None:
    """Scenario 3 — FACTS path, non-default schema base table. ACTIVE.

    Regression guard for ``src/expand/sql_gen.rs:181, 224, 244`` (fact-query
    path). After the Phase 66 migration these sites use
    ``qualify_and_quote_table_ref``; this scenario fails with
    ``Catalog Error: Table with name sales does not exist!`` if a regression
    re-introduces unqualified emission.
    """
    db_path = str(tmp_path / "scenario3.duckdb")
    conn = _connect_adbc(db_path, ext_dir)
    try:
        _bootstrap_extension(conn, extension_path)

        _execute(conn, "CREATE SCHEMA staging")
        _execute(
            conn,
            "CREATE TABLE staging.sales (id INTEGER PRIMARY KEY, "
            "region VARCHAR, amount DECIMAL(10,2))",
        )
        _execute(
            conn,
            "INSERT INTO staging.sales VALUES (1, 'US', 100.00), (2, 'EU', 200.00)",
        )
        _execute(
            conn,
            """
            CREATE SEMANTIC VIEW staging_view AS
              TABLES (s AS staging.sales PRIMARY KEY (id))
              FACTS (s.net_amount AS s.amount * 1.0)
              DIMENSIONS (s.region AS s.region)
""",
        )
        conn.commit()

        rows = _scalar(
            conn,
            "SELECT COUNT(*) FROM semantic_view('staging_view', "
            "facts := ['region', 'net_amount'])",
        )
        assert rows == 2, f"expected 2 rows, got {rows}"
    finally:
        conn.close()


def test_semi_additive_non_default_schema(extension_path: Path, ext_dir: str, tmp_path: Path) -> None:
    """Scenario 4 — semi-additive metric, non-default schema base table. ACTIVE.

    Regression guard for ``src/expand/semi_additive.rs:195, 220, 238``.
    A ``MIN_BY(qty, snapshot_date)`` semi-additive metric emits inner
    subqueries that, post-migration, use ``qualify_and_quote_table_ref``;
    a regression to ``quote_table_ref`` would fail on the non-default-schema
    base table.
    """
    db_path = str(tmp_path / "scenario4.duckdb")
    conn = _connect_adbc(db_path, ext_dir)
    try:
        _bootstrap_extension(conn, extension_path)

        _execute(conn, "CREATE SCHEMA staging")
        _execute(
            conn,
            "CREATE TABLE staging.inventory ("
            "id INTEGER PRIMARY KEY, "
            "warehouse VARCHAR, "
            "snapshot_date DATE, "
            "qty INTEGER)",
        )
        _execute(
            conn,
            "INSERT INTO staging.inventory VALUES "
            "(1, 'WH1', DATE '2026-01-01', 10), "
            "(2, 'WH1', DATE '2026-01-02', 15), "
            "(3, 'WH2', DATE '2026-01-01', 20), "
            "(4, 'WH2', DATE '2026-01-02', 25)",
        )
        _execute(
            conn,
            """
            CREATE SEMANTIC VIEW inv_view AS
              TABLES (i AS staging.inventory PRIMARY KEY (id))
              DIMENSIONS (i.warehouse AS i.warehouse)
              METRICS (i.earliest_qty AS MIN_BY(i.qty, i.snapshot_date))
            """,
        )
        conn.commit()

        rows = _scalar(
            conn,
            "SELECT COUNT(*) FROM semantic_view('inv_view', "
            "dimensions := ['warehouse'], metrics := ['earliest_qty'])",
        )
        assert rows == 2, f"expected 2 rows, got {rows}"
    finally:
        conn.close()


def test_window_non_default_schema(extension_path: Path, ext_dir: str, tmp_path: Path) -> None:
    """Scenario 5 — window metric, non-default schema base table. ACTIVE.

    Regression guard for ``src/expand/window.rs:156, 181, 199``. Window-metric
    inner CTEs use ``qualify_and_quote_table_ref`` post-migration; a regression
    to ``quote_table_ref`` would fail on the non-default-schema base table.
    """
    db_path = str(tmp_path / "scenario5.duckdb")
    conn = _connect_adbc(db_path, ext_dir)
    try:
        _bootstrap_extension(conn, extension_path)

        _execute(conn, "CREATE SCHEMA staging")
        _execute(
            conn,
            "CREATE TABLE staging.events ("
            "id INTEGER PRIMARY KEY, "
            "user_id INTEGER, "
            "event_time TIMESTAMP, "
            "amount DECIMAL(10,2))",
        )
        _execute(
            conn,
            "INSERT INTO staging.events VALUES "
            "(1, 1, TIMESTAMP '2026-01-01 10:00:00', 10.00), "
            "(2, 1, TIMESTAMP '2026-01-01 11:00:00', 20.00), "
            "(3, 2, TIMESTAMP '2026-01-01 10:00:00', 30.00)",
        )
        _execute(
            conn,
            """
            CREATE SEMANTIC VIEW evt_view AS
              TABLES (e AS staging.events PRIMARY KEY (id))
              DIMENSIONS (e.user_id AS e.user_id, e.event_time AS e.event_time)
              METRICS (
                PRIVATE e.total_amount AS SUM(e.amount),
                e.running_avg AS AVG(total_amount) OVER (PARTITION BY EXCLUDING event_time ORDER BY event_time ASC NULLS LAST)
              )
""",
        )
        conn.commit()

        rows = _scalar(
            conn,
            "SELECT COUNT(*) FROM semantic_view('evt_view', "
            "dimensions := ['user_id', 'event_time'], metrics := ['running_avg'])",
        )
        # Window metric emits one row per (user_id, event_time) group: 3 distinct
        # source rows → 3 result rows.
        assert rows == 3, f"expected 3 rows, got {rows}"
    finally:
        conn.close()


def test_materialization_routing_non_default_schema_target(
    extension_path: Path, ext_dir: str, tmp_path: Path
) -> None:
    """Scenario 6 — materialization routing to non-default-schema target. ACTIVE.

    Regression guard for ``src/expand/materialization.rs:157``. The
    materialization's ``target_table => 'agg.daily_revenue'`` is emitted
    fully qualified post-migration; a regression to unqualified emission
    would cause the routed query to fail catalog resolution.
    """
    db_path = str(tmp_path / "scenario6.duckdb")
    conn = _connect_adbc(db_path, ext_dir)
    try:
        _bootstrap_extension(conn, extension_path)

        # Base table in default schema
        _execute(
            conn,
            "CREATE TABLE sales ("
            "id INTEGER PRIMARY KEY, "
            "region VARCHAR, "
            "sale_date DATE, "
            "amount DECIMAL(10,2))",
        )
        _execute(
            conn,
            "INSERT INTO sales VALUES "
            "(1, 'US', DATE '2026-01-01', 100.00), "
            "(2, 'EU', DATE '2026-01-01', 200.00)",
        )

        # Materialization target lives in non-default schema
        _execute(conn, "CREATE SCHEMA agg")
        _execute(
            conn,
            "CREATE TABLE agg.daily_revenue ("
            "region VARCHAR, total DECIMAL(18,2))",
        )
        # Seed sentinel values that could NEVER arise from raw expansion
        # over `sales` (where SUM(amount) GROUP BY region would yield
        # 100.00 / 200.00). Asserting the sentinel proves the query was
        # routed to agg.daily_revenue, not silently fell back to raw
        # expansion.
        _execute(
            conn,
            "INSERT INTO agg.daily_revenue VALUES ('US', -1.00), ('EU', -2.00)",
        )

        _execute(
            conn,
            """
            CREATE SEMANTIC VIEW rev_view AS
              TABLES (s AS sales PRIMARY KEY (id))
              DIMENSIONS (s.region AS s.region)
              METRICS (s.total AS SUM(s.amount))
              MATERIALIZATIONS (
                m AS (
                  TABLE agg.daily_revenue,
                  DIMENSIONS (region),
                  METRICS (total)
                )
              )
""",
        )
        conn.commit()

        # Query matching the materialization's exact dim/metric set routes
        # to agg.daily_revenue. Pre-migration the materialization emits
        # unqualified "FROM \"daily_revenue\"" which fails to resolve.
        # Post-migration, the sentinel value -1.00 proves routing actually
        # occurred (raw expansion would return 100.00 from SUM(s.amount)).
        with conn.cursor() as cur:
            cur.execute(
                "SELECT total FROM semantic_view('rev_view', "
                "dimensions := ['region'], metrics := ['total']) "
                "WHERE region = 'US'"
            )
            row = cur.fetchone()
        assert row is not None, "expected one row for region='US', got none"
        val = row[0]
        # Compare via float() to remain Decimal-agnostic; sentinel is
        # negative and could never arise from raw expansion of sales.
        assert float(val) == -1.0, (
            f"expected sentinel -1.00 from materialization routing, got {val} "
            "(likely raw expansion of sales)"
        )
    finally:
        conn.close()


def test_attach_facts_path(extension_path: Path, ext_dir: str, tmp_path: Path) -> None:
    """Scenario 7 — multi-DB ATTACH + FACTS metric on attached DB table. ACTIVE.

    Regression guard for the cross-catalog interaction with the FACTS path
    (``sql_gen.rs:181, 224, 244``). The other DB is pre-created via a side
    ``duckdb.connect()`` so the ADBC session only ATTACHes; the view's SOURCE
    TABLE lives in the attached DB (``db2.main.sales``), so the FACTS expansion
    has to emit a fully-qualified reference for the per-call
    ``Connection(*context.db)`` to resolve it. That reference is what this
    scenario exercises.

    The view itself is created in the session's own database. It was previously
    written ``CREATE SEMANTIC VIEW db2.main.attached_view``, and this docstring
    claimed the view was created "inside the attached DB" — it never was. The
    qualifier was parsed and discarded, so the view landed in ``scenario7.main``
    exactly as an unqualified name would have. Since semantic views became
    schema-scoped the qualifier is honoured, and a ``<database>.`` prefix naming
    another database is rejected rather than silently retargeted — semantic
    views are single-catalog (TECH-DEBT #26). Writing the name unqualified says
    what actually happens and keeps the cross-catalog coverage, which was always
    in the source-table reference rather than the view name.
    """
    db_path = str(tmp_path / "scenario7.duckdb")
    other_db_path = str(tmp_path / "other.duckdb")

    # Pre-create the attached DB outside the ADBC session
    import duckdb

    side = duckdb.connect(other_db_path)
    try:
        side.execute(
            "CREATE TABLE sales (id INTEGER PRIMARY KEY, "
            "region VARCHAR, amount DECIMAL(10,2))"
        )
        side.execute(
            "INSERT INTO sales VALUES (1, 'US', 100.00), (2, 'EU', 200.00)"
        )
    finally:
        side.close()

    conn = _connect_adbc(db_path, ext_dir)
    try:
        _bootstrap_extension(conn, extension_path)

        # A2 (Phase 68): SQL-string escape parity with line 100.
        # WR-04 (Phase 68 review): tempfile.TemporaryDirectory() paths on
        # macOS/Linux never contain single quotes, newlines, or NUL bytes,
        # so the .replace("'", "''") escape is sufficient for this
        # fixture's input domain. The "parity with line 100" comment
        # describes textual symmetry with _bootstrap_extension, NOT a
        # general SQL-escape contract. Do not copy this pattern to a
        # production code path that handles user-supplied paths — newline
        # / backslash / NUL injection would produce a syntactically valid
        # but semantically wrong ATTACH statement.
        other_db_path_sql = str(other_db_path).replace("'", "''")
        _execute(conn, f"ATTACH '{other_db_path_sql}' AS db2")
        _execute(
            conn,
            """
            CREATE SEMANTIC VIEW attached_view AS
              TABLES (s AS db2.main.sales PRIMARY KEY (id))
              FACTS (s.net_amount AS s.amount * 1.0)
              DIMENSIONS (s.region AS s.region)
""",
        )
        conn.commit()

        rows = _scalar(
            conn,
            "SELECT COUNT(*) FROM semantic_view('attached_view', "
            "facts := ['region', 'net_amount'])",
        )
        assert rows == 2, f"expected 2 rows, got {rows}"
    finally:
        conn.close()


# --------------------------------------------------------------------------
# Test runner
# --------------------------------------------------------------------------


_SCENARIOS = [
    test_main_path_default_schema,
    test_main_path_non_default_schema,
    test_facts_non_default_schema,
    test_semi_additive_non_default_schema,
    test_window_non_default_schema,
    test_materialization_routing_non_default_schema_target,
    test_attach_facts_path,
]


def run_tests() -> int:
    extension_path = get_extension_path()
    if not extension_path.exists():
        print(f"ERROR: extension not found at {extension_path}")
        print("Run `just build` first.")
        return 1

    ext_dir = get_ext_dir()
    passed = 0
    failed = 0

    for fn in _SCENARIOS:
        name = fn.__name__
        print(f"RUN:  {name}")
        with tempfile.TemporaryDirectory(prefix="sv_adbc_q_") as tmp:
            try:
                fn(extension_path, ext_dir, Path(tmp))
                print("  PASS")
                passed += 1
            except Exception as e:  # noqa: BLE001 (intentional broad catch in test runner)
                print(f"  FAIL: {type(e).__name__}: {e}")
                traceback.print_exc()
                failed += 1

    print()
    print(f"Results: {passed} passed, {failed} failed")
    return 0 if failed == 0 else 1


def main() -> None:
    sys.exit(run_tests())


if __name__ == "__main__":
    main()
