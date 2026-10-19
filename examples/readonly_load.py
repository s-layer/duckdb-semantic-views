#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/readonly_load.py

Demonstrates v0.9.0 features:
  - LOAD semantic_views works on a read-only database.
  - Previously-defined semantic views can be queried via SHOW SEMANTIC VIEWS,
    DESCRIBE SEMANTIC VIEW, and the semantic_view table function.
  - CREATE / DROP / ALTER SEMANTIC VIEW fail with DuckDB's standard
    read-only error rather than a confusing schema-create error at LOAD.

Two scenarios:
  1. Bootstrap a view in a subprocess (writable), then reopen the same
     file with read_only=True from the parent process and query it.
  2. Open a fresh (never-bootstrapped) database read-only and confirm
     SHOW SEMANTIC VIEWS returns an empty list (no error) and
     DESCRIBE SEMANTIC VIEW missing surfaces the standard
     "does not exist" error.

Both scenarios end by attempting a CREATE / DROP on the read-only
connection to demonstrate the read-only error message verbatim.

Why bootstrap in a subprocess? Phase 62's OverrideContext is attached
per-DBConfig and keeps the catalog connection alive until process exit.
Once the extension has been LOADed against a writable handle, the same
DB cannot be reopened read-only in the SAME process (the open hangs
because the Database is still referenced). Running bootstrap in a
subprocess sidesteps this by letting the OS reclaim the DBConfig at
child exit. This mirrors real deployments: bootstrap in a build/CI job,
then ship the read-only DB to a separate analytics worker process.
"""

import os
import subprocess
import sys
import tempfile

import duckdb

EXTENSION_PATH = os.environ.get(
    "SEMANTIC_VIEWS_EXTENSION_PATH",
    "build/debug/semantic_views.duckdb_extension",
)


CREATE_VIEW_SQL = (
    "CREATE SEMANTIC VIEW orders_view AS "
    "TABLES (o AS orders PRIMARY KEY (id)) "
    "DIMENSIONS (o.region AS o.region) "
    "METRICS (o.total AS SUM(o.amount))"
)


def list_views(con) -> list[str]:
    cur = con.execute("SHOW SEMANTIC VIEWS")
    name_idx = [d[0] for d in cur.description].index("name")
    return [r[name_idx] for r in cur.fetchall()]


def bootstrap_in_subprocess(db_path: str) -> None:
    """Open writable, install + load the extension, define a view, close.

    Run in a subprocess so the in-process DBConfig (which holds the
    extension's catalog connection) is reclaimed at exit and we can
    reopen the same file read-only from the parent.
    """
    script = f"""
import duckdb
con = duckdb.connect({db_path!r}, config={{"allow_unsigned_extensions": "true"}})
con.execute("LOAD '{EXTENSION_PATH}'")
con.execute('''
    CREATE TABLE orders (
        id INTEGER PRIMARY KEY,
        region VARCHAR,
        amount DECIMAL(10,2)
    );
    INSERT INTO orders VALUES
        (1, 'US', 100.00),
        (2, 'EU', 200.00),
        (3, 'US', 150.00);
''')
con.execute({CREATE_VIEW_SQL!r})
print('Defined views (writable subprocess):',
      [r[1] for r in con.execute('SHOW SEMANTIC VIEWS').fetchall()])  # col 1 = name
con.close()
"""
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
    # Surface the subprocess's stdout so the user sees the bootstrap step.
    if result.stdout.strip():
        print(result.stdout, end="")


def bootstrapped_demo() -> None:
    print("=== Scenario 1: bootstrapped database, reopened read-only ===\n")
    with tempfile.TemporaryDirectory(prefix="sv_readonly_demo_") as tmp:
        db_path = os.path.join(tmp, "demo.duckdb")

        # Step 1 -- bootstrap in a subprocess (writable open + LOAD + CREATE).
        bootstrap_in_subprocess(db_path)

        # Step 2 -- reopen the SAME file read-only from the parent and LOAD.
        # Pre-v0.9.0 this LOAD raised "Cannot execute statement of type
        # CREATE_SCHEMA on database ... in read-only mode" because
        # init_catalog unconditionally issued CREATE SCHEMA IF NOT EXISTS.
        ro = duckdb.connect(
            db_path,
            read_only=True,
            config={"allow_unsigned_extensions": "true"},
        )
        ro.execute(f"LOAD '{EXTENSION_PATH}'")
        print("LOAD on read-only DB:    OK\n")

        # Step 3 -- query.
        print("SHOW SEMANTIC VIEWS:    ", list_views(ro))
        desc = ro.execute("DESCRIBE SEMANTIC VIEW orders_view").fetchall()
        print(f"describe rows:           {len(desc)} metadata rows")
        rows = ro.execute(
            "SELECT region, total FROM semantic_view("
            "  'orders_view', dimensions := ['region'], metrics := ['total']"
            ") ORDER BY region"
        ).fetchall()
        print("semantic_view rows:     ", rows)

        # Step 4 -- DDL fails with DuckDB's standard read-only error.
        print()
        try:
            ro.execute("DROP SEMANTIC VIEW orders_view")
            print("DROP unexpectedly succeeded -- this should not happen.")
        except duckdb.Error as e:
            print(f"DROP on read-only DB:    fails as expected\n  -> {e}")

        ro.close()
        print()


def fresh_readonly_demo() -> None:
    print("=== Scenario 2: fresh read-only database, no bootstrap ===\n")
    with tempfile.TemporaryDirectory(prefix="sv_fresh_ro_demo_") as tmp:
        db_path = os.path.join(tmp, "fresh.duckdb")

        # Bootstrap a valid DuckDB file header by opening writable
        # WITHOUT loading the extension, then closing. (No extension load
        # in this scenario — so no in-process DBConfig leak; we can stay
        # in-process here.)
        duckdb.connect(db_path).execute("SELECT 1").close()

        ro = duckdb.connect(
            db_path,
            read_only=True,
            config={"allow_unsigned_extensions": "true"},
        )
        ro.execute(f"LOAD '{EXTENSION_PATH}'")
        print("LOAD on fresh read-only DB:    OK")
        print("SHOW SEMANTIC VIEWS (no bootstrap):", list_views(ro))
        print("  -> empty list, NOT a catalog error\n")

        # DESCRIBE SEMANTIC VIEW on a missing view -> clean "does not exist".
        try:
            ro.execute("DESCRIBE SEMANTIC VIEW nonexistent").fetchall()
        except duckdb.Error as e:
            print(f"DESCRIBE SEMANTIC VIEW nonexistent -> {e}")

        # CREATE on a fresh read-only DB also fails (the rewrite emits an
        # INSERT against semantic_layer._definitions which doesn't exist).
        print()
        try:
            ro.execute(CREATE_VIEW_SQL.replace("orders_view", "v"))
            print("CREATE unexpectedly succeeded -- this should not happen.")
        except duckdb.Error as e:
            print(f"CREATE on fresh read-only DB:    fails as expected\n  -> {e}")

        ro.close()


if __name__ == "__main__":
    bootstrapped_demo()
    fresh_readonly_demo()
    print("\nAll scenarios completed.")
