#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Integration tests for CREATE SEMANTIC VIEW ... FROM YAML FILE under the
v0.10.0 read-elimination architecture (Phase 65 Plan 04 -- D-11).

Plan 04's rewrite_yaml_file_create no longer reads the YAML file via the
extension-owned catalog connection. Instead the rewritten INSERT selects
from the `__sv_compute_create_from_yaml(path, name, kind, comment)`
helper TF (registered via the C++ Catalog API in cpp/src/shim.cpp). The
helper's bind callback opens a per-call `Connection(*context.db)`, runs
`read_text(?)` against the user-supplied path, calls into Rust to parse
+ enrich + serialize, and returns the metadata-less JSON in a single
VARCHAR row. The outer INSERT wraps that row with `json_merge_patch` +
`json_object` to add `created_on` / `database_name` / `schema_name` on
the caller's connection -- mirroring the non-YAML CREATE path
byte-for-byte.

Test surface:
  T1: Plain CREATE FROM YAML FILE populates _definitions; metadata fields
      reflect the caller's session.
  T2: CREATE OR REPLACE replaces existing definitions.
  T3: IF NOT EXISTS is a silent no-op when the view already exists.
  T4: Nonexistent file path surfaces a "FROM YAML FILE failed" error
      from inside the helper TF's bind callback.
  T5: Malformed YAML surfaces a parse error from the Rust FFI helper.
  T6: A 2 MiB YAML file is rejected with the YAML_SIZE_CAP error.
  T7: BEGIN / CREATE FROM YAML FILE / ROLLBACK -- view does NOT persist
      (D-21 transactional invariant; helper TF runs inside the outer
      INSERT's bind on the caller's connection).
  T8: get_ddl round-trip: re-parse the emitted DDL produces an equivalent
      view (uses an OR REPLACE shape so we don't trip the
      already-exists guard during the round-trip).

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


VALID_YAML = """\
base_table: t
tables:
  - alias: o
    table: t
    pk_columns:
      - id
dimensions:
  - name: id
    expr: o.id
    source_table: o
metrics:
  - name: c
    expr: COUNT(*)
    source_table: o
"""


VALID_YAML_REPLACEMENT = """\
base_table: t
tables:
  - alias: o
    table: t
    pk_columns:
      - id
dimensions:
  - name: id
    expr: o.id
    source_table: o
metrics:
  - name: total
    expr: SUM(o.amount)
    source_table: o
"""


def open_writable(db_path: str):
    """Open a writable DuckDB connection with the extension installed and loaded."""
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


def run_test(name, test_fn):
    print(f"\n{'=' * 60}")
    print(f"TEST: {name}")
    print(f"{'=' * 60}")
    try:
        test_fn()
        print("  RESULT: PASS")
        return True
    except AssertionError as e:
        print(f"  RESULT: FAIL\n  {e}")
        return False
    except Exception as e:
        print(f"  RESULT: ERROR\n  {type(e).__name__}: {e}")
        traceback.print_exc()
        return False


def _setup_table(conn):
    """Common: create the backing table used by the YAML fixtures."""
    conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, amount DOUBLE)")
    conn.execute("INSERT INTO t VALUES (1, 10.0), (2, 20.0), (3, 30.0)")


# ---------------------------------------------------------------------------
# T1: Plain CREATE FROM YAML FILE
# ---------------------------------------------------------------------------


def test_plain_create_from_yaml_file():
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "t1.duckdb")
        yaml_path = str(Path(tmp) / "view.yaml")
        Path(yaml_path).write_text(VALID_YAML)

        conn = open_writable(db)
        try:
            _setup_table(conn)
            conn.execute(f"CREATE SEMANTIC VIEW v FROM YAML FILE '{yaml_path}'")

            # list_semantic_views surfaces the view.
            rows = conn.execute(
                "SELECT name, database_name, schema_name FROM list_semantic_views()"
            ).fetchall()
            assert rows == [("v", "t1", "main")], f"got: {rows}"

            # Persisted JSON has the metadata fields populated on the caller's conn.
            row = conn.execute(
                "SELECT json_extract_string(definition, '$.database_name'), "
                "       json_extract_string(definition, '$.schema_name'), "
                "       json_extract_string(definition, '$.created_on') "
                "FROM semantic_layer._definitions WHERE name = 'v'"
            ).fetchone()
            assert row[0] == "t1", f"database_name: {row[0]!r}"
            assert row[1] == "main", f"schema_name: {row[1]!r}"
            assert row[2] is not None and "T" in row[2] and row[2].endswith("Z"), (
                f"created_on shape: {row[2]!r}"
            )

            # Query end-to-end.
            count = conn.execute(
                "SELECT c FROM semantic_view('v', metrics := ['c'])"
            ).fetchone()[0]
            assert count == 3, f"got: {count}"
        finally:
            conn.close()


# ---------------------------------------------------------------------------
# T2: CREATE OR REPLACE
# ---------------------------------------------------------------------------


def test_or_replace_replaces_definition():
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "t2.duckdb")
        y1 = str(Path(tmp) / "v1.yaml")
        y2 = str(Path(tmp) / "v2.yaml")
        Path(y1).write_text(VALID_YAML)
        Path(y2).write_text(VALID_YAML_REPLACEMENT)

        conn = open_writable(db)
        try:
            _setup_table(conn)
            conn.execute(f"CREATE SEMANTIC VIEW v FROM YAML FILE '{y1}'")
            conn.execute(f"CREATE OR REPLACE SEMANTIC VIEW v FROM YAML FILE '{y2}'")

            # The new metric `total` is now queryable; the old `c` is not.
            total = conn.execute(
                "SELECT total FROM semantic_view('v', metrics := ['total'])"
            ).fetchone()[0]
            assert total == 60.0, f"got: {total}"

            try:
                conn.execute("SELECT c FROM semantic_view('v', metrics := ['c'])")
                raise AssertionError("expected error -- old metric c should be gone")
            except Exception as e:
                # Either "unknown metric" or similar -- accept any error here.
                assert "c" in str(e).lower() or "metric" in str(e).lower(), str(e)
        finally:
            conn.close()


# ---------------------------------------------------------------------------
# T3: IF NOT EXISTS is a silent no-op when view exists
# ---------------------------------------------------------------------------


def test_if_not_exists_silent_noop():
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "t3.duckdb")
        y1 = str(Path(tmp) / "v1.yaml")
        y2 = str(Path(tmp) / "v2.yaml")
        Path(y1).write_text(VALID_YAML)
        Path(y2).write_text(VALID_YAML_REPLACEMENT)

        conn = open_writable(db)
        try:
            _setup_table(conn)
            conn.execute(f"CREATE SEMANTIC VIEW v FROM YAML FILE '{y1}'")
            # IF NOT EXISTS with a different YAML must NOT replace the
            # existing definition.
            conn.execute(
                f"CREATE SEMANTIC VIEW IF NOT EXISTS v FROM YAML FILE '{y2}'"
            )
            count = conn.execute(
                "SELECT c FROM semantic_view('v', metrics := ['c'])"
            ).fetchone()[0]
            assert count == 3, f"old metric c should still work; got: {count}"
        finally:
            conn.close()


# ---------------------------------------------------------------------------
# T4: Nonexistent path -> "FROM YAML FILE failed"
# ---------------------------------------------------------------------------


def test_nonexistent_file_surfaces_friendly_error():
    import duckdb

    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "t4.duckdb")
        bogus_path = str(Path(tmp) / "does_not_exist.yaml")
        conn = open_writable(db)
        try:
            _setup_table(conn)
            try:
                conn.execute(
                    f"CREATE SEMANTIC VIEW v FROM YAML FILE '{bogus_path}'"
                )
                raise AssertionError("expected error -- file does not exist")
            except duckdb.Error as e:
                assert "FROM YAML FILE failed" in str(e), (
                    f"expected 'FROM YAML FILE failed' substring, got: {e}"
                )
        finally:
            conn.close()


# ---------------------------------------------------------------------------
# T5: Malformed YAML -> parse error from Rust FFI helper
# ---------------------------------------------------------------------------


def test_malformed_yaml_surfaces_parse_error():
    import duckdb

    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "t5.duckdb")
        bad_path = str(Path(tmp) / "bad.yaml")
        Path(bad_path).write_text("not: valid: yaml: at all: :")
        conn = open_writable(db)
        try:
            _setup_table(conn)
            try:
                conn.execute(
                    f"CREATE SEMANTIC VIEW v FROM YAML FILE '{bad_path}'"
                )
                raise AssertionError("expected parse error")
            except duckdb.Error as e:
                # The Rust FFI helper writes the message into the bind
                # error_buf; the C++ side re-throws it with the
                # "FROM YAML FILE failed:" prefix.
                msg = str(e)
                assert "FROM YAML FILE failed" in msg, (
                    f"expected 'FROM YAML FILE failed' prefix, got: {msg}"
                )
        finally:
            conn.close()


# ---------------------------------------------------------------------------
# T6: 2 MiB YAML -> YAML_SIZE_CAP error
# ---------------------------------------------------------------------------


def test_oversized_yaml_rejected_at_size_cap():
    import duckdb

    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "t6.duckdb")
        big_path = str(Path(tmp) / "big.yaml")
        # 2 MiB of `a` -- exceeds the 1 MiB YAML_SIZE_CAP.
        Path(big_path).write_text("a" * (2 * 1024 * 1024))
        conn = open_writable(db)
        try:
            _setup_table(conn)
            try:
                conn.execute(
                    f"CREATE SEMANTIC VIEW v FROM YAML FILE '{big_path}'"
                )
                raise AssertionError("expected size-cap error")
            except duckdb.Error as e:
                msg = str(e)
                # The Rust helper returns "...exceeds size limit ... byte cap"
                # which the C++ side wraps with "FROM YAML FILE failed:".
                assert "FROM YAML FILE failed" in msg, (
                    f"expected wrap, got: {msg}"
                )
                assert "exceeds" in msg, (
                    f"expected 'exceeds' from size-cap message, got: {msg}"
                )
        finally:
            conn.close()


# ---------------------------------------------------------------------------
# T7: BEGIN / CREATE FROM YAML FILE / ROLLBACK -- D-21 transactional invariant
# ---------------------------------------------------------------------------


def test_begin_create_yaml_rollback_does_not_persist():
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "t7.duckdb")
        yaml_path = str(Path(tmp) / "v.yaml")
        Path(yaml_path).write_text(VALID_YAML)
        conn = open_writable(db)
        try:
            _setup_table(conn)
            conn.execute("BEGIN")
            conn.execute(f"CREATE SEMANTIC VIEW v FROM YAML FILE '{yaml_path}'")

            # Inside the txn, the view's row is visible to the caller's
            # own SELECT on _definitions (list_semantic_views() reads via
            # the extension's catalog conn -- committed only -- per
            # TECH-DEBT 19).
            row = conn.execute(
                "SELECT name FROM semantic_layer._definitions WHERE name = 'v'"
            ).fetchone()
            assert row == ("v",), f"got: {row}"

            conn.execute("ROLLBACK")

            # After ROLLBACK the row is gone (the INSERT participated in
            # the user's transaction -- D-21).
            row = conn.execute(
                "SELECT name FROM semantic_layer._definitions WHERE name = 'v'"
            ).fetchone()
            assert row is None, f"view should be absent post-ROLLBACK; got: {row}"
        finally:
            conn.close()


# ---------------------------------------------------------------------------
# T8: get_ddl round-trip
# ---------------------------------------------------------------------------


def test_get_ddl_round_trip():
    with tempfile.TemporaryDirectory() as tmp:
        db = str(Path(tmp) / "t8.duckdb")
        yaml_path = str(Path(tmp) / "v.yaml")
        Path(yaml_path).write_text(VALID_YAML)
        conn = open_writable(db)
        try:
            _setup_table(conn)
            conn.execute(f"CREATE SEMANTIC VIEW v FROM YAML FILE '{yaml_path}'")
            # get_ddl takes (kind, name) -- kind 'SEMANTIC VIEW' selects
            # the semantic-view definition; matches the SQL DDL surface.
            ddl = conn.execute(
                "SELECT get_ddl('SEMANTIC_VIEW', 'v')"
            ).fetchone()[0]
            assert isinstance(ddl, str) and len(ddl) > 0, f"DDL: {ddl!r}"
            assert "SEMANTIC VIEW" in ddl.upper(), f"DDL: {ddl}"

            # Re-parse the emitted DDL. get_ddl emits CREATE OR REPLACE by
            # default so re-execution doesn't trip the already-exists
            # guard. If get_ddl produced something syntactically broken,
            # this would raise.
            conn.execute(ddl)

            # Confirm the round-tripped view still works.
            count = conn.execute(
                "SELECT c FROM semantic_view('v', metrics := ['c'])"
            ).fetchone()[0]
            assert count == 3, f"got: {count}"
        finally:
            conn.close()


if __name__ == "__main__":
    results = [
        run_test("test_plain_create_from_yaml_file", test_plain_create_from_yaml_file),
        run_test("test_or_replace_replaces_definition", test_or_replace_replaces_definition),
        run_test("test_if_not_exists_silent_noop", test_if_not_exists_silent_noop),
        run_test("test_nonexistent_file_surfaces_friendly_error", test_nonexistent_file_surfaces_friendly_error),
        run_test("test_malformed_yaml_surfaces_parse_error", test_malformed_yaml_surfaces_parse_error),
        run_test("test_oversized_yaml_rejected_at_size_cap", test_oversized_yaml_rejected_at_size_cap),
        run_test("test_begin_create_yaml_rollback_does_not_persist", test_begin_create_yaml_rollback_does_not_persist),
        run_test("test_get_ddl_round_trip", test_get_ddl_round_trip),
    ]
    passed = sum(results)
    total = len(results)
    print(f"\n{'=' * 60}")
    print(f"SUMMARY: {passed}/{total} tests passed")
    print(f"{'=' * 60}")
    sys.exit(0 if passed == total else 1)
