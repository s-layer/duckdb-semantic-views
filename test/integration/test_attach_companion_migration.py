#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
FF-3 (code-review 2026-07-02): the v0.1.0 companion-file migration must never
touch an ATTACHed database.

`init_catalog` imports a sibling `<db>.semantic_views` JSON file into
`semantic_layer._definitions` and then DELETES it. Before FF-3, `init_extension`
resolved the DB path as the FIRST non-empty file from `PRAGMA database_list` —
which, with an in-memory primary and a file-backed ATTACHed database, was the
ATTACHed file. The migration then slurped the attached DB's companion into the
(ephemeral) in-memory primary catalog and `remove_file`'d the source:
cross-catalog data loss. FF-3 ties the migration to the PRIMARY database's own
path (the `database_list` row matching `current_database()` on the load
connection) and skips migration entirely when the primary is in-memory.

  T1: in-memory primary + file-backed ATTACHed DB with a companion file —
      after LOAD, the attached companion is UNTOUCHED and its rows are NOT
      migrated into the in-memory primary.
  T2: file-backed PRIMARY with its own companion still migrates correctly
      (regression guard: the legitimate primary migration is preserved).

Exit codes: 0 = all passed, 1 = at least one failed.
"""

import json
import os
import shutil
import sys
import tempfile
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()


def _companion_payload() -> dict:
    # v0.1.0 companion format: {view_name: definition_json_string}.
    return {
        "legacy_view": json.dumps(
            {"tables": [{"alias": "t", "table": "t"}], "dimensions": [], "metrics": []}
        )
    }


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


def test_attached_companion_untouched() -> None:
    """In-memory primary + file-backed ATTACH: the attached companion is left
    in place and NOT migrated into the ephemeral in-memory primary catalog."""
    import duckdb

    d = tempfile.mkdtemp()
    attached = os.path.join(d, "adb.db")
    companion = attached + ".semantic_views"
    # Materialize the attached DB file, then plant its v0.1.0 companion.
    c0 = duckdb.connect(attached)
    c0.execute("CREATE TABLE t (x INTEGER)")
    c0.close()
    with open(companion, "w") as f:
        json.dump(_companion_payload(), f)
    assert os.path.exists(companion)

    conn = duckdb.connect(
        ":memory:",
        config={"allow_unsigned_extensions": "true", "extension_directory": EXT_DIR},
    )
    try:
        # ATTACH before LOAD so the migration sees the attached file at init.
        conn.execute(f"ATTACH '{attached}' AS adb")
        _install_and_load(conn)

        assert os.path.exists(companion), (
            "FF-3 regression: the ATTACHed DB's companion file was deleted by "
            "the primary's migration"
        )
        rows = conn.execute(
            "SELECT count(*) FROM memory.semantic_layer._definitions"
        ).fetchone()[0]
        assert rows == 0, (
            f"FF-3 regression: attached companion was migrated into the in-memory "
            f"primary ({rows} rows)"
        )
    finally:
        conn.close()
        shutil.rmtree(d, ignore_errors=True)


def test_primary_companion_still_migrates() -> None:
    """A file-backed PRIMARY database's own companion is still migrated then
    deleted — the legitimate v0.1.0 migration must keep working."""
    import duckdb

    d = tempfile.mkdtemp()
    primary = os.path.join(d, "main.db")
    companion = primary + ".semantic_views"
    c0 = duckdb.connect(primary)
    c0.close()
    with open(companion, "w") as f:
        json.dump(_companion_payload(), f)

    conn = duckdb.connect(
        primary,
        config={"allow_unsigned_extensions": "true", "extension_directory": EXT_DIR},
    )
    try:
        _install_and_load(conn)

        assert not os.path.exists(companion), (
            "the PRIMARY database's own companion should be migrated then deleted"
        )
        names = [
            r[0]
            for r in conn.execute(
                "SELECT name FROM semantic_layer._definitions"
            ).fetchall()
        ]
        assert names == ["legacy_view"], f"expected migrated legacy_view, got {names}"
    finally:
        conn.close()
        shutil.rmtree(d, ignore_errors=True)


if __name__ == "__main__":
    results = [
        run_test("test_attached_companion_untouched", test_attached_companion_untouched),
        run_test(
            "test_primary_companion_still_migrates",
            test_primary_companion_still_migrates,
        ),
    ]
    passed = sum(results)
    total = len(results)
    print(f"\n{'=' * 60}\nSUMMARY: {passed}/{total} tests passed\n{'=' * 60}")
    sys.exit(0 if passed == total else 1)
