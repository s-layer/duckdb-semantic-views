#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
Regression test for the v0.8.0 silent-truncation bug in the parser_override
buffer path.

Pre-fix the C++ shim allocated a fixed 64 KB std::string for the rewritten
SQL emitted by sv_parser_override_rust, and the Rust-side `write_to_buffer`
helper silently truncated anything over the cap. For semantic views with
many metrics / dimensions, the rewritten
``INSERT INTO semantic_layer._definitions VALUES ('<name>', '<json>')``
exceeded 64 KB, the C++ side reparsed a truncated SQL string, and DuckDB
surfaced a confusing ``Parser Error: syntax error at or near …`` instead
of either succeeding or producing a real diagnostic.

Post-fix the FFI hands the C++ caller a heap-owned byte buffer (no cap),
released via ``sv_free_buffer`` through an RAII guard.

This test creates a view with enough metrics to push the rewritten INSERT
well past 64 KB, runs it inside a transaction (exercising the
parser_override path that the fix targets), and verifies it commits
cleanly and produces the expected metric count.

Usage:
    uv run test/integration/test_large_view_rewrite.py

Exit codes:
    0 = passed
    1 = failed
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path


def build_large_view_sql(view_name: str, num_metrics: int) -> str:
    """Compose a CREATE SEMANTIC VIEW with enough metrics + long comments
    that the enriched JSON definition exceeds 64 KB once rewritten as
    ``INSERT INTO _definitions VALUES ('<name>', '<json>')``.

    Each metric carries a ~600-byte comment; 200 metrics × 600 bytes is
    ~120 KB of comment text alone, comfortably past the legacy cap.
    """
    long_comment = "x" * 600
    metrics = ",\n            ".join(
        f"o.metric_{i:04d} AS SUM(o.amount) COMMENT = '{long_comment}'"
        for i in range(num_metrics)
    )
    return f"""
        CREATE SEMANTIC VIEW {view_name} AS
          TABLES (o AS large_orders PRIMARY KEY (id))
          DIMENSIONS (o.region AS o.region)
          METRICS (
            {metrics}
          )
    """


def run_test() -> int:
    import duckdb

    ext_dir = get_ext_dir()
    ext_path = get_extension_path()
    if not ext_path.exists():
        print(f"ERROR: extension not found at {ext_path}")
        print("Run `just build` first.")
        return 1

    conn = duckdb.connect(
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": ext_dir,
        }
    )
    conn.execute(f"FORCE INSTALL '{ext_path}'")
    conn.execute("LOAD semantic_views")

    conn.execute(
        "CREATE TABLE large_orders ("
        "id INTEGER PRIMARY KEY, region VARCHAR, amount DECIMAL(10,2))"
    )
    conn.execute("INSERT INTO large_orders VALUES (1, 'US', 100.00)")

    num_metrics = 200
    create_sql = build_large_view_sql("large_view", num_metrics)
    # The rewritten SQL is roughly: outer INSERT (~80 bytes of fixed text)
    # + the enriched-JSON definition. The enriched JSON serialises every
    # metric (name + expr + comment + access_modifier + ...). The original
    # CREATE statement itself is already >120 KB once we factor in
    # comments × num_metrics — confirm this is well past the legacy cap.
    print(f"CREATE statement length: {len(create_sql)} bytes")
    assert len(create_sql) > 64 * 1024, (
        f"Test input ({len(create_sql)} bytes) does not exceed the legacy "
        "64 KB cap; bump num_metrics or comment length."
    )

    # Exercise the transactional parser_override path (the fix's primary site).
    print("Running BEGIN; CREATE SEMANTIC VIEW (large) ; COMMIT …")
    try:
        conn.execute("BEGIN")
        conn.execute(create_sql)
        conn.execute("COMMIT")
    except Exception as e:
        print(f"  FAIL: CREATE raised {type(e).__name__}: {e}")
        return 1

    # Verify the view was actually persisted and that all metrics survived
    # the round-trip (silent truncation would have either failed parse or
    # produced a damaged metrics list).
    (count,) = conn.execute(
        "SELECT count(*) FROM list_semantic_views() WHERE name = 'large_view'"
    ).fetchone()
    if count != 1:
        print(f"  FAIL: list_semantic_views() returned {count}, expected 1")
        return 1

    (metric_count,) = conn.execute(
        "SELECT count(*) FROM show_semantic_metrics('large_view')"
    ).fetchone()
    if metric_count != num_metrics:
        print(f"  FAIL: show_semantic_metrics() returned {metric_count}, "
              f"expected {num_metrics}")
        return 1

    # And confirm a query round-trip works on a representative metric.
    sample = conn.execute(
        "SELECT * FROM semantic_view('large_view', "
        "dimensions := ['region'], metrics := ['metric_0000', 'metric_0199'])"
    ).fetchall()
    if not sample:
        print("  FAIL: semantic_view query returned no rows")
        return 1

    # Cleanup so reruns are idempotent against a freshly-attached file.
    conn.execute("DROP SEMANTIC VIEW large_view")

    print(
        f"  PASS: created view with {num_metrics} metrics "
        f"({len(create_sql)}-byte CREATE), commit/round-trip succeeded"
    )
    return 0


if __name__ == "__main__":
    sys.exit(run_test())
