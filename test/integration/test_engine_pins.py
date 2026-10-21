#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
The wheel pin and the engine pin must name the same DuckDB build.

`scripts/ensure_engine.py` builds the engine from the sdist of the wheel that
`.duckdb-pypi-version` names, and stamps it with `.duckdb-version`. The engine
source carries no label of its own, so the build cannot tell whether that wheel
really is the release `.duckdb-version` names. If it is not (a typo, a later
nightly), the extension would claim one engine's version while laying out its
C++ objects for another's, and the load guard, which compares version strings,
would let it into the host it does not fit.

This test asks the wheel itself. It runs under the wheel the PEP 723 header pins
(which `tests::duckdb_derived_pins_agree` keeps equal to `.duckdb-pypi-version`)
and checks that the wheel is that release, that its engine reports
`.duckdb-version`, and, while the workflows pin the engine by commit, that its
engine was built from that commit. No extension is loaded.

Exit codes: 0 = pins agree, 1 = they do not.
"""

import re
import sys
from pathlib import Path

import duckdb

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = [ROOT / ".github/workflows/BuildAll.yml", ROOT / ".github/workflows/BuildQuick.yml"]


def pin(name):
    path = ROOT / name
    return path.read_text().strip() if path.exists() else None


def main():
    engine = pin(".duckdb-version")
    wheel = pin(".duckdb-pypi-version") or engine.removeprefix("v")
    library_version, source_id = duckdb.execute("PRAGMA version").fetchone()[:2]
    failures = []

    if duckdb.__version__ != wheel:
        failures.append(f"the wheel is duckdb {duckdb.__version__}, the pin says {wheel}")
    if library_version != engine:
        failures.append(
            f"duckdb {wheel} reports engine {library_version}, but .duckdb-version is {engine}: "
            "fix .duckdb-pypi-version so it names the wheel of that engine"
        )
    for workflow in WORKFLOWS:
        m = re.search(r"^\s*duckdb_version:\s*(\S+)", workflow.read_text(), re.MULTILINE)
        commit = m.group(1) if m else ""
        if re.fullmatch(r"[0-9a-f]{7,40}", commit) and not commit.startswith(source_id):
            failures.append(
                f"{workflow.name} builds engine commit {commit}, but duckdb {wheel} was built "
                f"from {source_id}"
            )

    print(f"wheel duckdb {duckdb.__version__}: engine {library_version}, commit {source_id}")
    for failure in failures:
        print(f"FAIL: {failure}")
    if failures:
        return 1
    print("PASS: the wheel pin, the engine pin and the workflows' engine commit agree")
    return 0


if __name__ == "__main__":
    sys.exit(main())
