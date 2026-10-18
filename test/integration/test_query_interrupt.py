#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
Regression test: `duckdb_interrupt()` must actually stop a `semantic_view()` query.

`semantic_view()` used to run its expanded SQL eagerly inside `init_global`
(`cpp/src/shim.cpp`), on a *fresh* `Connection(*context.db)` — and DuckDB's
interrupt flag is per-`ClientContext`. `con.interrupt()` (and therefore ADBC's
`adbc_cancel()`, the CLI's Ctrl-C, and any framework timeout built on them) sets
the flag on the *caller's* context; the inner query polled its own, which
nothing ever set. The inner query therefore ran to full completion and the
interrupt only surfaced afterwards, when the outer pipeline resumed.

Since TECH-DEBT #42's `bind_replace` change there is no inner query:
`semantic_view()`'s bind hands DuckDB the expanded SQL as a subquery, and DuckDB
runs it as part of the caller's query, so a cancel stops it as it stops any
query. This file keeps guarding that it does.

Two properties are asserted, because the pre-fix build satisfies one of them:

  1. TIMING — the query must abort shortly after the interrupt fires, not after
     the inner aggregate completes. This is the property that was broken; the
     pre-fix build reports INTERRUPT only after running the full query.
  2. ERROR SHAPE — the error must be a bare `Interrupted!`, NOT wrapped in
     `semantic_view: SQL execution failed: ...`. ADBC maps to
     ADBC_STATUS_CANCELLED on an exact string match against
     `InterruptException::INTERRUPT_MESSAGE`; a wrapped message would surface a
     cancelled query as ADBC_STATUS_INTERNAL.

DuckDB 2.0 adds a second way to cancel a query: `SET max_execution_time`, a
per-query deadline the engine enforces apart from the interrupt flag. It is the
caller's setting, so the probe connections never had it, and `semantic_view()`
and the metadata binds below ran to completion past it. Every check here runs
once per cancellation (CANCELLATIONS), and a deadline must surface as the
engine's own "Query exceeded maximum execution time", bare, the same way.

A plain-SQL control runs first through the identical harness. It aborts early on
*every* build (DuckDB's own executor polls the flag correctly), so if the control
fails, the harness is wrong rather than the feature — the failure message says so.

The workload is self-calibrating: the dimension expression is a chain of `md5()`
calls whose depth doubles until one uninterrupted `semantic_view()` run costs at
least TARGET_BASELINE_S. Cost is scaled through the *expression* rather than the
row count so the fact table stays small and the grouped result stays 256 rows —
what is being timed is the inner aggregate, not result materialisation.

The same holds for the metadata commands. Since TECH-DEBT #51, SHOW SEMANTIC
DIMENSIONS / METRICS / FACTS, SHOW COLUMNS, SHOW SEMANTIC DIMENSIONS FOR METRIC
and DESCRIBE SEMANTIC VIEW probe each view's member types at bind time, on the
bind's own `Connection(*context.db)`. Pre-fix, a cancel neither reached a probe
already running nor stopped the remaining ones, so a cross-view SHOW typed
every view and only then reported INTERRUPT. The second half of this file
asserts the same two properties for those binds, against a source whose BIND
is slow: a `read_csv` over a list of globs, each of which scans the directory
(and polls the probe connection's interrupt flag) when expanded.

Usage:
    uv run test/integration/test_query_interrupt.py

Exit codes:
    0 = all scenarios passed
    1 = at least one scenario failed
"""

from __future__ import annotations

import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

ROWS = 2_000_000

# Calibration: keep doubling the md5-chain depth until one uninterrupted
# semantic_view() run costs at least TARGET_BASELINE_S. MAX_DEPTH caps runtime.
START_DEPTH = 8
MAX_DEPTH = 128
TARGET_BASELINE_S = 3.0

# Below this the gap between "aborted early" and "ran to completion" is too
# small to distinguish from scheduling noise, and the test would pass
# vacuously against a pre-fix build. Hard failure, not a silent skip.
MIN_BASELINE_S = 1.5

# When the interrupt fires, relative to the start of the query.
FIRE_AT_S = 0.4

# An interrupted run must finish within this fraction of the uninterrupted one.
# A regressed build reports only after the inner query completes, landing at
# ~1.0; a working build stops as soon as the in-flight task ends. Placed to
# leave room for a task that takes as long as the whole abort has been observed
# to take under load, while still failing anything that ran to completion.
ABORT_FRACTION = 0.6

# Floor under the fraction above: never require an abort sooner than the fire
# delay plus one `ExecuteTask()` chunk, however short the calibrated baseline.
MIN_ABORT_SLACK_S = 1.5

# The abort limit must stay this far below the uninterrupted baseline, or the
# assertion cannot tell a working build from one that ran to completion. Checked
# against the measured baseline, so a machine where the floor dominates fails
# loudly instead of passing vacuously.
VACUITY_MARGIN = 0.75


def make_connection():
    """Create an in-memory DuckDB connection with the extension loaded."""
    import duckdb

    con = duckdb.connect(
        ":memory:",
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": get_ext_dir(),
        },
    )
    con.execute(f"FORCE INSTALL '{get_extension_path()}'")
    con.execute("LOAD semantic_views")
    # The progress bar kicks in on queries past ~2s and would spray redraws
    # through the test output.
    con.execute("SET enable_progress_bar = false")
    return con


def bucket_expr(depth):
    """A CPU-heavy but low-cardinality grouping key: substr(md5(md5(...)), 1, 2)."""
    expr = "CAST(f.id AS VARCHAR)"
    for _ in range(depth):
        expr = f"md5({expr})"
    return f"substr({expr}, 1, 2)"


SV_SQL = """
    SELECT count(*) FROM semantic_view(
        'iq_view',
        dimensions := ['f.bucket'],
        metrics := ['f.total']
    )
"""


def plain_sql(depth):
    """Hand-written twin of what the semantic view expands to.

    Used as the control: DuckDB's own executor polls the interrupt flag, so this
    aborts early on every build, pre-fix and post-fix alike.
    """
    return f"""
        SELECT count(*) FROM (
            SELECT {bucket_expr(depth)} AS bucket, sum(f.amount) AS total
            FROM iq_facts f
            GROUP BY 1
        )
    """


def build_table(con):
    con.execute(
        f"""
        CREATE OR REPLACE TABLE iq_facts AS
        SELECT i AS id, (i % 1000)::DOUBLE AS amount
        FROM range({ROWS}) t(i)
        """
    )


def build_view(con, depth):
    con.execute("DROP SEMANTIC VIEW IF EXISTS iq_view")
    con.execute(
        f"""
        CREATE SEMANTIC VIEW iq_view AS
          TABLES (f AS iq_facts PRIMARY KEY (id))
          DIMENSIONS (f.bucket AS {bucket_expr(depth)})
          METRICS (f.total AS sum(f.amount))
        """
    )


def time_query(con, sql):
    """Run `sql` to completion, returning wall-clock seconds."""
    start = time.perf_counter()
    con.execute(sql).fetchall()
    return time.perf_counter() - start


def run_interrupted(con, sql, fire_at=FIRE_AT_S):
    """
    Run `sql`, firing con.interrupt() from another thread `fire_at` seconds in.

    Returns (elapsed_seconds, exception_or_None).
    """
    timer = threading.Timer(fire_at, con.interrupt)
    timer.start()
    start = time.perf_counter()
    try:
        con.execute(sql).fetchall()
        err = None
    except Exception as exc:  # noqa: BLE001 - the exception IS the observation
        err = exc
    elapsed = time.perf_counter() - start
    timer.cancel()
    # If the query somehow beat the timer, the flag may still be pending; a
    # throwaway statement clears it (DuckDB resets `interrupted` per query).
    try:
        con.execute("SELECT 1").fetchall()
    except Exception:  # noqa: BLE001 - swallowing a stale pending interrupt
        con.execute("SELECT 1").fetchall()
    return elapsed, err


# DuckDB 2.0's per-query deadline (`SET max_execution_time`, in milliseconds)
# stops a query the same way an interrupt does, from inside the engine, with
# its own message. The caller's deadline is the caller's ClientContext state,
# so it has to be carried to the probe connections exactly like the flag.
DEADLINE_MESSAGE = "Query exceeded maximum execution time"


def run_deadlined(con, sql, deadline_s=FIRE_AT_S):
    """
    Run `sql` under `SET max_execution_time` of `deadline_s` seconds.

    Returns (elapsed_seconds, exception_or_None), like run_interrupted().
    """
    con.execute(f"SET max_execution_time = {int(deadline_s * 1000)}")
    start = time.perf_counter()
    try:
        con.execute(sql).fetchall()
        err = None
    except Exception as exc:  # noqa: BLE001 - the exception IS the observation
        err = exc
    elapsed = time.perf_counter() - start
    con.execute("RESET max_execution_time")
    return elapsed, err


# How each kind of cancellation is fired, and the message it must surface as.
CANCELLATIONS = (
    ("interrupt", run_interrupted, "Interrupted!"),
    ("deadline", run_deadlined, DEADLINE_MESSAGE),
)


def calibrate(con):
    """Grow the md5 chain until one uninterrupted semantic_view() run is slow enough."""
    depth = START_DEPTH
    while True:
        build_view(con, depth)
        baseline = time_query(con, SV_SQL)
        print(f"  calibration: md5 depth {depth:>4} -> semantic_view() baseline {baseline:.2f}s")
        if baseline >= TARGET_BASELINE_S or depth >= MAX_DEPTH:
            return depth, baseline
        depth *= 2


def check(label, passed, detail):
    print(f"  [{'PASS' if passed else 'FAIL'}] {label}: {detail}")
    return passed


# ---------------------------------------------------------------------------
# Metadata binds (SHOW / DESCRIBE member-type probes, TECH-DEBT #51)
# ---------------------------------------------------------------------------

# Calibration for the slow-bind source: double the glob count until one
# single-probe SHOW costs at least TARGET_BASELINE_S.
START_GLOBS = 250
MAX_GLOBS = 8000


def build_slow_source(con, view, csv_dir, n):
    """A view whose every bind expands `n` globs, one directory scan each."""
    globs = ", ".join(f"'{csv_dir}/f{i}.cs?'" for i in range(n))
    con.execute(
        f"CREATE OR REPLACE VIEW {view} AS SELECT * FROM read_csv([{globs}], union_by_name = true)"
    )


def write_csvs(csv_dir, n):
    for i in range(n):
        path = csv_dir / f"f{i}.csv"
        if not path.exists():
            path.write_text("id,x\n1,2\n")


def create_probe_view(con, name, source):
    con.execute(f"DROP SEMANTIC VIEW IF EXISTS {name}")
    con.execute(
        f"""
        CREATE SEMANTIC VIEW {name} AS
          TABLES (s AS {source} PRIMARY KEY (id))
          DIMENSIONS (s.x AS s.x)
          METRICS (s.total AS sum(s.x))
        """
    )


def run_metadata_probe_tests(con) -> bool:
    import shutil
    import tempfile

    csv_dir = Path(tempfile.mkdtemp(prefix="sv_interrupt_"))
    try:
        return _run_metadata_probe_tests(con, csv_dir)
    finally:
        shutil.rmtree(csv_dir, ignore_errors=True)


def _run_metadata_probe_tests(con, csv_dir) -> bool:
    ok = True

    print("\nCalibrating the slow-bind source for the metadata probes...")
    n = START_GLOBS
    one_sql = "SHOW SEMANTIC DIMENSIONS IN mp_one"
    while True:
        write_csvs(csv_dir, n)
        build_slow_source(con, "mp_src", csv_dir, n)
        create_probe_view(con, "mp_one", "mp_src")
        baseline = time_query(con, one_sql)
        print(f"  calibration: {n:>5} globs -> one-probe SHOW baseline {baseline:.2f}s")
        if baseline >= TARGET_BASELINE_S or n >= MAX_GLOBS:
            break
        n *= 2

    # One statement per bind helper the probes run under: the single-name SHOW,
    # DESCRIBE, and the two-name DIMENSIONS FOR METRIC (a separate C++ helper),
    # plus SHOW COLUMNS, which the docstring lists as probing too.
    single = [
        ("SHOW SEMANTIC DIMENSIONS IN <view>", one_sql),
        ("DESCRIBE SEMANTIC VIEW <view>", "DESCRIBE SEMANTIC VIEW mp_one"),
        ("SHOW COLUMNS IN SEMANTIC VIEW <view>", "SHOW COLUMNS IN SEMANTIC VIEW mp_one"),
        (
            "SHOW SEMANTIC DIMENSIONS IN <view> FOR METRIC",
            "SHOW SEMANTIC DIMENSIONS IN mp_one FOR METRIC total",
        ),
    ]
    for label, sql in single:
        ok &= check_interrupted_statement(con, label, sql)

    # semantic_view()'s own bind runs on a probe connection too: the Rust bind
    # types the requested members, and a DECIMAL or LIST output adds a LIMIT 0
    # probe of the expanded query. Both read the slow source.
    con.execute("DROP SEMANTIC VIEW IF EXISTS mp_dec")
    con.execute(
        """
        CREATE SEMANTIC VIEW mp_dec AS
          TABLES (s AS mp_src PRIMARY KEY (id))
          DIMENSIONS (s.x AS s.x)
          METRICS (s.dtotal AS sum(s.x::DECIMAL(18, 2)))
        """
    )
    binds = [
        (
            "semantic_view(<view>) bind",
            "SELECT * FROM semantic_view('mp_one', metrics := ['total'])",
        ),
        (
            "semantic_view(<view>) bind with a DECIMAL output",
            "SELECT * FROM semantic_view('mp_dec', metrics := ['dtotal'])",
        ),
        # explain_semantic_view() runs EXPLAIN of the expanded query on its
        # probe, which binds the slow source the same way.
        (
            "explain_semantic_view(<view>) bind",
            "SELECT * FROM explain_semantic_view('mp_one', metrics := ['total'])",
        ),
    ]
    for label, sql in binds:
        ok &= check_interrupted_statement(con, label, sql)
    con.execute("DROP SEMANTIC VIEW mp_dec")

    # Cross-view: many views, each probe a fraction of the whole. The fix must
    # stop between views, not type all of them first.
    con.execute("DROP SEMANTIC VIEW mp_one")
    per_probe_target = 0.15
    # Each glob scans the whole directory (n files), so cost is linear in it.
    small = max(10, int(n * per_probe_target / max(baseline, 0.01)))
    build_slow_source(con, "mp_small", csv_dir, small)
    create_probe_view(con, "mp_v0", "mp_small")
    per_view = time_query(con, "SHOW SEMANTIC METRICS IN mp_v0")
    views = min(200, max(8, int(2 * TARGET_BASELINE_S / max(per_view, 0.005)) + 1))
    for i in range(1, views):
        create_probe_view(con, f"mp_v{i}", "mp_small")
    print(f"  cross-view: {views} views over {small} globs, ~{per_view:.2f}s per view")
    ok &= check_interrupted_statement(
        con, "cross-view SHOW SEMANTIC METRICS", "SHOW SEMANTIC METRICS"
    )

    for i in range(views):
        con.execute(f"DROP SEMANTIC VIEW mp_v{i}")
    return ok


def check_interrupted_statement(con, label, sql) -> bool:
    """Time `sql` uncancelled, then assert each cancellation stops it early and bare."""
    baseline = time_query(con, sql)
    limit = max(FIRE_AT_S + MIN_ABORT_SLACK_S, ABORT_FRACTION * baseline)
    print(f"\n{label}: cancellation must stop the member-type probes")
    ok = check(
        "the abort limit still discriminates a regressed build",
        limit <= VACUITY_MARGIN * baseline,
        f"limit {limit:.2f}s <= {VACUITY_MARGIN:.0%} of baseline {baseline:.2f}s",
    )
    for kind, run, expected in CANCELLATIONS:
        elapsed, err = run(con, sql)
        message = "" if err is None else str(err)
        ok &= check(
            f"{kind}: the statement raises rather than completing",
            err is not None,
            f"error={type(err).__name__ if err else 'None'} after {elapsed:.2f}s",
        )
        ok &= check(
            f"{kind}: the statement stops early instead of probing to completion",
            err is not None and elapsed < limit,
            f"stopped after {elapsed:.2f}s (baseline {baseline:.2f}s, limit {limit:.2f}s)",
        )
        # Bare means an INTERRUPT carrying the engine's own message, not wrapped
        # in the "<function> failed: ..." BinderException every other bind
        # failure gets. Raised from a bind, it carries the call's query
        # location, as DuckDB's own table functions' bind-time interrupts do.
        ok &= check(
            f"{kind}: the error is a bare interrupt",
            message.startswith(f"INTERRUPT Error: {expected}") and "failed:" not in message,
            f"message={message!r}",
        )
    return ok


def check_generous_deadline(con) -> bool:
    """
    A deadline that has not passed must not cancel anything.

    Every other deadline check expects a failure, and an instant one passes
    them. If the forwarded deadline were read on the wrong clock or epoch, any
    limit at all would cancel every probing bind at once; this control would
    then fail where the others pass.
    """
    print("\nControl (deadline): a deadline far in the future cancels nothing")
    con.execute(
        "CREATE OR REPLACE TABLE gd_src AS SELECT range AS id, range % 7 AS x FROM range(100)"
    )
    con.execute("DROP SEMANTIC VIEW IF EXISTS gd_view")
    con.execute(
        """
        CREATE SEMANTIC VIEW gd_view AS
          TABLES (s AS gd_src PRIMARY KEY (id))
          DIMENSIONS (s.x AS s.x)
          METRICS (s.total AS sum(s.x), s.dtotal AS sum(s.x::DECIMAL(18, 2)))
        """
    )
    statements = [
        (
            "semantic_view() with a DECIMAL output",
            "SELECT * FROM semantic_view('gd_view', dimensions := ['x'], metrics := ['dtotal'])",
        ),
        ("SHOW SEMANTIC DIMENSIONS IN <view>", "SHOW SEMANTIC DIMENSIONS IN gd_view"),
        (
            "explain_semantic_view()",
            "SELECT * FROM explain_semantic_view('gd_view', metrics := ['total'])",
        ),
    ]
    ok = True
    con.execute("SET max_execution_time = 600000")
    try:
        for label, sql in statements:
            try:
                rows = con.execute(sql).fetchall()
                err = None
            except Exception as exc:  # noqa: BLE001 - the exception IS the observation
                rows, err = [], exc
            ok &= check(
                f"{label} returns rows under a 10-minute deadline",
                err is None and len(rows) > 0,
                f"rows={len(rows)} error={err!r}",
            )
    finally:
        con.execute("RESET max_execution_time")
        con.execute("DROP SEMANTIC VIEW gd_view")
        con.execute("DROP TABLE gd_src")
    return ok


def run_tests() -> int:
    ext_path = get_extension_path()
    if not ext_path.exists():
        print(f"ERROR: extension not found at {ext_path}")
        print("Run `just build` first.")
        return 1

    con = make_connection()
    build_table(con)

    print(f"Calibrating workload ({ROWS:,} rows)...")
    depth, sv_baseline = calibrate(con)
    control_sql = plain_sql(depth)
    plain_baseline = time_query(con, control_sql)
    print(f"  plain-SQL twin baseline: {plain_baseline:.2f}s")

    # An abort is "early" if it lands well before the full run would have.
    #
    # The tolerance is a fraction of the UNINTERRUPTED run, not `fire delay +
    # fraction of it`. The regression this file guards makes the query run to
    # completion, so the two outcomes to separate are "stopped partway" and
    # "stopped at ~baseline" -- a fraction of the baseline sits between them by
    # construction, whatever the machine.
    #
    # The earlier formula (`FIRE_AT_S + max(1.0, 0.30 * baseline)`) budgeted for
    # the abort *delay* instead, and that delay does not scale with the
    # baseline: it is bounded by how long one `ExecuteTask()` chunk takes, which
    # is a property of the md5 depth, ~1.1s here and roughly constant across
    # baselines from 3.7s to 4.8s. Under load it stretched to 1.8s against a
    # 1.43s allowance and failed a build that had aborted at 46% of baseline --
    # a false positive, not a caught regression. See TECH-DEBT (v0.12.0).
    #
    # The floor keeps the limit off the fire delay plus one task on a fast
    # machine, where a small baseline would otherwise demand an impossibly
    # prompt abort.
    def deadline(baseline):
        return max(FIRE_AT_S + MIN_ABORT_SLACK_S, ABORT_FRACTION * baseline)

    ok = True

    print("\nPreconditions")
    ok &= check(
        "workload is slow enough to be conclusive",
        sv_baseline >= MIN_BASELINE_S,
        f"semantic_view() baseline {sv_baseline:.2f}s >= {MIN_BASELINE_S}s "
        f"(at md5 depth {depth}; raise MAX_DEPTH if this fails on a fast machine)",
    )
    if sv_baseline < MIN_BASELINE_S:
        # Everything below would be measuring noise.
        return 1

    # The abort limit is only meaningful if a build that never observed the
    # interrupt would breach it. Assert that against the MEASURED uninterrupted
    # run rather than trusting the constants: if calibration lands somewhere the
    # floor dominates, the limit can creep up to the baseline itself and every
    # assertion below starts passing vacuously -- including against the pre-fix
    # build this file exists to catch. Loud failure, not a silent green.
    sv_limit = deadline(sv_baseline)
    ok &= check(
        "the abort limit still discriminates a regressed build",
        sv_limit <= VACUITY_MARGIN * sv_baseline,
        f"limit {sv_limit:.2f}s <= {VACUITY_MARGIN:.0%} of baseline "
        f"{sv_baseline:.2f}s (a build that ran to completion would report "
        f"~{sv_baseline:.2f}s and fail, as intended)",
    )
    if sv_limit > VACUITY_MARGIN * sv_baseline:
        print(
            "  NOTE: the calibrated workload is too short for the abort floor "
            f"({FIRE_AT_S}s + {MIN_ABORT_SLACK_S}s). Raise TARGET_BASELINE_S so "
            "calibration picks a deeper md5 chain."
        )
        return 1

    for kind, run, expected in CANCELLATIONS:
        print(f"\nControl ({kind}): plain SQL through the identical harness")
        elapsed, err = run(con, control_sql)
        limit = deadline(plain_baseline)
        control_ok = check(
            "plain SQL aborts early",
            err is not None and elapsed < limit and expected in str(err),
            f"stopped after {elapsed:.2f}s (baseline {plain_baseline:.2f}s, limit {limit:.2f}s), "
            f"error={err!r}",
        )
        if not control_ok:
            print(
                f"  NOTE: the control failing means the HARNESS is broken (the {kind} "
                "never reached DuckDB at all), not that semantic_view() regressed."
            )
        ok &= control_ok

        print(f"\nsemantic_view() ({kind}): cancellation must reach the inner query")
        elapsed, err = run(con, SV_SQL)
        limit = deadline(sv_baseline)
        message = "" if err is None else str(err)

        ok &= check(
            "the query raises rather than completing",
            err is not None,
            f"error={type(err).__name__ if err else 'None'} after {elapsed:.2f}s",
        )
        ok &= check(
            "the query stops early instead of running to completion",
            err is not None and elapsed < limit,
            f"stopped after {elapsed:.2f}s (baseline {sv_baseline:.2f}s, limit {limit:.2f}s)",
        )
        ok &= check(
            f"the error is a bare interrupt ({expected!r})",
            expected in message,
            f"message={message!r}",
        )
        ok &= check(
            "the interrupt is not wrapped as an execution failure",
            "SQL execution failed" not in message,
            f"message={message!r}",
        )

    ok &= check_generous_deadline(con)

    ok &= run_metadata_probe_tests(con)

    print("\nThe connection is still usable after the cancelled query")
    ok &= check(
        "follow-up query succeeds",
        con.execute("SELECT 42").fetchall() == [(42,)],
        "SELECT 42 returned 42",
    )

    print()
    print("ALL TESTS PASSED" if ok else "SOME TESTS FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(run_tests())
