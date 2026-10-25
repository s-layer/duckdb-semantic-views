#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Concurrent DDL race-guard regression test (v0.8.0, Phase 60).

Two threads each open the same file-backed DuckDB, BEGIN a transaction, and
attempt to CREATE the same semantic view name. DuckDB's primary-key constraint
on `semantic_layer._definitions(name)` serializes the inserts, so exactly one
thread must succeed and the other must see a clear conflict error.

Before CREATE lowered to an INSERT against `_definitions` (v0.8.0+), CREATE went through the legacy plan_function which committed
out-of-band — concurrency semantics were undefined. v0.8.0 keeps the same
serialization guarantee while collapsing onto a single execution path.

Scope note: these tests cover the CREATE / CREATE IF NOT EXISTS races only.
The non-IF-EXISTS DROP/ALTER existence guard (`SELECT CASE WHEN NOT EXISTS
THEN error('... does not exist')`) is snapshot-consistent with its DML only
inside an explicit transaction; under autocommit the guard and the DML commit
separately, so a drop landing in the window between them is not caught (the
DROP silently affects 0 rows). That guard window is a documented limitation
(FF-1 / TECH-DEBT #27), not something these tests pin — a deterministic
regression for it is not feasible because the window is inherently timing
dependent.

Exit codes:
    0 = test passed
    1 = test failed
"""

import sys
import tempfile
import threading
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()


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


CREATE_SQL = """
CREATE SEMANTIC VIEW shared_view AS
TABLES (a AS t PRIMARY KEY (id))
DIMENSIONS (a.dim AS a.id)
METRICS (a.m AS SUM(a.val))
"""

CREATE_IF_NOT_EXISTS_SQL = """
CREATE SEMANTIC VIEW IF NOT EXISTS ine_view AS
TABLES (a AS t PRIMARY KEY (id))
DIMENSIONS (a.dim AS a.id)
METRICS (a.m AS SUM(a.val))
"""


class WorkerResult:
    def __init__(self, idx: int) -> None:
        self.idx = idx
        self.ok = False
        self.error: Exception | None = None


def worker(thread_conn, sql: str, result: WorkerResult, staged: threading.Barrier) -> None:
    # Force the two transactions to OVERLAP deterministically rather than
    # leaving it to thread-scheduling luck.
    #
    # Each worker opens an explicit transaction, issues its CREATE (which
    # lowers to an INSERT [OR IGNORE] against
    # `semantic_layer._definitions`), and only THEN waits on the shared
    # barrier. Both transactions take their MVCC snapshot at BEGIN — before
    # either has committed — so both see the target name absent and both stage
    # the INSERT. The barrier holds every worker at the post-INSERT point until
    # all have staged, so no COMMIT can land before the other's snapshot is
    # fixed: the first COMMIT wins and the second necessarily hits DuckDB's
    # primary-key write-write conflict. That makes "exactly one success, one
    # failure" deterministic.
    #
    # The previous autocommit form (no BEGIN, a plain Event gate) could let the
    # threads serialize cleanly — one CREATE fully committing before the other
    # took its snapshot. Plain CREATE still errored then (its committed-state
    # guard saw the row), but CREATE IF NOT EXISTS silently no-opped
    # (INSERT OR IGNORE saw the row) → two successes, and the test flaked.
    try:
        thread_conn.execute("BEGIN TRANSACTION")
        thread_conn.execute(sql)
        # INSERT is now staged in this transaction's snapshot; release only
        # once the other worker has staged its INSERT too, so the commits race.
        staged.wait(timeout=20)
        thread_conn.execute("COMMIT")
        result.ok = True
    except Exception as exc:  # noqa: BLE001 - we want every error type
        result.error = exc


def test_concurrent_create_serializes() -> bool:
    with tempfile.TemporaryDirectory() as tmpdir:
        db_path = str(Path(tmpdir) / "concurrent.duckdb")
        master = open_master(db_path)
        master.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val DOUBLE)")
        master.execute("INSERT INTO t VALUES (1, 1.0), (2, 2.0)")

        # Worker connections share the master's DB instance. Each cursor()
        # is a distinct DuckDB connection (own transaction state) so the
        # two threads genuinely race.
        c1 = master.cursor()
        c2 = master.cursor()

        staged = threading.Barrier(2)
        r1, r2 = WorkerResult(1), WorkerResult(2)
        t1 = threading.Thread(target=worker, args=(c1, CREATE_SQL, r1, staged))
        t2 = threading.Thread(target=worker, args=(c2, CREATE_SQL, r2, staged))
        t1.start()
        t2.start()
        t1.join(timeout=30)
        t2.join(timeout=30)
        if t1.is_alive() or t2.is_alive():
            print("FAIL: worker thread did not finish within timeout")
            return False

        successes = [r for r in (r1, r2) if r.ok]
        failures = [r for r in (r1, r2) if not r.ok]

        if len(successes) != 1:
            print(f"FAIL: expected exactly 1 success, got {len(successes)}")
            for r in (r1, r2):
                print(f"  worker {r.idx}: ok={r.ok} err={r.error!r}")
            return False
        if len(failures) != 1:
            print(f"FAIL: expected exactly 1 failure, got {len(failures)}")
            return False

        # Catalog should have exactly one committed row for that name.
        rows = master.execute(
            "SELECT count(*) FROM semantic_layer._definitions WHERE name = 'shared_view'"
        ).fetchone()
        if rows is None or rows[0] != 1:
            print(f"FAIL: expected 1 row in _definitions, got {rows}")
            return False

        # The losing thread should report a meaningful conflict, not a parser
        # error. The exact text comes from DuckDB's PK constraint or the
        # extension's friendly duplicate-name message.
        losing_msg = str(failures[0].error or "")
        if "shared_view" not in losing_msg and "constraint" not in losing_msg.lower():
            print(
                "FAIL: losing thread error did not look like a name conflict: "
                f"{losing_msg!r}"
            )
            return False

        print("PASS: exactly one concurrent CREATE committed")
        return True


def test_concurrent_create_if_not_exists_serializes() -> bool:
    """Cross-connection CREATE IF NOT EXISTS race: PK constraint serializes.

    INSERT OR IGNORE absorbs *same-snapshot* duplicates atomically (handled
    by the in-snapshot fast-path tests in v080_transactional_ddl.test), but
    under DuckDB MVCC two concurrent connections that each see the row
    absent in their own snapshot will both attempt an INSERT, and the
    second commit raises a PK constraint violation. That matches plain
    CREATE concurrency semantics — the silent no-op contract for IF NOT
    EXISTS applies only to in-snapshot duplicates. See TECH-DEBT item 23.

    This regression test pins the failure shape (clear constraint error,
    not silent corruption or parser error) so a future SQL rewrite cannot
    quietly degrade the loser's experience.
    """
    with tempfile.TemporaryDirectory() as tmpdir:
        db_path = str(Path(tmpdir) / "concurrent_ine.duckdb")
        master = open_master(db_path)
        master.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val DOUBLE)")
        master.execute("INSERT INTO t VALUES (1, 1.0), (2, 2.0)")

        c1 = master.cursor()
        c2 = master.cursor()

        staged = threading.Barrier(2)
        r1, r2 = WorkerResult(1), WorkerResult(2)
        t1 = threading.Thread(target=worker, args=(c1, CREATE_IF_NOT_EXISTS_SQL, r1, staged))
        t2 = threading.Thread(target=worker, args=(c2, CREATE_IF_NOT_EXISTS_SQL, r2, staged))
        t1.start()
        t2.start()
        t1.join(timeout=30)
        t2.join(timeout=30)
        if t1.is_alive() or t2.is_alive():
            print("FAIL: worker thread did not finish within timeout (IF NOT EXISTS)")
            return False

        successes = [r for r in (r1, r2) if r.ok]
        failures = [r for r in (r1, r2) if not r.ok]

        if len(successes) != 1:
            print(f"FAIL: expected exactly 1 success, got {len(successes)}")
            for r in (r1, r2):
                print(f"  worker {r.idx}: ok={r.ok} err={r.error!r}")
            return False
        if len(failures) != 1:
            print(f"FAIL: expected exactly 1 failure, got {len(failures)}")
            return False

        # Catalog should still have exactly one row, regardless of which
        # worker won.
        rows = master.execute(
            "SELECT count(*) FROM semantic_layer._definitions WHERE name = 'ine_view'"
        ).fetchone()
        if rows is None or rows[0] != 1:
            print(f"FAIL: expected 1 row in _definitions, got {rows}")
            return False

        # Loser should see a PK constraint violation, not a parser error or
        # corrupt data — pin the shape so a future regression doesn't slip
        # through.
        losing_msg = str(failures[0].error or "")
        if "ine_view" not in losing_msg and "constraint" not in losing_msg.lower():
            print(
                "FAIL: losing thread error did not look like a name conflict: "
                f"{losing_msg!r}"
            )
            return False

        print("PASS: concurrent CREATE IF NOT EXISTS serialized on PK constraint")
        return True


def main() -> int:
    try:
        ok1 = test_concurrent_create_serializes()
        ok2 = test_concurrent_create_if_not_exists_serializes()
        return 0 if (ok1 and ok2) else 1
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        return 1


if __name__ == "__main__":
    sys.exit(main())
