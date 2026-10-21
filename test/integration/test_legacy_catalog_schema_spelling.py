#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
CAT-1 (code-review 2026-08-03): the schema-scoping migration must record the
CATALOG's spelling of a schema, not the spelling the legacy row happens to
carry.

`current_schema()` echoes whatever spelling was last written in `USE`, so a view
created after `USE "ANALYTICS"` recorded `ANALYTICS` in its definition JSON for
a schema the catalog itself calls `analytics`. Migrated verbatim, the row keys
under a spelling the catalog never had. Because `INSERT OR REPLACE` conflicts on
the byte-equal `(schema_name, name)` primary key while every read guard folds
case, a later `CREATE OR REPLACE analytics.v` then inserts a SECOND row instead
of replacing the first — and reads, which match both, resolve to whichever sorts
first. `ORDER BY schema_name` puts the uppercase one first, so the CREATE
reports success while every subsequent read returns the PRE-REPLACE definition.

Unit tests cover the migration in isolation against a hand-built table. This
exercises the same path the way a user meets it: a real pre-scoping database
file, opened through a real `LOAD`, then written to with ordinary DDL.

  T1: a legacy catalog whose row records a non-canonical schema spelling
      migrates to the catalog's own spelling, with the column and the JSON in
      lockstep (the SHOW / DESCRIBE listings read the JSON).
  T2: the user-visible consequence — after migration, `CREATE OR REPLACE`
      REPLACES that view rather than duplicating it, and reads return the new
      definition rather than the stale one.
  T3: a recorded schema the catalog does not know keeps its spelling; the row
      is carried across rather than dropped or failing the migration.

Exit codes: 0 = all passed, 1 = at least one failed.
"""

import json
import shutil
import sys
import tempfile
import traceback
from pathlib import Path

import duckdb

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()


# Loading a locally-built (unsigned) extension needs both of these, as in the
# sibling integration tests.
_EXT_CONFIG = {"allow_unsigned_extensions": "true", "extension_directory": EXT_DIR}


def _install_and_load(conn) -> None:
    conn.execute(f"FORCE INSTALL '{EXT_PATH}'")
    conn.execute("LOAD semantic_views")


def run_test(name, test_fn) -> bool:
    print(f"\n{'=' * 60}\nTEST: {name}\n{'=' * 60}")
    try:
        test_fn()
        print("  RESULT: PASS")
        return True
    except AssertionError as e:
        print(f"  RESULT: FAIL\n  {e}")
        return False
    except Exception as e:  # noqa: BLE001
        traceback.print_exc()
        print(f"  RESULT: ERROR\n  {type(e).__name__}: {e}")
        return False


def _definition_json(schema_spelling: str) -> str:
    """A minimal current-format definition recording the given schema spelling."""
    return json.dumps(
        {
            "name": "v",
            "schema_name": schema_spelling,
            "tables": [{"alias": "o", "table": "orders", "pk_columns": ["id"]}],
            "dimensions": [{"name": "region", "expr": "o.region", "source_table": "o"}],
            "metrics": [{"name": "cnt", "expr": "count(*)", "source_table": "o"}],
        }
    )


def _make_legacy_db(path: str, *, schema_spelling: str, create_schema: bool) -> None:
    """Write a database in the PRE-scoping catalog shape.

    Built WITHOUT the extension loaded, so the table really is the old shape:
    `_definitions` keyed on `name` alone, with no `schema_name` column. That is
    what `definitions_is_legacy_shape` detects and migrates on the next LOAD.
    """
    conn = duckdb.connect(path)
    try:
        conn.execute("CREATE TABLE orders (id INTEGER, region VARCHAR)")
        conn.execute("INSERT INTO orders VALUES (1, 'EU'), (2, 'US')")
        if create_schema:
            # The catalog's own spelling is lowercase; the stored row below
            # records a different one.
            conn.execute("CREATE SCHEMA analytics")
        conn.execute("CREATE SCHEMA semantic_layer")
        conn.execute(
            "CREATE TABLE semantic_layer._definitions ("
            "  name       VARCHAR PRIMARY KEY,"
            "  definition VARCHAR NOT NULL"
            ")"
        )
        conn.execute(
            "INSERT INTO semantic_layer._definitions (name, definition) VALUES (?, ?)",
            ["v", _definition_json(schema_spelling)],
        )
    finally:
        conn.close()


def _rows(conn):
    return conn.execute(
        "SELECT schema_name, name, definition FROM semantic_layer._definitions "
        "ORDER BY schema_name, name"
    ).fetchall()


def test_migration_canonicalises_the_schema_spelling() -> None:
    d = tempfile.mkdtemp()
    try:
        db = str(Path(d) / "legacy.db")
        _make_legacy_db(db, schema_spelling="ANALYTICS", create_schema=True)

        conn = duckdb.connect(db, config=_EXT_CONFIG)
        try:
            _install_and_load(conn)  # migration runs here
            rows = _rows(conn)
            assert len(rows) == 1, f"expected one migrated row, got {rows}"
            schema, name, definition = rows[0]
            assert schema == "analytics", (
                f"the stored key must be the catalog's spelling, got {schema!r}"
            )
            assert name == "v"
            in_json = json.loads(definition).get("schema_name")
            assert in_json == schema, (
                "the column and the schema inside the JSON must stay in lockstep "
                f"(column={schema!r}, json={in_json!r}) -- SHOW/DESCRIBE read the JSON"
            )
        finally:
            conn.close()
    finally:
        shutil.rmtree(d, ignore_errors=True)


def test_create_or_replace_replaces_rather_than_duplicating() -> None:
    """The user-visible consequence of CAT-1, end to end."""
    d = tempfile.mkdtemp()
    try:
        db = str(Path(d) / "legacy.db")
        _make_legacy_db(db, schema_spelling="ANALYTICS", create_schema=True)

        conn = duckdb.connect(db, config=_EXT_CONFIG)
        try:
            _install_and_load(conn)
            conn.execute(
                "CREATE OR REPLACE SEMANTIC VIEW analytics.v AS "
                "  TABLES (o AS orders PRIMARY KEY (id)) "
                "  DIMENSIONS (o.region AS o.region) "
                "  METRICS (o.cnt2 AS count(*))"
            )

            rows = _rows(conn)
            assert len(rows) == 1, (
                "CREATE OR REPLACE must replace the migrated row, not add a "
                f"second one under a different spelling of the same schema: {rows}"
            )

            # And the surviving row must be the NEW definition, not the stale
            # one a duplicate-row catalog would have kept serving.
            # SHOW SEMANTIC METRICS: 8 Snowflake-aligned columns, `name` at
            # index 4 (database, schema, view, table, name, type, synonyms,
            # comment).
            metrics = [
                r[4]
                for r in conn.execute("SHOW SEMANTIC METRICS IN analytics.v").fetchall()
            ]
            assert metrics == ["cnt2"], (
                f"reads must return the replaced definition, got metrics {metrics}"
            )
        finally:
            conn.close()
    finally:
        shutil.rmtree(d, ignore_errors=True)


def test_unknown_schema_spelling_is_carried_across() -> None:
    """A schema the catalog does not know cannot be canonicalised.

    The row must still migrate — losing a definition here is unrecoverable, and
    such a row is no worse off than it was before the migration.
    """
    d = tempfile.mkdtemp()
    try:
        db = str(Path(d) / "legacy.db")
        _make_legacy_db(db, schema_spelling="GhostSchema", create_schema=False)

        conn = duckdb.connect(db, config=_EXT_CONFIG)
        try:
            _install_and_load(conn)
            rows = _rows(conn)
            assert len(rows) == 1, f"the row must be carried across, got {rows}"
            assert rows[0][0] == "GhostSchema", (
                f"an unresolvable schema keeps its recorded spelling, got {rows[0][0]!r}"
            )
        finally:
            conn.close()
    finally:
        shutil.rmtree(d, ignore_errors=True)


if __name__ == "__main__":
    results = [
        run_test(
            "test_migration_canonicalises_the_schema_spelling",
            test_migration_canonicalises_the_schema_spelling,
        ),
        run_test(
            "test_create_or_replace_replaces_rather_than_duplicating",
            test_create_or_replace_replaces_rather_than_duplicating,
        ),
        run_test(
            "test_unknown_schema_spelling_is_carried_across",
            test_unknown_schema_spelling_is_carried_across,
        ),
    ]
    passed = sum(results)
    total = len(results)
    print(f"\n{'=' * 60}\nSUMMARY: {passed}/{total} tests passed\n{'=' * 60}")
    sys.exit(0 if passed == total else 1)
