#!/usr/bin/env python3
"""
Strict wrapper around the duckdb_sqllogictest runner.

Drop-in for ``python -m duckdb_sqllogictest`` (same arguments; the Makefile
points ``TEST_RUNNER`` here), run with the configure/venv interpreter. It runs
the upstream runner in-process and then refuses to call a run green when the
runner would have reported success without actually testing anything:

1. ``require semantic_views`` that cannot be satisfied FAILS the file instead
   of skipping it. This uses the runner's own ``required_requires`` mechanism
   (which upstream has no CLI flag for). A missing or renamed extension file
   used to skip every file and exit 0.
2. Every file named in ``--file-list`` / ``--file-path`` must produce a result.
   The runner logs a file it cannot parse to its skip log and moves on, with
   no SKIPPED line and no failure.
3. A file that requires semantic_views and reports SUCCESS must have executed
   at least one ``statement`` / ``query``. (``mode skip`` with no ``unskip``
   reports SUCCESS having run nothing.)
4. A SKIPPED file is a failure unless it was skipped by a ``require`` for
   something other than a required extension -- e.g. ``require windows`` /
   ``require notwindows`` (see scripts/patch_sqllogictest.py), which BuildAll
   legitimately hits on Windows/macOS. Those skips are listed loudly at the end.
   Anything else (``halt``, unsupported statements, decorators) is a failure.
5. A file that hits one of the runner's ``ignore_error_messages`` ("HTTP",
   "Unable to connect") fails. Upstream treats such an error as a PASS: the
   failing query's expected result is never checked, and the file reports
   SUCCESS. The only exception is the local offline opt-in: with
   ``SV_OFFLINE_EXTENSIONS=1`` (refused when ``CI`` is set), a file passed as
   ``--offline-excused FILE`` (a file that needs an extension downloaded from
   extensions.duckdb.org) is reported loudly as SKIPPED instead. This is for
   running the wrapper directly: ``make`` passes no such file, because under
   the opt-in it leaves its ``OFFLINE_EXTENSION_TESTS`` out of the list.
   Any other file that hits a network error still fails, offline or
   not.
6. A file in which any statement or query raised an ``INTERNAL Error`` fails,
   even where a ``statement error`` / expected-error block matched a substring
   of it. DuckDB wraps some failures (e.g. a scalar function that throws
   without being marked fallible) in an INTERNAL error that keeps the original
   message inside it, so a substring check passes on what is really a crash
   path. Upstream lists "INTERNAL" in ``always_fail_error_messages`` but never
   consults it.

With ``SV_ROUTE_TO_GRAMMAR=1`` the wrapper also runs every statement the
extension's grammar productions cover (``GRAMMAR_OWNED`` below) with parser
overrides off on its connection, so it is parsed by DuckDB's grammar and our
productions instead of the parser override; everything else runs as usual.
That runs the same files against both implementations while they coexist
(``just test-sql-grammar``). A statement whose expected error pins a caret
(``LINE 1:``, or a bare ``LINE``) is not routed (``CARET_EXPECTED`` below). The summary reports how
many statements were routed, and a run that routed none fails.

The required extensions default to ``semantic_views``; pass
``--required-extension NAME`` (repeatable) to change that. ``--offline-excused
FILE`` (repeatable) is described in 5. Every other argument is forwarded to the
runner untouched.

The upstream package is pinned by extension-ci-tools and already patched by
scripts/patch_sqllogictest.py; this wrapper hooks six of its methods
(``SQLLogicRunner.reset`` / ``skip_error_message``,
``SQLLogicContext.execute_statement`` / ``execute_query`` / ``get_connection``,
``SQLLogicTestExecutor.execute_test``). If a runner bump
renames them, the ``getattr`` checks below fail loudly rather than letting the
guards go quiet.
"""

from __future__ import annotations

import argparse
import faulthandler
import io
import os
import re
import sys
from dataclasses import dataclass, field

import duckdb
from duckdb_sqllogictest import python_runner, result
from duckdb_sqllogictest.result import ExecuteResult


@dataclass
class FileOutcome:
    path: str
    result: str = "NOT RUN"
    executed: int = 0
    skip_reason: str = ""
    requires: set[str] = field(default_factory=set)
    network_errors: list[str] = field(default_factory=list)
    internal_errors: list[str] = field(default_factory=list)


# Statements the grammar productions in src/grammar/ cover. Extend it as each
# statement moves onto the grammar.
GRAMMAR_OWNED = re.compile(
    r"^\s*(?:(?:--[^\n]*(?:\n|$)|/\*.*?\*/)\s*)*"
    r"(?:show\s+(?:terse\s+)?semantic\b|show\s+columns\s+in\s+semantic\b|desc(?:ribe)?\s+semantic\b"
    r"|drop\s+semantic\b|alter\s+semantic\b|create\s+(?:or\s+replace\s+)?semantic\b)",
    re.IGNORECASE | re.DOTALL,
)

# An expected error that pins a caret (`LINE 1: ... ^`, or just `LINE`) stays on the parser
# override. The grammar raises a parser error with its location, and DuckDB
# renders that only on the ClientContext path, which the Python client the
# runner uses does not take (DuckDB's own syntax errors show none either). See
# TECH-DEBT #99.
CARET_EXPECTED = re.compile(r"\bLINE(?: \d+:|$)", re.MULTILINE)


def _routed(statement) -> bool:
    if not GRAMMAR_OWNED.match("\n".join(statement.lines)):
        return False
    expected = getattr(statement, "expected_result", None)
    lines = getattr(expected, "lines", None) or []
    return not CARET_EXPECTED.search("\n".join(lines))


class _InternalErrorWatch:
    """Connection proxy that records every INTERNAL error before re-raising it.

    Only ``execute`` is watched: that is how the runner runs ``statement``
    blocks, the only place an expected error can be written. ``query`` must
    stay the real method, because DuckDB's replacement scan resolves the
    runner's ``original_rel`` from the calling frame's locals, which a wrapper
    frame would hide; a query that raises fails its block anyway.
    """

    def __init__(self, conn, sink):
        self._conn = conn
        self._sink = sink

    def _watch(self, fn, *args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except duckdb.Error as e:
            if "INTERNAL Error" in str(e) and self._sink:
                self._sink[-1].internal_errors.append(str(e).splitlines()[0])
            raise

    def execute(self, *args, **kwargs):
        return self._watch(self._conn.execute, *args, **kwargs)

    def __getattr__(self, name):
        return getattr(self._conn, name)


def _require_params(test) -> set[str]:
    params = set()
    for statement in test.statements:
        if type(statement).__name__ == "Require":
            params.add(statement.header.parameters[0].lower())
    return params


def _hook(cls, name):
    original = getattr(cls, name, None)
    if original is None:
        sys.exit(
            f"ERROR: duckdb_sqllogictest no longer has {cls.__name__}.{name}; "
            "scripts/run_sqllogictest.py must be updated for the new runner"
        )
    return original


def main() -> int:
    # A crash in the engine or the extension kills the process with no Python
    # error, and on Windows an access violation leaves only make's exit code.
    # faulthandler prints where each thread was, and line buffering keeps the
    # runner's `[n/N] <file>` line, which a pipe would otherwise hold in a
    # buffer the crash discards.
    faulthandler.enable()
    if isinstance(sys.stdout, io.TextIOWrapper):
        sys.stdout.reconfigure(line_buffering=True)
    pre = argparse.ArgumentParser(add_help=False)
    pre.add_argument("--required-extension", action="append", default=None)
    pre.add_argument("--offline-excused", action="append", default=[])
    pre.add_argument("--file-list")
    pre.add_argument("--file-path")
    ours, _ = pre.parse_known_args()
    required = {e.lower() for e in (ours.required_extension or ["semantic_views"])}

    offline = os.environ.get("SV_OFFLINE_EXTENSIONS") == "1"
    if offline and os.environ.get("CI"):
        print(
            "ERROR: SV_OFFLINE_EXTENSIONS=1 excuses network failures and is for local "
            f"offline runs only; refusing because CI={os.environ['CI']!r}",
            file=sys.stderr,
        )
        return 1
    excused = {os.path.normpath(p) for p in ours.offline_excused}

    # Strip our own options; forward everything else to the upstream runner.
    own_options = ("--required-extension", "--offline-excused")
    forwarded = []
    skip_next = False
    for arg in sys.argv[1:]:
        if skip_next:
            skip_next = False
            continue
        if arg in own_options:
            skip_next = True
            continue
        if arg.startswith(tuple(o + "=" for o in own_options)):
            continue
        forwarded.append(arg)

    if ours.file_list:
        with open(ours.file_list) as f:
            expected = f.read().split()
    elif ours.file_path:
        expected = [ours.file_path]
    else:
        expected = None  # directory glob: no fixed list to check against

    outcomes: dict[str, FileOutcome] = {}
    current: list[FileOutcome] = []

    # 1. Required extensions fail instead of skipping.
    orig_reset = _hook(result.SQLLogicRunner, "reset")

    def reset(self):
        orig_reset(self)
        self.required_requires.update(required)

    result.SQLLogicRunner.reset = reset

    # 5. Record every error the runner would wave through as a network error.
    orig_skip_error_message = _hook(result.SQLLogicRunner, "skip_error_message")

    def skip_error_message(self, message):
        ignored = orig_skip_error_message(self, message)
        if ignored and current:
            current[-1].network_errors.append(str(message).splitlines()[0])
        return ignored

    result.SQLLogicRunner.skip_error_message = skip_error_message

    # 3. Count executed statements/queries (loop bodies included), and with
    # SV_ROUTE_TO_GRAMMAR=1 run the grammar-owned ones with overrides off.
    route = os.environ.get("SV_ROUTE_TO_GRAMMAR") == "1"
    routed = [0]
    # A routed statement that fails inside a transaction aborts it, and no SET
    # runs until the file's own ROLLBACK. The setting it had is restored then,
    # before the connection's next statement (keyed by connection identity).
    pending_restore = {}
    orig_get_connection = _hook(result.SQLLogicContext, "get_connection")

    def restore(conn, setting):
        try:
            conn.execute(f"SET allow_parser_override_extension = '{setting}'")
            pending_restore.pop(id(conn), None)
        except duckdb.TransactionException:
            pending_restore[id(conn)] = setting

    for method in ("execute_statement", "execute_query"):
        orig = _hook(result.SQLLogicContext, method)

        def counted(self, statement, _orig=orig):
            if current:
                current[-1].executed += 1
            if not route:
                return _orig(self, statement)
            conn = orig_get_connection(self, statement.connection_name)
            if id(conn) in pending_restore:
                restore(conn, pending_restore[id(conn)])
            if not _routed(statement):
                return _orig(self, statement)
            routed[0] += 1
            try:
                before = pending_restore.get(id(conn)) or conn.execute(
                    "SELECT current_setting('allow_parser_override_extension')"
                ).fetchone()[0]
                conn.execute("SET allow_parser_override_extension = 'DEFAULT'")
            except duckdb.TransactionException:
                # Still aborted: the statement fails whichever parser runs it.
                return _orig(self, statement)
            try:
                return _orig(self, statement)
            finally:
                restore(conn, before)

        setattr(result.SQLLogicContext, method, counted)

    # 6. Record every INTERNAL error, whether or not an expected error matched it.
    def get_connection(self, name=None):
        return _InternalErrorWatch(orig_get_connection(self, name), current)

    result.SQLLogicContext.get_connection = get_connection

    # 2./4. Record each file's result and skip reason.
    orig_execute_test = _hook(python_runner.SQLLogicTestExecutor, "execute_test")

    def execute_test(self, test):
        outcome = FileOutcome(path=test.path, requires=_require_params(test))
        outcomes[test.path] = outcome
        current.append(outcome)
        skip_log_before = len(self.skip_log)
        try:
            res = orig_execute_test(self, test)
        finally:
            current.pop()
        outcome.result = res.type.name
        if res.type == ExecuteResult.Type.SKIPPED and len(self.skip_log) > skip_log_before:
            outcome.skip_reason = self.skip_log[-1]
        return res

    python_runner.SQLLogicTestExecutor.execute_test = execute_test

    sys.argv = [sys.argv[0], *forwarded]
    exit_code = 0
    try:
        python_runner.SQLLogicPythonRunner().run()
    except SystemExit as e:  # the runner calls exit(1) on the first ERROR
        exit_code = e.code if isinstance(e.code, int) else 1
    if exit_code:
        print(f"sqllogictest: FAILED (runner exit {exit_code})", file=sys.stderr)
        return exit_code

    problems: list[str] = []
    allowed_skips: list[str] = []

    if expected is not None:
        for path in expected:
            if path not in outcomes:
                problems.append(
                    f"{path}: never ran (the runner could not parse it, or skipped it unannounced)"
                )
    if not outcomes:
        problems.append("no test files ran at all")
    if route and not routed[0]:
        problems.append("SV_ROUTE_TO_GRAMMAR=1, but no statement was routed to the grammar")

    for outcome in outcomes.values():
        needs_required = bool(outcome.requires & required)
        if outcome.internal_errors:
            problems.append(
                f"{outcome.path}: raised an INTERNAL error, which an expected-error "
                f"substring cannot excuse: {outcome.internal_errors[0]}"
            )
            continue
        if outcome.network_errors:
            first = outcome.network_errors[0]
            if offline and os.path.normpath(outcome.path) in excused:
                allowed_skips.append(
                    f"SKIPPED (SV_OFFLINE_EXTENSIONS=1): {outcome.path} -- needs a DuckDB "
                    f"extension downloaded from extensions.duckdb.org; hit: {first}"
                )
            else:
                problems.append(
                    f"{outcome.path}: hit an ignored network error, which the runner "
                    f"counts as a pass without checking the result: {first}"
                )
            continue
        if outcome.result == "SUCCESS":
            if needs_required and outcome.executed == 0:
                problems.append(
                    f"{outcome.path}: reported SUCCESS but executed 0 statements/queries"
                )
        elif outcome.result == "SKIPPED":
            reason = outcome.skip_reason
            # "<file>:<line> require <param>: Missing" for a non-required param
            # is a legitimate platform/feature skip; everything else is not.
            marker = " require "
            param = ""
            if marker in reason and reason.endswith(": Missing"):
                param = reason.split(marker, 1)[1][: -len(": Missing")].strip().lower()
            if param and param not in required:
                allowed_skips.append(reason)
            else:
                problems.append(f"{outcome.path}: SKIPPED ({reason or 'no reason logged'})")
        else:
            problems.append(f"{outcome.path}: result {outcome.result}")

    for line in allowed_skips:
        if line.startswith("SKIPPED"):
            print(line)
        else:
            print(f"SKIPPED (allowed, require not met on this platform): {line}")

    ran = sum(1 for o in outcomes.values() if o.result == "SUCCESS")
    statements = sum(o.executed for o in outcomes.values())
    if problems:
        print(
            f"sqllogictest: FAILED -- {len(problems)} file(s) failed the wrapper's checks "
            f"(required extensions: {', '.join(sorted(required))}):",
            file=sys.stderr,
        )
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 1

    print(
        f"sqllogictest: {ran} file(s) ran, {statements} statements/queries executed"
        + (f", {len(allowed_skips)} skipped (listed above)" if allowed_skips else "")
        + (f", {routed[0]} routed to the grammar" if route else "")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
