#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
`GET_DDL('SEMANTIC_VIEW', <name>, <use_fully_qualified_names>)` — the dump and
restore round trip (TECH-DEBT #25).

Semantic views are scoped to a schema, but `GET_DDL` rendered a bare `CREATE`
name. Re-running that output put the view in whatever schema the executing
session happened to be in, so a dump/restore silently relocated every view that
did not live in the restoring session's schema. Snowflake's signature is
`GET_DDL(<type>, <name>[, <use_fully_qualified_names>])` and its default is
unqualified, so the default was right and the gap was the missing argument.

What this file pins that neither the unit tests nor the .test file can:

  1. the ACTUAL round trip — fetch the DDL string, drop the view, then execute
     that string from a *different* current schema and check where the view
     lands. sqllogictest cannot express it, because the DDL is produced by one
     statement and must become the text of a later one;
  2. the negative control on the same fixture: the two-argument form relocates
     the view. Without it "qualified restores correctly" proves nothing —
     both spellings would look identical if the restoring session already sat
     in the right schema, which is why the replay happens from `staging`;
  3. that the restored view still ANSWERS — a header that parses but loses the
     body would satisfy a name check and return nothing.

The emitted text itself is pinned independently by
`render_ddl::tests::qualified_name_tests` (per-case red) and
`test/sql/get_ddl_qualified_name.test`.

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

failures = []


def check(label, got, expected):
    if got == expected:
        print(f"  PASS  {label}: {got}")
    else:
        print(f"  FAIL  {label}: got {got!r}, expected {expected!r}")
        failures.append(label)


def make_connection(db_path):
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


def seed(conn):
    """Two schemas; the view lives in `analytics`, restores run from `staging`."""
    conn.execute("CREATE SCHEMA analytics")
    conn.execute("CREATE SCHEMA staging")
    conn.execute(
        "CREATE TABLE main.orders (id INTEGER PRIMARY KEY, amount INTEGER, "
        "region VARCHAR)"
    )
    conn.execute(
        "INSERT INTO main.orders VALUES (1, 100, 'north'), (2, 250, 'south'), "
        "(3, 50, 'north')"
    )
    conn.execute(
        """CREATE SEMANTIC VIEW analytics.sales AS
             TABLES (o AS main.orders PRIMARY KEY (id))
             DIMENSIONS (o.region AS o.region)
             METRICS (o.revenue AS SUM(o.amount))"""
    )


def schema_of(conn, name):
    """Which schemas hold a semantic view of this name, sorted."""
    rows = conn.execute(
        "SELECT schema_name FROM list_semantic_views() WHERE name = ? "
        "ORDER BY schema_name",
        [name],
    ).fetchall()
    return [r[0] for r in rows]


def replay_from_staging(conn, ddl):
    """Drop the view, then execute `ddl` with the session sitting in staging."""
    conn.execute("DROP SEMANTIC VIEW analytics.sales")
    conn.execute("USE staging")
    try:
        conn.execute(ddl)
    finally:
        conn.execute("USE main")


def main():
    with tempfile.TemporaryDirectory() as tmp:
        db_path = str(Path(tmp) / "get_ddl_qualified.db")

        print("1. The two-argument form relocates the view (the bug):")
        conn = make_connection(db_path)
        seed(conn)
        ddl = conn.execute(
            "SELECT GET_DDL('SEMANTIC_VIEW', 'analytics.sales')"
        ).fetchone()[0]
        check(
            "unqualified DDL header",
            ddl.splitlines()[0],
            "CREATE OR REPLACE SEMANTIC VIEW sales AS",
        )
        replay_from_staging(conn, ddl)
        # This is the defect, asserted as it actually behaves: restoring an
        # unqualified dump from `staging` files the view under `staging`.
        # It is Snowflake's documented default, so it is not a bug to fix —
        # it is the reason the third argument has to exist.
        check("restored (unqualified) lands in", schema_of(conn, "sales"), ["staging"])
        conn.close()

    with tempfile.TemporaryDirectory() as tmp:
        db_path = str(Path(tmp) / "get_ddl_qualified2.db")

        print("\n2. The third argument round-trips the view to its own schema:")
        conn = make_connection(db_path)
        seed(conn)
        ddl = conn.execute(
            "SELECT GET_DDL('SEMANTIC_VIEW', 'analytics.sales', true)"
        ).fetchone()[0]
        check(
            "qualified DDL header",
            ddl.splitlines()[0],
            "CREATE OR REPLACE SEMANTIC VIEW analytics.sales AS",
        )
        replay_from_staging(conn, ddl)
        check("restored (qualified) lands in", schema_of(conn, "sales"), ["analytics"])

        print("\n3. The restored view still answers:")
        rows = conn.execute(
            "SELECT region, revenue FROM semantic_view('analytics.sales', "
            "dimensions := ['region'], metrics := ['revenue']) ORDER BY region"
        ).fetchall()
        check("restored view rows", rows, [("north", 150), ("south", 250)])
        conn.close()

    with tempfile.TemporaryDirectory() as tmp:
        db_path = str(Path(tmp) / "get_ddl_qualified3.db")

        print("\n4. A quoted schema and name round-trip as two parts:")
        conn = make_connection(db_path)
        conn.execute('CREATE SCHEMA "my schema"')
        conn.execute("CREATE SCHEMA staging")
        conn.execute("CREATE TABLE main.orders (id INTEGER PRIMARY KEY, r VARCHAR)")
        conn.execute(
            """CREATE SEMANTIC VIEW "my schema"."my view" AS
                 TABLES (o AS main.orders PRIMARY KEY (id))
                 DIMENSIONS (o.r AS o.r)
                 METRICS (o.n AS COUNT(o.id))"""
        )
        ddl = conn.execute(
            "SELECT GET_DDL('SEMANTIC_VIEW', '\"my schema\".\"my view\"', true)"
        ).fetchone()[0]
        check(
            "quoted qualified header",
            ddl.splitlines()[0],
            'CREATE OR REPLACE SEMANTIC VIEW "my schema"."my view" AS',
        )
        # The wrong fix — quoting the joined string — emits
        # `"my schema.my view"`, which parses as ONE name containing a dot and
        # restores into a view called exactly that, in `staging`.
        conn.execute('DROP SEMANTIC VIEW "my schema"."my view"')
        conn.execute("USE staging")
        try:
            conn.execute(ddl)
        finally:
            conn.execute("USE main")
        check("restored quoted view lands in", schema_of(conn, "my view"), ["my schema"])
        check(
            "no view named with a literal dot",
            schema_of(conn, "my schema.my view"),
            [],
        )
        conn.close()


if __name__ == "__main__":
    try:
        main()
    except Exception:
        traceback.print_exc()
        sys.exit(1)
    if failures:
        print(f"\nFAILED ({len(failures)}): {', '.join(failures)}")
        sys.exit(1)
    print("\nAll checks passed.")
    sys.exit(0)
