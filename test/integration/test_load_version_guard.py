#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# ///
"""
A DuckDB other than the one we were built against must refuse to load us.

The extension is built with the `C_STRUCT` ABI pinned to the v1.5.6 C API (see
the Makefile), so DuckDB accepts it in any host whose C API is at least that.
DuckDB 2.0 is one. Our C++ shim, though, is compiled against one exact DuckDB
release and reads host objects by layout, so running inside any other release
is undefined behaviour. The extension therefore checks the host's version at
load time and refuses with an actionable message.

This test runs under a DuckDB that differs from `.duckdb-version` (the last 1.5
release, whose C API is the one we declare, so only our own check stands
between it and the shim) and asserts that `LOAD` fails with our message and
does not crash. The LOAD runs in a child process so that a crash, which is what
the unguarded extension may do, reports as a failure with its exit status
instead of killing the test.

The host DuckDB is not pinned in this file's header, because every header pin
must name the wheel of the engine we build (`.duckdb-pypi-version` while that
engine is a pre-release, else `.duckdb-version`). Run it with `just test-load-version-guard`, which
supplies the host with `uv run --with`; when the build moves to a new DuckDB,
point that pin at a release other than the new one. The check below refuses to
run the test against the version we build for, since it would then prove
nothing.

Exit codes:
    0 = test passed
    1 = test failed
"""

import subprocess
import sys
import textwrap
from pathlib import Path

import duckdb

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_extension_path, get_project_root

EXT_PATH = get_extension_path()

CHILD = textwrap.dedent(
    """
    import sys
    import duckdb

    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    try:
        con.execute(f"LOAD '{sys.argv[1]}'")
    except duckdb.Error as exc:
        print(str(exc))
        sys.exit(0)
    print("LOADED")
    sys.exit(0)
    """
)


def test_mismatched_host_refuses_to_load():
    built = (get_project_root() / ".duckdb-version").read_text().strip()
    host = duckdb.sql("SELECT library_version FROM pragma_version()").fetchone()[0]
    assert host != built, (
        f"this test needs a host DuckDB other than {built}, the version we build "
        f"for; it is running under {host}. Run it through `just test-load-version-guard`, "
        "or update the host pin in that recipe."
    )

    child = subprocess.run(
        [sys.executable, "-c", CHILD, str(EXT_PATH)],
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert child.returncode == 0, (
        f"LOAD in DuckDB {host} crashed the process (exit {child.returncode}).\n"
        f"stdout: {child.stdout}\nstderr: {child.stderr}"
    )
    out = child.stdout.strip()
    assert out != "LOADED", (
        f"semantic_views built for DuckDB {built} loaded into DuckDB {host}; "
        "the load-time version check should have refused it"
    )
    assert f"built for DuckDB {built}" in out and f"DuckDB {host}" in out, (
        f"LOAD failed, but not with the version check's message: {out}"
    )


if __name__ == "__main__":
    try:
        test_mismatched_host_refuses_to_load()
    except AssertionError as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        sys.exit(1)
    except Exception as exc:  # noqa: BLE001 — surface anything else clearly
        import traceback

        traceback.print_exc()
        print(f"ERROR: {exc}", file=sys.stderr)
        sys.exit(1)
    print("PASSED: load_version_guard")
    sys.exit(0)
