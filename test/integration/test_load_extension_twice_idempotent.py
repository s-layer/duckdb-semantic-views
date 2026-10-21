#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0", "pytest>=7.0"]
# requires-python = ">=3.10"
# ///
"""
Phase 65.1 Plan 12 (WR-09 D-21) — `LOAD semantic_views` must be idempotent
across repeated calls within a single process.

`ParserExtension::Register` unconditionally appends to
`DBConfig::parser_extensions` (no dedup API), so without the
`sv_register_parser_hooks` idempotence guard added in Plan 12, every LOAD
would grow that list by one entry pointing at the same `sv_parser_override`
— an unbounded "soft leak" called out by 65-REVIEW.md WR-09.

DuckDB does NOT surface a double-register as a user error, so neither
LOAD call throws on the pre-fix binary; the symptom is internal list
bloat which isn't user-visible. The structural pre/post-fix discriminator
lives in `tests/parser_hook_idempotent.rs` (B-07 plan-checker fix —
counts parser-extension entries via the `sv_count_parser_extensions`
helper FFI).

This Python test pins the BEHAVIOURAL contract: a second `LOAD
semantic_views` in the same process must (a) not throw, and (b) leave
the parser-override hook functional so subsequent DDL still flows
through the `sv_parser_override` rewrite path. A regression where list
bloat caused a downstream parser failure (e.g. ambiguous override
dispatch) would surface as the sentinel DDL block failing.

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

            # Second LOAD — D-21 idempotence contract. Pre-fix this did
            # NOT throw either (DuckDB doesn't surface double-register as
            # a user error) so the assertion is "no exception" only; the
            # structural pre/post-fix discriminator lives in the Rust
            # integration test `tests/parser_hook_idempotent.rs`.
            conn.execute("LOAD semantic_views")

            # Sentinel — exercise a DDL statement that goes through the
            # parser_override hook. If the second LOAD had somehow broken
            # the override dispatch (e.g. via duplicate entries causing
            # ambiguous selection in some future DuckDB version), this
            # would surface as a parse error here.
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


if __name__ == "__main__":
    # `uv run test/integration/test_load_extension_twice_idempotent.py`
    # — invoke the test directly so the file is self-contained, mirroring
    # the convention established by `test_concurrent_reads_per_call_conn.py`.
    try:
        test_load_extension_twice_idempotent()
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
