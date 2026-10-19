#!/usr/bin/env python3
"""
Guards for the sqllogictest run behind `just test-sql` / `make test_debug`.

Run with the configure/venv interpreter (the runner lives there), from the
repository root, after `just build`:

    make patch-runner
    configure/venv/bin/python test/integration/test_sqllogictest_guards.py

(`just test-sqllogictest-guards` does exactly that.)

The bare duckdb_sqllogictest runner exits 0 in every scenario below except the
controls. Each scenario drives the REAL entry point -- `make test_debug` with a
one-file TEST_LIST -- so what is tested is the Makefile wiring CI uses, not the
wrapper script in isolation:

  missing_extension    the extension path does not exist; `require
                       semantic_views` is SKIPPED in every file.
  renamed_extension    the extension exists under another file name, so the
                       runner registers it under the wrong name and `require
                       semantic_views` is SKIPPED. (The case that motivated
                       this: a renamed build output silently skipped all 115
                       files and printed "115 test files passed".)
  zero_statements      `require semantic_views` succeeds, then `mode skip`
                       swallows every statement: the file reports SUCCESS
                       having executed nothing.
  unparseable_file     the runner cannot parse the file, logs it to its skip
                       log and moves on -- no SKIPPED line, no failure.
  halted_file          `halt` ends the file early; reported as SKIPPED.
  network_error_http   a `query` fails with an error containing "HTTP" after an
                       earlier statement ran. The runner's
                       `ignore_error_messages` ({"HTTP", "Unable to connect"})
                       treats that as a pass: the query's result is never
                       checked and the file reports SUCCESS.
  network_error_unable_to_connect  the same, for "Unable to connect".
  offline_not_excused  under SV_OFFLINE_EXTENSIONS=1 a network error is still a
                       failure for a file NOT on the offline list.
  offline_refused_in_ci  SV_OFFLINE_EXTENSIONS=1 with CI set is refused.
  internal_error_matched  a `statement error` block whose expected substring
                       matches, but the error is an INTERNAL one. DuckDB keeps
                       the original message inside the INTERNAL wrapper (for
                       example a scalar function that throws without being
                       marked fallible), so the substring check alone passes a
                       crash path. Simulated with error(), as the network cases
                       are.

  control_pass         a normal file: must still exit 0.
  control_expected_error  the same expected-error block on an ordinary error:
                       must still exit 0.
  control_platform     `require windows` on a non-Windows host: a legitimate
                       platform skip, must still exit 0 (BuildAll runs the
                       suite on Windows and macOS, where such skips are real).
  control_offline_excused  under SV_OFFLINE_EXTENSIONS=1 (CI unset), a file
                       passed as needing a downloaded extension
                       (`--offline-excused`) may hit a network error: it must
                       exit 0 AND say SKIPPED loudly.

The four network/offline scenarios after the first two drive the wrapper
script directly rather than through make, because they need the offline
variable set and CI unset, which the CI job running this file cannot provide
through make.

Each guard scenario must exit non-zero AND print its own reason (so an
unrelated failure cannot pass for the guard working); each control must exit 0.
Each scenario runs and reports independently, so one red does not hide the
others; the script exits 1 if any scenario did not behave as expected.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
REAL_EXTENSION = REPO_ROOT / "build" / "debug" / "semantic_views.duckdb_extension"

GOOD_TEST = """\
# name: {name}
require semantic_views

statement ok
CREATE TABLE t (id INTEGER, x INTEGER)

query I
SELECT 1
----
1
"""

ZERO_STATEMENTS_TEST = """\
# name: {name}
require semantic_views

mode skip

statement ok
SELECT 1

query I
SELECT 1
----
1
"""

UNPARSEABLE_TEST = """\
# name: {name}
require semantic_views

query I
SELECT 1
----
1

frobnicate the parser
"""

HALTED_TEST = """\
# name: {name}
require semantic_views

statement ok
SELECT 1

halt

query I
SELECT 1
----
1
"""

NETWORK_ERROR_TEST = """\
# name: {name}
require semantic_views

statement ok
CREATE TABLE t (id INTEGER)

query I
SELECT error('{message}: simulated by test_sqllogictest_guards.py')
----
1
"""

EXPECTED_ERROR_TEST = """\
# name: {name}
require semantic_views

statement error
SELECT error('{message}: simulated by test_sqllogictest_guards.py')
----
simulated by test_sqllogictest_guards.py
"""

PLATFORM_SKIP_TEST = """\
# name: {name}
require semantic_views

require windows

statement ok
SELECT 1
"""


def run_make(
    workdir: Path, test_body: str, extension_name: str = "semantic_views"
) -> subprocess.CompletedProcess:
    test_file = workdir / "case.test"
    test_file.write_text(test_body.format(name=test_file))
    test_list = workdir / "TEST_LIST"
    test_list.write_text(f"{test_file}\n")
    env = dict(os.environ)
    # A guard test must not inherit the offline opt-in: it is about what CI sees.
    env.pop("SV_OFFLINE_EXTENSIONS", None)
    return subprocess.run(
        [
            "make",
            "--no-print-directory",
            "test_debug",
            f"EXTENSION_NAME={extension_name}",
            f"TEST_LIST_PATH={test_list}",
        ],
        cwd=REPO_ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )


def run_wrapper(
    workdir: Path, test_body: str, env_overrides: dict, extra_args: tuple = ()
) -> subprocess.CompletedProcess:
    """Run scripts/run_sqllogictest.py directly, as the Makefile's TEST_RUNNER does."""
    test_file = workdir / "case.test"
    test_file.write_text(test_body.format(name=test_file))
    test_list = workdir / "TEST_LIST"
    test_list.write_text(f"{test_file}\n")
    env = dict(os.environ)
    for key in ("SV_OFFLINE_EXTENSIONS", "CI"):
        env.pop(key, None)
    env.update(env_overrides)
    args = [arg.format(test_file=test_file) for arg in extra_args]
    return subprocess.run(
        [
            sys.executable,
            "scripts/run_sqllogictest.py",
            "--required-extension",
            "semantic_views",
            "--test-dir",
            "test/sql",
            "--file-list",
            str(test_list),
            "--external-extension",
            str(REAL_EXTENSION),
            *args,
        ],
        cwd=REPO_ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )


def case_missing_extension(workdir: Path):
    return run_make(
        workdir, GOOD_TEST, extension_name="semantic_views_does_not_exist"
    ), "require semantic_views: FAILED"


def case_renamed_extension(workdir: Path):
    alias = REPO_ROOT / "build" / "debug" / "sv_guard_renamed.duckdb_extension"
    if alias.exists() or alias.is_symlink():
        alias.unlink()
    alias.symlink_to(REAL_EXTENSION)
    try:
        return run_make(
            workdir, GOOD_TEST, extension_name="sv_guard_renamed"
        ), "require semantic_views: FAILED"
    finally:
        alias.unlink()


def case_zero_statements(workdir: Path):
    return run_make(workdir, ZERO_STATEMENTS_TEST), "executed 0 statements/queries"


def case_unparseable_file(workdir: Path):
    return run_make(workdir, UNPARSEABLE_TEST), "never ran"


def case_halted_file(workdir: Path):
    return run_make(workdir, HALTED_TEST), "HALT was encountered"


def case_network_error_http(workdir: Path):
    body = NETWORK_ERROR_TEST.replace("{message}", "HTTP Error 503")
    return run_make(workdir, body), "ignored network error"


def case_network_error_unable_to_connect(workdir: Path):
    body = NETWORK_ERROR_TEST.replace("{message}", "Unable to connect")
    return run_make(workdir, body), "ignored network error"


def case_offline_not_excused(workdir: Path):
    body = NETWORK_ERROR_TEST.replace("{message}", "HTTP Error 503")
    return run_wrapper(workdir, body, {"SV_OFFLINE_EXTENSIONS": "1"}), "ignored network error"


def case_offline_refused_in_ci(workdir: Path):
    return run_wrapper(
        workdir, GOOD_TEST, {"SV_OFFLINE_EXTENSIONS": "1", "CI": "true"}
    ), "refusing because CI='true'"


def case_internal_error_matched(workdir: Path):
    body = EXPECTED_ERROR_TEST.replace("{message}", "INTERNAL Error: Scalar function threw")
    return run_make(workdir, body), "raised an INTERNAL error"


def case_control_expected_error(workdir: Path):
    body = EXPECTED_ERROR_TEST.replace("{message}", "Invalid Input Error")
    return run_make(workdir, body), None


def case_control_offline_excused(workdir: Path):
    body = NETWORK_ERROR_TEST.replace("{message}", "HTTP Error 503")
    proc = run_wrapper(
        workdir,
        body,
        {"SV_OFFLINE_EXTENSIONS": "1"},
        ("--offline-excused", "{test_file}"),
    )
    return proc, "SKIPPED (SV_OFFLINE_EXTENSIONS=1)"


def case_control_pass(workdir: Path):
    return run_make(workdir, GOOD_TEST), None


def case_control_platform(workdir: Path):
    return run_make(workdir, PLATFORM_SKIP_TEST), None


CASES = [
    case_missing_extension,
    case_renamed_extension,
    case_zero_statements,
    case_unparseable_file,
    case_halted_file,
    case_network_error_http,
    case_network_error_unable_to_connect,
    case_offline_not_excused,
    case_offline_refused_in_ci,
    case_internal_error_matched,
    case_control_pass,
    case_control_expected_error,
    case_control_platform,
    case_control_offline_excused,
]


def main() -> int:
    if not REAL_EXTENSION.is_file():
        print(f"ERROR: {REAL_EXTENSION} not found -- run `just build` first", file=sys.stderr)
        return 1

    failed = []
    for case in CASES:
        name = case.__name__.removeprefix("case_")
        workdir = Path(tempfile.mkdtemp(prefix=f"sv_guard_{name}_"))
        try:
            proc, marker = case(workdir)
        finally:
            shutil.rmtree(workdir, ignore_errors=True)
        # A control (case_control_*) must exit 0, and print its marker when it
        # has one. A guard case must exit non-zero AND say why: a non-zero exit
        # for some unrelated reason (a crash in the wrapper, a broken Makefile)
        # must not pass for the guard working.
        should_pass = name.startswith("control_")
        passed = proc.returncode == 0
        expectation = (
            ("exit 0" if marker is None else f"exit 0 mentioning {marker!r}")
            if should_pass
            else f"a non-zero exit mentioning {marker!r}"
        )
        if passed == should_pass and (marker is None or marker in proc.stdout):
            print(f"ok    {name}: exit {proc.returncode} ({expectation} expected)")
        else:
            failed.append(name)
            print(f"FAIL  {name}: exit {proc.returncode}, expected {expectation}. Output:")
            for line in proc.stdout.rstrip().splitlines():
                if "warning: overriding recipe" in line or "warning: ignoring old recipe" in line:
                    continue
                print(f"      | {line}")

    print(f"\n{len(CASES) - len(failed)}/{len(CASES)} scenarios behaved as expected")
    if failed:
        print("Failed: " + ", ".join(failed), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
