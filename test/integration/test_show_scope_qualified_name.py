#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
`SHOW SEMANTIC VIEWS IN SCHEMA <db_name>.<schema_name>` — Snowflake's qualified
scope form (TECH-DEBT #25 follow-up).

Snowflake's SHOW scope clause is
`IN { <name> | ACCOUNT | DATABASE [<db>] | SCHEMA [<db>.<schema>] }`, so the
two-part spelling is the documented way to name a schema in another database.

It parsed here and then rejoined the parts with `.` before comparing against
`schema_name`, which stores a BARE schema name — so the filter became
`schema_name = 'memory.main'` and matched nothing, with no error. Same silent
no-match shape as #25's quoting half; the case-fold fix could not help, because
folding does not make two different strings equal.

What this file pins that the unit tests cannot:

  1. the qualified form actually returns rows end-to-end;
  2. the DATABASE half is really applied — a same-named schema in a DIFFERENT
     database must not match. This is the case that separates the real fix from
     "match on the last part", which would look correct in every single-database
     test and silently return the wrong rows here. It needs a second ATTACHed
     database, which is why it lives outside sqllogictest;
  3. a schema literally NAMED `a.b` (quoted) is one part, not a qualifier — the
     thing a `split('.')` fix breaks.

Row-content assertions cannot go in a .test file because `list_semantic_views`
leads with a live `created_on`. The emitted SQL text is pinned independently by
`parse::rewrite::tests::show_scope_qualified_name_tests`.

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


def view_names(conn, sql):
    """Names from a SHOW SEMANTIC VIEWS statement, sorted.

    Column 1 is `name`; column 0 is the live `created_on` — the reason this
    assertion cannot be written into a .test file.
    """
    return sorted(row[1] for row in conn.execute(sql).fetchall())


def make_view(conn, name, schema):
    """Create a semantic view recorded as living in `schema`.

    The `USE` is load-bearing and not incidental setup: `schema_name` is stamped
    from `current_schema()`, so a qualified CREATE name does NOT determine it —
    `CREATE SEMANTIC VIEW analytics.v` while USE-d into `main` records `main`.
    (That asymmetry is TECH-DEBT #25's open stamping item; this file works with
    the behaviour as it is rather than asserting on it.)
    """
    conn.execute(f"CREATE TABLE IF NOT EXISTS {schema}.orders (id INTEGER, amt INTEGER)")
    conn.execute(f"USE {schema}")
    conn.execute(
        f"CREATE SEMANTIC VIEW {name} AS "
        f"TABLES (o AS {schema}.orders PRIMARY KEY (id)) "
        f"METRICS (o.total AS sum(o.amt))"
    )


def main():
    tmp = tempfile.mkdtemp()
    # Not `primary.db`: a database is named after its file, and `primary` is a
    # reserved word, which DuckDB (and so SHOW's scope name) refuses unquoted.
    primary = str(Path(tmp) / "primary_db.db")
    conn = make_connection(primary)
    db = conn.execute("SELECT current_database()").fetchone()[0]

    conn.execute("CREATE SCHEMA analytics")
    make_view(conn, "v_primary", "analytics")

    print(f"Primary database is {db!r}; one view in schema 'analytics'.")

    # Guard the fixture: if the stamp is not what the assertions below assume,
    # every "expected []" case would pass vacuously.
    stamped = conn.execute(
        "SELECT database_name, schema_name FROM list_semantic_views() "
        "WHERE name = 'v_primary'"
    ).fetchall()
    check("fixture — v_primary is stamped in the right db/schema", stamped, [(db, "analytics")])

    print("\n1. The qualified form returns rows (was silently empty):")
    check(
        f"IN SCHEMA {db}.analytics",
        view_names(conn, f"SHOW SEMANTIC VIEWS IN SCHEMA {db}.analytics"),
        ["v_primary"],
    )
    check(
        f"IN SCHEMA {db.upper()}.ANALYTICS (folds too)",
        view_names(conn, f"SHOW SEMANTIC VIEWS IN SCHEMA {db.upper()}.ANALYTICS"),
        ["v_primary"],
    )

    print("\n2. The DATABASE half is really applied — a same-named schema in")
    print("   another database must NOT match (defeats a last-part-only fix):")
    check(
        "IN SCHEMA nosuchdb.analytics",
        view_names(conn, "SHOW SEMANTIC VIEWS IN SCHEMA nosuchdb.analytics"),
        [],
    )
    # ...and with a database that genuinely exists and genuinely holds a schema
    # of the same name, so the empty result above cannot be dismissed as "the
    # database name simply does not resolve".
    other = str(Path(tmp) / "other.db")
    conn.execute(f"ATTACH '{other}' AS otherdb")
    conn.execute("CREATE SCHEMA otherdb.analytics")
    check(
        "IN SCHEMA otherdb.analytics (real db, real same-named schema, no views)",
        view_names(conn, "SHOW SEMANTIC VIEWS IN SCHEMA otherdb.analytics"),
        [],
    )
    # The unqualified spelling still finds it — proving the row is reachable and
    # the empty results above are the database predicate doing its job.
    check(
        "IN SCHEMA analytics (unqualified, still finds it)",
        view_names(conn, "SHOW SEMANTIC VIEWS IN SCHEMA analytics"),
        ["v_primary"],
    )

    print("\n3. A quoted dot is a NAME, not a qualifier:")
    conn.execute('CREATE SCHEMA "a.b"')
    make_view(conn, "v_dotted", '"a.b"')
    check(
        'IN SCHEMA "a.b"',
        view_names(conn, 'SHOW SEMANTIC VIEWS IN SCHEMA "a.b"'),
        ["v_dotted"],
    )
    # The mirror image, and a WRONG HIT before the fix rather than a miss:
    # rejoining made unquoted `a.b` match the schema literally named `a.b`, so
    # a qualifier was answered by a same-spelled name. Read correctly it means
    # database `a`, schema `b`, which exists nowhere here.
    check(
        "IN SCHEMA a.b (unquoted — a qualifier, matches nothing)",
        view_names(conn, "SHOW SEMANTIC VIEWS IN SCHEMA a.b"),
        [],
    )

    print("\n4. Over-long and mis-shaped names are errors, not silent empties:")
    for sql, label in [
        ("SHOW SEMANTIC VIEWS IN SCHEMA a.b.c", "three-part schema"),
        ("SHOW SEMANTIC VIEWS IN DATABASE a.b", "qualified database"),
    ]:
        try:
            conn.execute(sql).fetchall()
            print(f"  FAIL  {label}: expected an error, got a result set")
            failures.append(label)
        except Exception as e:
            first = str(e).splitlines()[0]
            if "Invalid" in first:
                print(f"  PASS  {label}: {first[:70]}")
            else:
                print(f"  FAIL  {label}: wrong error: {first[:70]}")
                failures.append(label)

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
