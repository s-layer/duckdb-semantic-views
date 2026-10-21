#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0", "pytest>=7.0"]
# requires-python = ">=3.10"
# ///
"""
Concurrent per-call Connection writes regression (Phase 65.1 Plan 05, WR-04).

Companion to ``test_concurrent_reads_per_call_conn.py`` covering the writes
side of the per-call Connection model introduced in Phase 65. N threads
each cycle through a mix of ``CREATE SEMANTIC VIEW IF NOT EXISTS``,
``DROP SEMANTIC VIEW IF EXISTS``, and ``ALTER SEMANTIC VIEW ... SET COMMENT``
on **overlapping** view names (``v_0`` .. ``v_3``) so 8 threads contend on
only 4 names — maximising the race-guard SQL surface and the
``INSERT OR IGNORE``/``json_merge_patch`` paths exercised on the caller's
connection.

Per-call outcomes are categorised into five buckets:

- ``success`` — the operation completed without error.
- ``already_exists`` — caught error matching the canonical "already exists"
  wording (from the race-guard SQL emitted by ``parser_override``).
- ``constraint_violation`` — PK violation per TECH-DEBT 23 (D-20). When two
  ``CREATE IF NOT EXISTS`` transactions both evaluate against a snapshot
  in which the row is absent and both attempt the INSERT, the loser sees
  either ``Constraint Error: Duplicate key "name: v_X" violates primary
  key constraint`` (single-statement check-time path) or
  ``TransactionContext Error: Failed to commit: PRIMARY KEY or UNIQUE
  constraint violation: duplicate key "v_X"`` (commit-time path from the
  race-guard rewrite). Both shapes are documented behaviour.
- ``tuple_conflict`` — DuckDB's optimistic-concurrency rollback for two
  transactions that both wrote to (DELETE / UPDATE) the same row of
  ``semantic_layer._definitions``. Surfaces as
  ``TransactionContext Error: Conflict on tuple deletion!`` for racing
  DROPs and ALTERs on the same view name. Documented as the upstream
  serialisation contract — semantic-view writes against
  ``_definitions(name)`` PK serialise the same way any other
  concurrent DDL does.
- ``unknown_error`` — anything else. The test fails loudly with the full
  diagnostic text on any unknown error.

The test asserts:

1. Zero ``unknown_error`` outcomes — any unexpected error shape fails the
   test with full repr of the exception.
2. No thread hangs — all threads ``join()`` within ``WALL_BUDGET_SECONDS``
   plus a small slack budget.
3. Final-state check: ``list_semantic_views()`` content is a subset of the
   union of ``success``-marked CREATEs (so we did not conjure rows from
   nowhere). Exact equality is intentionally NOT asserted — race ordering
   means a thread's CREATE may be followed by another thread's DROP, so
   the strict invariant is "no spurious rows", not "exactly the union
   minus drops" (researcher Pitfall 4 / Assumption A4).

Exit codes:
    0 = test passed
    1 = test failed
"""

import random
import sys
import tempfile
import threading
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()

NUM_THREADS = 8
CALLS_PER_THREAD = 10
WALL_BUDGET_SECONDS = 30.0

# Overlapping name pool — 8 threads cycling through 4 view names maximises
# contention on the race-guard SQL and the _definitions PK.
VIEW_NAMES = ["v_0", "v_1", "v_2", "v_3"]


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
    # A single underlying physical table for all CREATE statements to point
    # at. No pre-existing semantic views — the threads race to CREATE them.
    "CREATE TABLE IF NOT EXISTS t1 (id INTEGER, name VARCHAR)",
    "INSERT INTO t1 VALUES (1, 'a'), (2, 'b'), (3, 'c')",
]


@dataclass
class WorkerResult:
    idx: int
    ok: bool = False
    success: int = 0
    already_exists: int = 0
    constraint_violation: int = 0
    tuple_conflict: int = 0
    unknown_errors: list = field(default_factory=list)
    # Per-thread success ledgers — the final-state check folds these across
    # all threads.
    create_successes: list = field(default_factory=list)
    drop_successes: list = field(default_factory=list)


def categorise(exc: Exception) -> str:
    """Map an exception raised by a CREATE/DROP/ALTER to one of four known
    categories, or ``unknown_error`` for anything else."""
    msg = str(exc)
    if "already exists" in msg:
        return "already_exists"
    # Two PK-violation shapes for CREATE IF NOT EXISTS races (TECH-DEBT 23):
    #   1. Single-statement check-time: 'Constraint Error: Duplicate key
    #      "name: v_X" violates primary key constraint'
    #   2. Multi-statement commit-time from the race-guard rewrite:
    #      'TransactionContext Error: Failed to commit: PRIMARY KEY or
    #       UNIQUE constraint violation: duplicate key "v_X"'
    if "Duplicate key" in msg and "primary key constraint" in msg:
        return "constraint_violation"
    if (
        "PRIMARY KEY or UNIQUE constraint violation" in msg
        and "duplicate key" in msg
    ):
        return "constraint_violation"
    # Optimistic-concurrency rollback for two transactions that both
    # wrote to the same row of _definitions (concurrent DROP/ALTER on the
    # same view name). DuckDB raises this as a TransactionContext error
    # at commit time.
    if "Conflict on tuple deletion" in msg:
        return "tuple_conflict"
    return "unknown_error"


def worker(
    thread_conn,
    thread_id: int,
    result: WorkerResult,
    gate: threading.Event,
    view_names: list,
) -> None:
    """Each thread runs ``CALLS_PER_THREAD`` random operations from the set
    {CREATE, DROP, ALTER} on a random view name from ``view_names``.

    Per-thread RNG is seeded deterministically from ``thread_id`` for
    reproducibility while still producing distinct schedules across threads.
    """
    rng = random.Random(thread_id)
    gate.wait()
    deadline = time.monotonic() + WALL_BUDGET_SECONDS

    for i in range(CALLS_PER_THREAD):
        if time.monotonic() > deadline:
            result.unknown_errors.append(
                f"thread {thread_id} exceeded wall budget mid-run at iter {i}"
            )
            return

        view = rng.choice(view_names)
        op = rng.choice(["create", "drop", "alter"])
        try:
            if op == "create":
                thread_conn.execute(
                    f"CREATE SEMANTIC VIEW IF NOT EXISTS {view} AS "
                    f"TABLES (a AS t1 PRIMARY KEY (id)) "
                    f"DIMENSIONS (a.name AS a.name) "
                    f"METRICS (a.cnt AS count(*))"
                )
                result.success += 1
                result.create_successes.append(view)
            elif op == "drop":
                thread_conn.execute(f"DROP SEMANTIC VIEW IF EXISTS {view}")
                result.success += 1
                result.drop_successes.append(view)
            else:  # alter
                # Use IF EXISTS so a race where the target was just DROPped
                # is a silent no-op rather than a "does not exist" error.
                # This matches realistic bootstrap-retry script shapes.
                thread_conn.execute(
                    f"ALTER SEMANTIC VIEW IF EXISTS {view} "
                    f"SET COMMENT = 'thread_{thread_id}_iter_{i}'"
                )
                result.success += 1
        except Exception as exc:  # noqa: BLE001 — surface every failure type
            cat = categorise(exc)
            if cat == "already_exists":
                result.already_exists += 1
            elif cat == "constraint_violation":
                result.constraint_violation += 1
            elif cat == "tuple_conflict":
                result.tuple_conflict += 1
            else:
                result.unknown_errors.append(
                    f"thread {thread_id} op={op} view={view}: {exc!r}"
                )

    result.ok = True


def test_concurrent_writes_per_call_conn() -> bool:
    with tempfile.TemporaryDirectory() as tmpdir:
        db_path = str(Path(tmpdir) / "concurrent_writes.duckdb")
        master = open_master(db_path)
        cursors: list = []
        try:
            for stmt in SETUP_SQL:
                master.execute(stmt)

            cursors = [master.cursor() for _ in range(NUM_THREADS)]
            results = [WorkerResult(idx=i) for i in range(NUM_THREADS)]
            gate = threading.Event()
            threads = [
                threading.Thread(
                    target=worker,
                    args=(cursors[i], i, results[i], gate, VIEW_NAMES),
                )
                for i in range(NUM_THREADS)
            ]

            for t in threads:
                t.start()

            start = time.monotonic()
            gate.set()

            for t in threads:
                t.join(timeout=WALL_BUDGET_SECONDS + 5)

            elapsed = time.monotonic() - start

            # 1. No thread may hang past the wall budget + slack.
            for t, r in zip(threads, results):
                if t.is_alive():
                    print(
                        f"FAIL: thread {r.idx} did not finish within "
                        f"{WALL_BUDGET_SECONDS + 5:.1f}s wall budget + slack"
                    )
                    return False

            # 2. Wall budget honoured.
            if elapsed > WALL_BUDGET_SECONDS + 5:
                print(
                    f"FAIL: {NUM_THREADS}×{CALLS_PER_THREAD} concurrent writes "
                    f"took {elapsed:.1f}s (budget {WALL_BUDGET_SECONDS:.1f}s + 5s slack)"
                )
                return False

            # 3. Worker liveness — each worker must have reached its terminal
            #    ok=True assignment. A False here means the worker exited early
            #    via the wall-budget guard (recorded under unknown_errors).
            not_ok = [r for r in results if not r.ok]
            if not_ok:
                for r in not_ok:
                    print(
                        f"FAIL: thread {r.idx} did not complete cleanly; "
                        f"unknown_errors={r.unknown_errors!r}"
                    )
                return False

            # 4. Zero unknown_error outcomes — known races (already_exists,
            #    constraint_violation) are documented behaviour and contribute
            #    nothing to this assertion.
            unknown_total = 0
            for r in results:
                if r.unknown_errors:
                    unknown_total += len(r.unknown_errors)
                    for line in r.unknown_errors:
                        print(f"FAIL: {line}")
            if unknown_total > 0:
                print(
                    f"FAIL: {unknown_total} unknown_error outcomes across "
                    f"{NUM_THREADS} threads"
                )
                return False

            # 5. Final-state check: list_semantic_views() output must be a
            #    subset of the union of every success-marked CREATE. This
            #    catches "spurious rows" — a final view name we never claim
            #    to have created. We intentionally do NOT require equality
            #    with (CREATEs - DROPs); race ordering means a thread's CREATE
            #    can be followed by another thread's DROP and vice versa, so
            #    the strict subset check is the robust invariant (see
            #    Pitfall 4 / Assumption A4 in 65.1-RESEARCH.md).
            union_creates: set = set()
            for r in results:
                union_creates.update(r.create_successes)

            final_rows = master.execute(
                "SELECT name FROM list_semantic_views() ORDER BY name"
            ).fetchall()
            final_names = {row[0] for row in final_rows}

            spurious = final_names - union_creates
            if spurious:
                print(
                    f"FAIL: list_semantic_views() contains rows not present in "
                    f"the union of success-marked CREATEs: {sorted(spurious)!r}; "
                    f"union_creates={sorted(union_creates)!r}; "
                    f"final_names={sorted(final_names)!r}"
                )
                return False

            total_success = sum(r.success for r in results)
            total_already = sum(r.already_exists for r in results)
            total_pk = sum(r.constraint_violation for r in results)
            total_conflict = sum(r.tuple_conflict for r in results)
            print(
                f"PASS: {NUM_THREADS} threads × {CALLS_PER_THREAD} ops = "
                f"{NUM_THREADS * CALLS_PER_THREAD} calls in {elapsed:.2f}s; "
                f"success={total_success} already_exists={total_already} "
                f"constraint_violation={total_pk} tuple_conflict={total_conflict}; "
                f"final views={sorted(final_names)!r}"
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
        ok = test_concurrent_writes_per_call_conn()
        return 0 if ok else 1
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        return 1


if __name__ == "__main__":
    sys.exit(main())
