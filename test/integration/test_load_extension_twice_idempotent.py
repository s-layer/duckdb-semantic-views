#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0", "pytest>=7.0"]
# requires-python = ">=3.10"
# ///
"""
`LOAD semantic_views` must be idempotent across repeated calls within a single
process, and must turn the extension's grammar on for every connection.

The grammar is registered once per database (`sv_register_grammar` in
`cpp/src/shim.cpp` checks `FindGrammarExtension` first), so a second LOAD adds
nothing. DuckDB does not surface a double registration as a user error, so this
file pins the behaviour: a second LOAD does not throw, and DDL still parses.

`active_grammar_extensions` is a per-connection setting that may only be
changed on the connection's own thread (TECH-DEBT #103), so LOAD turns the
grammar on in three ways:
  * the loading connection, when the LOAD ends (the sentinel DDL in the first
    test);
  * a connection opened later, when it opens;
  * a connection already open, when it next begins or ends a query. DuckDB
    parses a statement before its query begins, and a statement that fails to
    parse begins none, so that connection parses without the grammar until it
    runs a query (any query, LOAD semantic_views included).

Exit codes:
    0 = test passed (including pytest SKIP convention if run directly)
    1 = test failed
"""

import os
import sys
import tempfile
from pathlib import Path

import duckdb
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()


def test_load_extension_twice_idempotent():
    with tempfile.TemporaryDirectory() as tmp:
        db_path = os.path.join(tmp, "test.duckdb")
        conn = duckdb.connect(
            db_path,
            config={
                "allow_unsigned_extensions": "true",
                "extension_directory": EXT_DIR,
            },
        )
        try:
            conn.execute(f"FORCE INSTALL '{EXT_PATH}'")

            # First LOAD — bootstrap path.
            conn.execute("LOAD semantic_views")

            # Second LOAD — the idempotence contract. A double registration
            # would not throw either, so the assertion is "no exception" plus
            # the sentinel below.
            conn.execute("LOAD semantic_views")

            # Sentinel — a DDL statement through the grammar. If the second
            # LOAD had broken it (a duplicate production, say), this would
            # surface as a parse error here.
            conn.execute("CREATE TABLE t1 (id INTEGER)")
            conn.execute(
                "CREATE SEMANTIC VIEW v_after_double_load AS "
                "TABLES (t1 AS t1) "
                "METRICS (t1.cnt AS count(*))"
            )

            result = conn.execute(
                "SELECT cnt FROM semantic_view("
                "'v_after_double_load', "
                "dimensions := [], "
                "metrics := ['cnt'])"
            ).fetchall()
            assert result == [(0,)], (
                f"sentinel DDL after double-LOAD returned unexpected rows: {result!r}"
            )
        finally:
            conn.close()


def _connect(tmp):
    conn = duckdb.connect(
        os.path.join(tmp, "test.duckdb"),
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": EXT_DIR,
        },
    )
    conn.execute(f"FORCE INSTALL '{EXT_PATH}'")
    return conn


CREATE_VIEW = (
    "CREATE SEMANTIC VIEW {name} AS TABLES (t1 AS t1) METRICS (t1.cnt AS count(*))"
)


def test_connection_opened_after_load():
    with tempfile.TemporaryDirectory() as tmp:
        conn = _connect(tmp)
        try:
            conn.execute("LOAD semantic_views")
            conn.execute("CREATE TABLE t1 (id INTEGER)")
            later = conn.cursor()
            later.execute(CREATE_VIEW.format(name="v_later"))
            names = later.execute("SELECT name FROM (SHOW SEMANTIC VIEWS)").fetchall()
            assert names == [("v_later",)], names
        finally:
            conn.close()


def test_connection_open_before_load():
    with tempfile.TemporaryDirectory() as tmp:
        conn = _connect(tmp)
        try:
            earlier = conn.cursor()
            reloaded = conn.cursor()
            conn.execute("CREATE TABLE t1 (id INTEGER)")
            conn.execute("LOAD semantic_views")

            # A statement is parsed before its query begins, and one that fails
            # to parse begins none, so until this connection runs a query it
            # parses without the grammar.
            for _ in range(2):
                with pytest.raises(duckdb.ParserException, match="syntax error"):
                    earlier.execute("SHOW SEMANTIC VIEWS")
            earlier.execute("SELECT 1")
            earlier.execute(CREATE_VIEW.format(name="v_earlier"))
            names = earlier.execute("SELECT name FROM (SHOW SEMANTIC VIEWS)").fetchall()
            assert names == [("v_earlier",)], names

            # LOAD on the connection itself turns the grammar on at once.
            reloaded.execute("LOAD semantic_views")
            names = reloaded.execute("SELECT name FROM (SHOW SEMANTIC VIEWS)").fetchall()
            assert names == [("v_earlier",)], names
        finally:
            conn.close()


if __name__ == "__main__":
    # `uv run test/integration/test_load_extension_twice_idempotent.py`
    # — invoke the test directly so the file is self-contained, mirroring
    # the convention established by `test_concurrent_reads_per_call_conn.py`.
    try:
        test_load_extension_twice_idempotent()
        test_connection_opened_after_load()
        test_connection_open_before_load()
    except AssertionError as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        sys.exit(1)
    except Exception as exc:  # noqa: BLE001 — surface anything else clearly
        import traceback

        traceback.print_exc()
        print(f"ERROR: {exc}", file=sys.stderr)
        sys.exit(1)
    print("PASSED: load_extension_twice_idempotent")
    sys.exit(0)
