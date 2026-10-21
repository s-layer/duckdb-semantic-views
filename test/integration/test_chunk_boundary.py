#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Regression test for chunked emission past DataChunk capacity (2048 rows)
in the bind-materialized C++ table functions.

Pre-fix, `sv_list_semantic_views_function`, `sv_emit_varchar_rows`, and
`sv_emit_varchar_bool_rows` (cpp/src/shim.cpp) emitted their ENTIRE
bind-materialized row set in a single exec call, gated by a one-shot
`emitted` flag. The exec-callback `DataChunk` has capacity
STANDARD_VECTOR_SIZE (2048); `Vector::SetValue` performs no bounds check
in release builds, so row 2049 wrote a 16-byte string_t past the end of
the vector's data buffer — silent heap corruption reachable from plain
SQL (finding MS-1 in _notes/code-review-2026-07-02.md).

Post-fix the local state carries a `next_row` cursor and each exec call
emits at most STANDARD_VECTOR_SIZE rows.

Covers all three fixed emitters:
  A. sv_emit_varchar_rows      — show_semantic_dimensions_all() over a view
                                 with 2,100 dimensions (also exercised via
                                 describe_semantic_view on the same view).
  B. sv_emit_varchar_bool_rows — show_semantic_dimensions_for_metric() on
                                 the same view (join-free views return every
                                 dimension, so 2,100 rows).
  C. sv_list_semantic_views    — 2,100 registered views.

Assertions check exact row counts AND full content integrity (every
expected name present exactly once), so a pre-fix build that survives the
overflow without crashing still fails on missing/garbled rows.

Usage:
    uv run test/integration/test_chunk_boundary.py

Exit codes:
    0 = all scenarios passed
    1 = at least one scenario failed
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

# One chunk is 2048 rows; 2100 forces a second chunk with a 52-row tail.
N_DIMS = 2100
N_VIEWS = 2100


def build_wide_view_sql(view_name: str, num_dims: int) -> str:
    dims = ",\n            ".join(
        f"o.d_{i:04d} AS o.region" for i in range(num_dims)
    )
    return f"""
        CREATE SEMANTIC VIEW {view_name} AS
          TABLES (o AS cb_orders PRIMARY KEY (id))
          DIMENSIONS (
            {dims}
          )
          METRICS (o.revenue AS SUM(o.amount))
    """


def run_tests() -> int:
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
        "CREATE TABLE cb_orders ("
        "id INTEGER PRIMARY KEY, region VARCHAR, amount DECIMAL(10,2))"
    )
    conn.execute("INSERT INTO cb_orders VALUES (1, 'US', 100.00)")

    failures = 0

    def check(label: str, ok: bool, detail: str = "") -> None:
        nonlocal failures
        status = "PASS" if ok else "FAIL"
        print(f"  {status}: {label}" + (f" — {detail}" if detail else ""))
        if not ok:
            failures += 1

    # --- Scenario A: varchar emitter (show_semantic_dimensions_all) -------
    print(f"Scenario A: view with {N_DIMS} dimensions")
    conn.execute(build_wide_view_sql("cb_wide", N_DIMS))

    expected_names = {f"d_{i:04d}" for i in range(N_DIMS)}
    rows = conn.execute(
        "SELECT name FROM show_semantic_dimensions_all()"
    ).fetchall()
    names = [r[0] for r in rows]
    check(
        f"show_semantic_dimensions_all row count == {N_DIMS}",
        len(names) == N_DIMS,
        f"got {len(names)}",
    )
    check(
        "show_semantic_dimensions_all names intact across chunk boundary",
        set(names) == expected_names and len(set(names)) == len(names),
        f"missing={len(expected_names - set(names))} "
        f"unexpected={len(set(names) - expected_names)} "
        f"dupes={len(names) - len(set(names))}",
    )

    describe_count = conn.execute(
        "SELECT count(*) FROM describe_semantic_view('cb_wide')"
    ).fetchone()[0]
    check(
        f"describe_semantic_view emits >= {N_DIMS} rows without corruption",
        describe_count >= N_DIMS,
        f"got {describe_count}",
    )

    # --- Scenario B: varchar+bool emitter (dims for metric) ---------------
    print(f"Scenario B: show_semantic_dimensions_for_metric over {N_DIMS} dims")
    rows_b = conn.execute(
        "SELECT name, required FROM show_semantic_dimensions_for_metric('cb_wide', 'revenue')"
    ).fetchall()
    names_b = [r[0] for r in rows_b]
    check(
        f"show_semantic_dimensions_for_metric row count == {N_DIMS}",
        len(names_b) == N_DIMS,
        f"got {len(names_b)}",
    )
    check(
        "for_metric names intact across chunk boundary",
        set(names_b) == expected_names,
        f"missing={len(expected_names - set(names_b))}",
    )
    # `required` is True only for window-spec dims; a plain SUM metric marks
    # every dim False. A corrupted BOOLEAN vector past row 2048 would show
    # up as spurious True (or non-bool) values here.
    check(
        "for_metric BOOLEAN column intact (all False for plain SUM metric)",
        all(r[1] is False for r in rows_b),
        f"non-false={sum(1 for r in rows_b if r[1] is not False)}",
    )

    # --- Scenario C: list_semantic_views emitter ---------------------------
    print(f"Scenario C: {N_VIEWS} registered views (this takes a few seconds)")
    t0 = time.monotonic()
    for i in range(N_VIEWS - 1):  # cb_wide is view #1
        conn.execute(
            f"CREATE SEMANTIC VIEW cb_v_{i:04d} AS "
            "TABLES (o AS cb_orders PRIMARY KEY (id)) "
            "DIMENSIONS (o.region AS o.region) "
            "METRICS (o.n AS COUNT(*))"
        )
    print(f"  ({N_VIEWS - 1} CREATEs in {time.monotonic() - t0:.1f}s)")

    expected_views = {"cb_wide"} | {f"cb_v_{i:04d}" for i in range(N_VIEWS - 1)}
    view_names = [
        r[0]
        for r in conn.execute("SELECT name FROM list_semantic_views()").fetchall()
    ]
    check(
        f"list_semantic_views row count == {N_VIEWS}",
        len(view_names) == N_VIEWS,
        f"got {len(view_names)}",
    )
    check(
        "list_semantic_views names intact across chunk boundary",
        set(view_names) == expected_views and len(set(view_names)) == len(view_names),
        f"missing={len(expected_views - set(view_names))} "
        f"dupes={len(view_names) - len(set(view_names))}",
    )

    print()
    if failures:
        print(f"FAILED: {failures} assertion(s)")
        return 1
    print("ALL PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(run_tests())
