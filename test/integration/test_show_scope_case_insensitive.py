#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
`SHOW SEMANTIC VIEWS IN SCHEMA / IN DATABASE` must match case-insensitively
(TECH-DEBT #25 — the case-folding question left open by the quoting fix).

DuckDB resolves identifiers case-insensitively, and this project adopted that
rule uniformly (`ident::ident_matches`). These two filter slots were the last
sites comparing with a raw `=` against a stored text column instead.

Why that is worse than "you must spell it the way the catalog did": the stored
`schema_name` is stamped from `current_schema()` at CREATE time, and DuckDB
returns that as *the spelling the caller typed in their last `USE`*, not the
catalog's. Verified against DuckDB 1.5.5:

    CREATE SCHEMA MySchema      -> catalog stores 'MySchema' (case preserved)
    USE MySchema  -> current_schema() = 'MySchema'
    USE myschema  -> current_schema() = 'myschema'   <- same schema
    USE MYSCHEMA  -> current_schema() = 'MYSCHEMA'

So two semantic views created in ONE schema could carry DIFFERENT stamps, and
before the fix NO spelling of the filter returned both — including the
catalog's own. That is what this file was written to pin.

The divergence itself is gone: schema scoping (TECH-DEBT #25) made the stamp
canonical, resolving the target schema through `duckdb_schemas()` rather than
storing `current_schema()`'s echo, because `(schema_name, name)` became a
primary key and one schema has to map to one stored string. The fold this file
covers is still load-bearing, though — the stamp is the CATALOG's spelling, so
a caller writing `IN SCHEMA MYSCHEMA` still needs both sides folded to match a
stored `MySchema`. The precondition below asserts both halves: the stamps agree
now, and they differ in case from the spellings queried.

Why here and not in sqllogictest: the assertion is on returned ROWS, and
`list_semantic_views` leads with a live `created_on` timestamp that cannot be
written into a `.test` file. The same reason `test_readonly_load.py` carries
the read-only coverage. The emitted SQL text is pinned independently by
`parse::rewrite::tests::show_scope_case_folding_tests`.

Exit codes:
    0 = all tests passed
    1 = at least one test failed
"""

import sys
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


def make_connection():
    import duckdb

    conn = duckdb.connect(
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": EXT_DIR,
        },
    )
    conn.execute(f"FORCE INSTALL '{EXT_PATH}'")
    conn.execute("LOAD semantic_views")
    return conn


def view_names(conn, sql):
    """Names returned by a SHOW SEMANTIC VIEWS statement, sorted.

    Column 1 is `name`; column 0 is the live `created_on`, which is precisely
    why this assertion cannot live in a .test file.
    """
    return sorted(row[1] for row in conn.execute(sql).fetchall())


def main():
    conn = make_connection()

    conn.execute("CREATE SCHEMA MySchema")
    conn.execute("CREATE TABLE MySchema.orders (id INTEGER, amt DECIMAL(10,2))")

    # Two views in ONE schema, created under different spellings of it in the
    # preceding USE. Their stamps used to differ as a result (`MySchema` and
    # `myschema`), which is what made a fold-free filter unanswerable: no single
    # spelling returned both, not even the catalog's own. Schema scoping made
    # the stamp canonical, so both now record the catalog's spelling — the
    # stronger invariant asserted below.
    conn.execute("USE MySchema")
    conn.execute(
        "CREATE SEMANTIC VIEW v_upper AS "
        "TABLES (o AS MySchema.orders PRIMARY KEY (id)) "
        "METRICS (o.total AS sum(o.amt))"
    )
    conn.execute("USE myschema")
    conn.execute(
        "CREATE SEMANTIC VIEW v_lower AS "
        "TABLES (o AS MySchema.orders PRIMARY KEY (id)) "
        "METRICS (o.total AS sum(o.amt))"
    )

    stamps = sorted(
        row[0]
        for row in conn.execute(
            "SELECT schema_name FROM list_semantic_views() "
            "WHERE name IN ('v_upper', 'v_lower')"
        ).fetchall()
    )
    print("Precondition — both views carry the catalog's own spelling:")
    check("canonical schema_name stamps", stamps, ["MySchema", "MySchema"])

    # ...and the fold below still has real work to do. The stamp is canonical
    # now, but it is the CATALOG's spelling, not the caller's — so every
    # spelling this file queries with still has to be folded to match it. Assert
    # that explicitly: without it, a future change that stamped the *caller's*
    # spelling would leave every case below passing for the wrong reason.
    print("...and it differs in case from the spellings queried below:")
    check(
        "stored stamp is not what the caller writes",
        [s for s in {"MYSCHEMA", "myschema", "mYsChEmA"} if s in stamps],
        [],
    )

    both = ["v_lower", "v_upper"]

    print("\nIN SCHEMA — every spelling must return BOTH views:")
    for spelling in ["MySchema", "myschema", "MYSCHEMA", "mYsChEmA"]:
        check(
            f"IN SCHEMA {spelling}",
            view_names(conn, f"SHOW SEMANTIC VIEWS IN SCHEMA {spelling}"),
            both,
        )

    print("\n...and quoting must not change the answer (#25's invariant):")
    for spelling in ['"MySchema"', '"myschema"', '"MYSCHEMA"']:
        check(
            f"IN SCHEMA {spelling}",
            view_names(conn, f"SHOW SEMANTIC VIEWS IN SCHEMA {spelling}"),
            both,
        )

    print("\nTERSE takes the same suffix, so it must fold too:")
    check(
        "SHOW TERSE ... IN SCHEMA MYSCHEMA",
        view_names(conn, "SHOW TERSE SEMANTIC VIEWS IN SCHEMA MYSCHEMA"),
        both,
    )

    print("\nIN DATABASE — same defect, same fix:")
    for spelling in ["memory", "MEMORY", "Memory", '"MEMORY"']:
        got = view_names(conn, f"SHOW SEMANTIC VIEWS IN DATABASE {spelling}")
        check(f"IN DATABASE {spelling}", got, both)

    print("\nComposition with the surrounding clauses is unaffected:")
    check(
        "LIKE + IN SCHEMA + LIMIT",
        view_names(
            conn,
            "SHOW SEMANTIC VIEWS LIKE 'v_%' IN SCHEMA MYSCHEMA LIMIT 5",
        ),
        both,
    )

    # Over-matching controls. Folding must not turn the filter into a pass-through.
    print("\nControls — folding must not widen the filter to everything:")
    check(
        "IN SCHEMA main (a real but different schema)",
        view_names(conn, "SHOW SEMANTIC VIEWS IN SCHEMA main"),
        [],
    )
    check(
        "IN SCHEMA nosuchschema",
        view_names(conn, "SHOW SEMANTIC VIEWS IN SCHEMA nosuchschema"),
        [],
    )
    check(
        "IN DATABASE nosuchdb",
        view_names(conn, "SHOW SEMANTIC VIEWS IN DATABASE nosuchdb"),
        [],
    )
    # A prefix of a matching name must not match: `lower(x) = lower(y)` is an
    # equality, not a LIKE. Guards against a future switch to ILIKE, where a
    # name containing `%` or `_` would become a wildcard.
    check(
        "IN SCHEMA mysch (a prefix)",
        view_names(conn, "SHOW SEMANTIC VIEWS IN SCHEMA mysch"),
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
