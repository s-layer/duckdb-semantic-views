#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Concurrent per-call Connection regression for the read-side migration
(Phase 65 Plan 05 Batch 3).

After Batch 2 (16 of 17 migrations) and Batch 3 (H2 query_conn retired +
17 dead VTab/VScalar carcasses purged), every read-side bind callback
opens its own per-call `Connection(*context.db)` on the C++ side and
bridges to Rust via a reinterpret_cast borrow of the stack Connection
pointer (see `src/ddl/list.rs` file-level docs). This test stresses the
borrow contract under concurrency: 8 Python threads × 10 calls each =
80 calls of `SHOW SEMANTIC DIMENSIONS FROM v1`. Each call hits the
catalog through its own per-call Connection; none of the threads share
a long-lived extension-owned `duckdb_connection` after Plan 05 ships.

The test asserts:
- All 80 calls succeed (no `duckdb_disconnect` UB on borrowed handles,
  no panic propagation across the C++↔Rust boundary, no catalog-lookup
  failures under contention).
- All 80 result sets are byte-identical (the per-call Connection model
  preserves snapshot-consistent reads — each cursor sees the same row
  set in the absence of concurrent writers).
- Wall-clock duration is under 30 s (catches a regression where the
  per-call Connection becomes synchronously serialised behind a shared
  mutex).

Exit codes:
    0 = test passed
    1 = test failed
"""

import sys
import tempfile
import threading
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()

NUM_THREADS = 8
CALLS_PER_THREAD = 10
WALL_BUDGET_SECONDS = 30.0


def open_master(db_path: str):
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


SETUP_SQL = [
    "CREATE TABLE t (id INTEGER PRIMARY KEY, region VARCHAR, qty DOUBLE)",
    "INSERT INTO t VALUES (1, 'us', 1.0), (2, 'eu', 2.0), (3, 'us', 3.0)",
    """
    CREATE SEMANTIC VIEW v1 AS
    TABLES (a AS t PRIMARY KEY (id))
    DIMENSIONS (a.region AS a.region, a.id AS a.id)
    METRICS (a.total_qty AS SUM(a.qty))
    """,
]


class WorkerResult:
    def __init__(self, idx: int) -> None:
        self.idx = idx
        self.ok = False
        self.error: Exception | None = None
        self.rows_seen: list[tuple[tuple, ...]] = []


def worker(
    thread_conn,
    result: WorkerResult,
    gate: threading.Event,
) -> None:
    # Each thread shares the master's DB instance via .cursor() (own
    # connection state, same underlying Database). After Plan 05 the
    # extension's read-side TFs open their own per-call Connection
    # inside the C++ bind callback — no long-lived extension-owned
    # duckdb_connection is shared between these 8 threads.
    gate.wait()
    try:
        for _ in range(CALLS_PER_THREAD):
            rows = thread_conn.execute(
                "SELECT name, table_name FROM show_semantic_dimensions('v1') ORDER BY name"
            ).fetchall()
            # Capture a hashable snapshot of the row set for cross-thread
            # equality checking.
            result.rows_seen.append(tuple(tuple(r) for r in rows))
        result.ok = True
    except Exception as exc:  # noqa: BLE001 — surface every failure type
        result.error = exc


def test_concurrent_reads_per_call_conn() -> bool:
    with tempfile.TemporaryDirectory() as tmpdir:
        db_path = str(Path(tmpdir) / "concurrent_reads.duckdb")
        master = open_master(db_path)
        cursors: list = []
        try:
            for stmt in SETUP_SQL:
                master.execute(stmt)

            cursors = [master.cursor() for _ in range(NUM_THREADS)]
            results = [WorkerResult(i) for i in range(NUM_THREADS)]
            gate = threading.Event()
            threads = [
                threading.Thread(target=worker, args=(cursors[i], results[i], gate))
                for i in range(NUM_THREADS)
            ]

            for t in threads:
                t.start()

            start = time.monotonic()
            gate.set()

            for t in threads:
                t.join(timeout=WALL_BUDGET_SECONDS + 5)

            elapsed = time.monotonic() - start

            # 1. All threads must have completed within the wall budget.
            for t, r in zip(threads, results):
                if t.is_alive():
                    print(f"FAIL: thread {r.idx} did not finish within budget")
                    return False

            # 2. All 80 calls must have succeeded.
            failures = [r for r in results if not r.ok]
            if failures:
                for r in failures:
                    print(f"FAIL: thread {r.idx} raised: {r.error!r}")
                return False

            # 3. All 80 calls must have produced identical row sets.
            all_snapshots = [snap for r in results for snap in r.rows_seen]
            if len(all_snapshots) != NUM_THREADS * CALLS_PER_THREAD:
                print(
                    f"FAIL: expected {NUM_THREADS * CALLS_PER_THREAD} row snapshots, "
                    f"got {len(all_snapshots)}"
                )
                return False
            baseline = all_snapshots[0]
            for i, snap in enumerate(all_snapshots[1:], start=1):
                if snap != baseline:
                    print(
                        f"FAIL: snapshot {i} diverges from baseline.\n"
                        f"  baseline: {baseline!r}\n"
                        f"  snapshot: {snap!r}"
                    )
                    return False

            # 4. Wall budget guard — catches a regression where per-call
            #    Connection construction becomes serialised behind a shared mutex.
            if elapsed > WALL_BUDGET_SECONDS:
                print(
                    f"FAIL: {NUM_THREADS}×{CALLS_PER_THREAD} concurrent calls took "
                    f"{elapsed:.1f}s (budget {WALL_BUDGET_SECONDS:.1f}s) — possible "
                    f"serialisation regression"
                )
                return False

            print(
                f"PASS: {NUM_THREADS} threads × {CALLS_PER_THREAD} calls = "
                f"{NUM_THREADS * CALLS_PER_THREAD} reads in {elapsed:.2f}s; "
                f"all row sets identical."
            )
            return True
        finally:
            # Close cursors before master so the tempdir cleanup at context
            # exit isn't blocked by held DuckDB handles (notably on Windows
            # where the DB file is locked). Suppress per-handle errors to
            # avoid masking the underlying test outcome.
            for c in cursors:
                try:
                    c.close()
                except Exception:  # noqa: BLE001
                    pass
            try:
                master.close()
            except Exception:  # noqa: BLE001
                pass


def main() -> int:
    try:
        ok = test_concurrent_reads_per_call_conn()
        return 0 if ok else 1
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        return 1


if __name__ == "__main__":
    sys.exit(main())
