#!/usr/bin/env python3
"""
Run every ``examples/*.py`` against the debug extension and fail on any error.

Usage (from the repository root; ``just test-examples`` does this)::

    python3 scripts/run_examples.py                 # every example
    python3 scripts/run_examples.py basic_ddl_and_query.py ...   # just these

Each example runs as ``uv run --script <file>``, so it gets exactly the
dependencies its own PEP 723 header declares -- ``transactional_ddl.py`` gets
``adbc-driver-manager`` and ``pyarrow`` that way, with nothing installed by
hand. Every example is run from the repository root, because most of them
``LOAD 'build/debug/semantic_views.duckdb_extension'`` by relative path.

Nothing is skipped silently. Every file in ``examples/`` ends in exactly one
of these states, and the run prints one line per file plus a summary:

  PASS     exited 0.
  FAIL     exited non-zero (or timed out). The run fails.
  SKIPPED  only with ``SV_OFFLINE_EXTENSIONS=1`` (local only, refused when
           ``CI`` is set), and only for the files in
           ``NEEDS_DOWNLOADED_EXTENSION``. Same opt-in as the sqllogictest
           runner's offline mode; see MAINTAINER.md.
  XFAIL    a file in ``EXPECTED_FAILURES`` failed with its recorded error.
           A different error is a FAIL.
  XPASS    a file in ``EXPECTED_FAILURES`` passed. Printed as a warning (and a
           GitHub ``::warning::`` annotation), not a failure: the entry is
           stale and should be deleted, but turning that into a red would make
           this runner depend on the merge order of the change that fixes the
           example. See the entry's own note.

A stale allow-list entry naming a file that no longer exists is a FAIL, so the
lists cannot outlive the examples they describe.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
EXAMPLES_DIR = REPO_ROOT / "examples"
EXTENSION_PATH = REPO_ROOT / "build" / "debug" / "semantic_views.duckdb_extension"
TIMEOUT_SECONDS = 600

# Examples that need a DuckDB extension downloaded from extensions.duckdb.org
# at run time. CI runs them (it has network access); a local sandbox that
# cannot reach that host may skip them, loudly, with SV_OFFLINE_EXTENSIONS=1.
NEEDS_DOWNLOADED_EXTENSION: dict[str, str] = {
    # Empty. race_guards_and_unification.py was listed here while it called
    # enable_peg_parser(), which autoloads `autocomplete`; DuckDB 2.0 removed
    # the toggle (the PEG parser is the only one) and the example with it.
}

# Examples that are known to fail, keyed by file name: (substring the failure
# output must contain, why). Every entry needs a TECH-DEBT reference.
EXPECTED_FAILURES: dict[str, tuple[str, str]] = {
    # Empty. examples/type_inference.py was listed here (TECH-DEBT #85) until
    # PR #248 rewrote it without the multi-statement execute() call.
}


def offline_mode() -> bool:
    """Return True for ``SV_OFFLINE_EXTENSIONS=1``; refuse it under CI."""
    if os.environ.get("SV_OFFLINE_EXTENSIONS") != "1":
        return False
    if os.environ.get("CI"):
        print(
            "ERROR: SV_OFFLINE_EXTENSIONS=1 skips examples and is for local offline "
            f"runs only; refusing because CI={os.environ['CI']!r}",
            file=sys.stderr,
        )
        sys.exit(1)
    return True


def run_example(path: Path) -> tuple[int, str]:
    """Run one example; return (exit code, combined output). Timeout = 124."""
    try:
        proc = subprocess.run(
            ["uv", "run", "--quiet", "--script", str(path)],
            cwd=REPO_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired as e:
        out = e.stdout if isinstance(e.stdout, str) else (e.stdout or b"").decode(errors="replace")
        return 124, f"{out}\nTIMED OUT after {TIMEOUT_SECONDS}s"
    return proc.returncode, proc.stdout


def indent(text: str) -> str:
    return "\n".join("    | " + line for line in text.rstrip().splitlines())


def main() -> int:
    offline = offline_mode()

    if shutil.which("uv") is None:
        print(
            "ERROR: `uv` not found on PATH -- the examples run under `uv run --script` "
            "so each gets its own PEP 723 dependencies (install: pip install uv).",
            file=sys.stderr,
        )
        return 1

    if not EXTENSION_PATH.is_file():
        print(
            f"ERROR: {EXTENSION_PATH.relative_to(REPO_ROOT)} not found -- build it first "
            "(`just build`, or `just test-examples`, which builds).",
            file=sys.stderr,
        )
        return 1

    examples = sorted(EXAMPLES_DIR.glob("*.py"))
    if not examples:
        print(f"ERROR: no examples found under {EXAMPLES_DIR}", file=sys.stderr)
        return 1

    names = {p.name for p in examples}
    wanted = [Path(a).name for a in sys.argv[1:]]
    unknown = sorted(set(wanted) - names)
    if unknown:
        print(f"ERROR: no such example(s): {', '.join(unknown)}", file=sys.stderr)
        return 1
    stale = sorted((set(NEEDS_DOWNLOADED_EXTENSION) | set(EXPECTED_FAILURES)) - names)
    if stale:
        for name in stale:
            print(
                f"ERROR: allow-list entry for examples/{name}, which does not exist",
                file=sys.stderr,
            )
        return 1

    counts = {"PASS": 0, "FAIL": 0, "SKIPPED": 0, "XFAIL": 0, "XPASS": 0}
    failures: list[str] = []

    selected = [p for p in examples if not wanted or p.name in wanted]
    for path in selected:
        name = path.name
        rel = f"examples/{name}"

        if offline and name in NEEDS_DOWNLOADED_EXTENSION:
            counts["SKIPPED"] += 1
            print(
                f"SKIPPED (SV_OFFLINE_EXTENSIONS=1): {rel} -- "
                f"{NEEDS_DOWNLOADED_EXTENSION[name]} from extensions.duckdb.org; NOT run"
            )
            continue

        started = time.monotonic()
        rc, output = run_example(path)
        elapsed = f"{time.monotonic() - started:.1f}s"

        if name in EXPECTED_FAILURES:
            needle, why = EXPECTED_FAILURES[name]
            if rc == 0:
                counts["XPASS"] += 1
                msg = f"{rel} passed but is listed in EXPECTED_FAILURES -- delete its entry ({why})"
                print(f"XPASS    {rel} ({elapsed}) -- WARNING: {msg}")
                if os.environ.get("GITHUB_ACTIONS"):
                    print(f"::warning file=scripts/run_examples.py::{msg}")
            elif needle in output:
                counts["XFAIL"] += 1
                print(f"XFAIL    {rel} ({elapsed}) -- expected failure: {why}")
            else:
                counts["FAIL"] += 1
                failures.append(rel)
                print(
                    f"FAIL     {rel} ({elapsed}) -- exit {rc}, and NOT with its expected "
                    f"error {needle!r}:\n{indent(output)}"
                )
            continue

        if rc == 0:
            counts["PASS"] += 1
            print(f"PASS     {rel} ({elapsed})")
        else:
            counts["FAIL"] += 1
            failures.append(rel)
            print(f"FAIL     {rel} ({elapsed}) -- exit {rc}:\n{indent(output)}")

    summary = ", ".join(f"{v} {k}" for k, v in counts.items() if v)
    print(f"\n{len(selected)} examples: {summary}")
    if failures:
        print("Failed: " + ", ".join(failures), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
