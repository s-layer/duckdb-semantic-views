#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.1"]
# requires-python = ">=3.11"
# ///
"""
Replay recorded Snowflake fixture runs against the extension.

Each file in test/snowflake_parity/recorded/ is the output of a fixture run on Snowflake:
every statement, in order, with the rows it returned or the error it raised. This runs the
same statements here and checks, statement by statement, that we do what Snowflake did:

- a statement Snowflake ran succeeds here too;
- a query Snowflake answered returns the same rows (as a multiset; numbers compared by
  value, so 350.00000000 and 350.0 agree; column names compared case-insensitively);
- a statement Snowflake refused fails here too (the message is not compared);
- an INSERT adds as many rows as it did on Snowflake.

Snowflake is the reference for what a semantic view means, so a difference is a bug here
unless test/snowflake_parity/approved_differences.toml lists it under `differences` with
the reason it is expected. An approved difference that no longer differs also fails the run,
so the list cannot go stale. A query whose Snowflake answer can't be reproduced at all (it
depends on names Snowflake invented for that run) is listed under `unreproducible` instead:
it must still succeed, but its rows are not compared.

A statement whose run lost its connection before Snowflake answered is recorded as "Not
reached". It is not run here either, so later statements see the state Snowflake saw.

A SHOW is held to the same column names and to listing the same names (its `name`
column, case-insensitively), each under the same logical table (its `table_name` column,
case-insensitively, an empty one matching Snowflake's NULL); its other cells are each
engine's own spelling of types and empty lists.

Not compared, and counted in the summary: schema setup (CREATE/USE/DROP SCHEMA, since the
fixtures ran in a throwaway Snowflake schema), EXPLAIN (plans differ by design) and
DESCRIBE (Snowflake's output uses its own name folding and qualification).

The statements run with two dialect changes: NUMBER(p, s) is DECIMAL(p, s), and a
relationship written without a name gets one (rel_1, rel_2, ...), since Snowflake invents a
name for it.

Usage:
    uv run test/integration/test_snowflake_parity.py

Exit codes:
    0 = every compared statement agrees, or differs as approved
    1 = an unapproved difference, or a stale approval
"""

from __future__ import annotations

import math
import re
import sys
import tomllib
from dataclasses import dataclass
from decimal import ROUND_HALF_UP, Decimal, InvalidOperation
from pathlib import Path

import duckdb

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path, get_project_root  # noqa: E402

PARITY = get_project_root() / "test" / "snowflake_parity"
RECORDED = PARITY / "recorded"
APPROVED = PARITY / "approved_differences.toml"

QUERY = re.compile(r"^\s*(SELECT|WITH|SHOW)\b", re.I)
SHOW = re.compile(r"^\s*SHOW\b", re.I)
NOT_COMPARED = re.compile(
    r"^\s*(CREATE (OR REPLACE )?SCHEMA|USE |DROP SCHEMA|EXPLAIN|DESCRIBE)", re.I
)


@dataclass
class Recorded:
    label: str
    sql: str
    error: str | None
    columns: list[str] | None
    rows: list[list[str]] | None
    reached: bool = True


def parse_recording(path: Path) -> list[Recorded]:
    """The statements of a recorded run, with what Snowflake returned for each."""
    statements = []
    for section in path.read_text().split("\n## ")[1:]:
        label = section.split("\n", 1)[0].strip()
        sql = re.search(r"```sql\n(.*?)\n```", section, re.S)
        assert sql is not None, f"{path.name}: section {label!r} has no SQL"
        after = section[sql.end() :]
        if "**Not reached:**" in after:  # the connection failed before Snowflake answered
            statements.append(Recorded(label, sql.group(1), None, None, None, reached=False))
            continue
        error = re.search(r"\*\*Error:\*\*\n\n```\n(.*?)\n```", after, re.S)
        if error is not None:
            statements.append(Recorded(label, sql.group(1), error.group(1), None, None))
            continue
        lines = [line for line in after.splitlines() if line.startswith("|")]
        if not lines:
            statements.append(Recorded(label, sql.group(1), None, [], []))
            continue
        columns = _cells(lines[0])
        rows = [_cells(line) for line in lines[2:]]
        if columns == ["status"] and not QUERY.match(sql.group(1)):
            columns, rows = [], []  # DDL: Snowflake reports a status line, not rows
        statements.append(Recorded(label, sql.group(1), None, columns, rows))
    return statements


def _cells(line: str) -> list[str]:
    return [cell.strip() for cell in line.strip().strip("|").split("|")]


def to_duckdb(sql: str) -> str:
    """The two dialect changes the module docstring lists."""
    sql = re.sub(r"\bNUMBER\(", "DECIMAL(", sql, flags=re.I)
    names = iter(range(1, 1000))

    def name(match: re.Match[str]) -> str:
        return f"{match.group(1)}rel_{next(names)} AS {match.group(2)}"

    # An unnamed relationship: `<table>(<cols>) REFERENCES`, not preceded by `<name> AS`.
    return re.sub(
        r"(RELATIONSHIPS\s*\(\s*|,\s*)(\w+\s*\([\w\s,]+\)\s+REFERENCES)", name, sql, flags=re.I
    )


def _value(cell: object) -> object:
    """A cell in comparable form: None, a Decimal for anything numeric, else its text."""
    if cell is None or cell == "None":
        return None
    if isinstance(cell, bool):
        return str(cell).lower()
    if isinstance(cell, (int, float, Decimal)):
        if isinstance(cell, float) and not math.isfinite(cell):
            return str(cell)
        return Decimal(str(cell)).normalize()
    text = str(cell)
    try:
        return Decimal(text).normalize()
    except InvalidOperation:
        return text


def _scales(rows: list[list[str]], width: int) -> list[int | None]:
    """
    Per column, the decimal places Snowflake printed (the most any of its cells has).

    Snowflake gives an average a fixed scale (AVG of a NUMBER(10,2) prints 6 or 8 places);
    DuckDB gives a double. Our values are rounded to Snowflake's places before comparing, so
    2.3333333333333335 matches 2.333333 while 2.3334 would not.
    """
    scales: list[int | None] = [None] * width
    for row in rows:
        for i, cell in enumerate(row):
            match = re.fullmatch(r"-?\d+\.(\d+)", cell)
            if match is not None:
                scales[i] = max(scales[i] or 0, len(match.group(1)))
    return scales


def _rounded(row: list[object], scales: list[int | None]) -> list[object]:
    out: list[object] = []
    for cell, scale in zip(row, scales, strict=True):
        if scale is not None and isinstance(cell, (float, Decimal)) and math.isfinite(cell):
            cell = Decimal(str(cell)).quantize(Decimal(1).scaleb(-scale), ROUND_HALF_UP)
        out.append(cell)
    return out


def _key(row: list[object]) -> tuple[str, ...]:
    return tuple(repr(_value(cell)) for cell in row)


INSERTED = ["number of rows inserted"]


def compare(
    statement: Recorded, con: duckdb.DuckDBPyConnection, *, compare_rows: bool = True
) -> str | None:
    """
    None when we do what Snowflake did, else what differs.

    With ``compare_rows=False`` only success or failure is compared (see ``unreproducible`` in
    approved_differences.toml).
    """
    try:
        cursor = con.execute(to_duckdb(statement.sql))
        description = cursor.description or []
        columns = [str(d[0]).lower() for d in description]
        rows = cursor.fetchall() if description else []
    except duckdb.Error as exc:
        if statement.error is not None:
            return None
        return f"Snowflake succeeded, we failed: {str(exc).splitlines()[0][:300]}"
    if statement.error is not None:
        return f"Snowflake refused ({statement.error.splitlines()[0][:120]}), we succeeded"
    assert statement.columns is not None and statement.rows is not None
    if not QUERY.match(statement.sql):
        if [c.lower() for c in statement.columns] == INSERTED:
            theirs = sum(int(row[0]) for row in statement.rows)
            ours = sum(int(row[0]) for row in rows)
            if ours != theirs:
                return f"rows inserted: Snowflake {theirs}, ours {ours}"
        return None  # other DDL and DML: success is the whole answer
    if not statement.columns:
        return "the recording has no result table for this query, so nothing can be compared"
    if not compare_rows:
        return None
    expected_columns = [c.lower() for c in statement.columns]
    if columns != expected_columns:
        return f"columns: Snowflake {expected_columns}, ours {columns}"
    if SHOW.match(statement.sql) and "name" in columns:
        # SHOW spells cells in each engine's own way (Snowflake's NUMBER(38,0) for our
        # INTEGER, upper-case names, NULL for an empty list), so compare what it lists,
        # and under which logical table.
        at = [columns.index(c) for c in ("table_name", "name") if c in columns]
        listed = sorted(tuple(_listed(row[i]) for i in at) for row in rows)
        expected = sorted(tuple(_listed(row[i]) for i in at) for row in statement.rows)
        if listed != expected:
            return f"listed: Snowflake {expected}, ours {listed}"
        return None
    scales = _scales(statement.rows, len(columns))
    ours = sorted(_key(_rounded(list(row), scales)) for row in rows)
    theirs = sorted(_key(list(row)) for row in statement.rows)
    if ours != theirs:
        return f"rows: Snowflake {theirs}, ours {ours}"
    return None


def _listed(cell: object) -> str:
    """A SHOW name cell compared case-insensitively, an empty one as Snowflake's NULL."""
    text = "" if cell is None else str(cell)
    return "" if text == "None" else text.lower()


def main() -> int:
    listed = tomllib.loads(APPROVED.read_text())
    assert set(listed) <= {"differences", "unreproducible"}, f"{APPROVED.name}: {set(listed)}"
    approved: dict[str, dict[str, str]] = listed.get("differences", {})
    unreproducible: dict[str, dict[str, str]] = listed.get("unreproducible", {})
    ext_path = get_extension_path()
    if not ext_path.exists():
        print(f"ERROR: extension not found at {ext_path}")
        return 1

    failures: list[str] = []
    compared = skipped = approved_hits = success_only = not_reached = 0
    recordings = sorted(RECORDED.glob("*.md"))
    assert recordings, f"no recordings in {RECORDED}"
    for path in recordings:
        fixture = path.stem
        allowed = approved.get(fixture, {})
        loose = unreproducible.get(fixture, {})
        for label in sorted(set(allowed) & set(loose)):
            failures.append(f"{fixture}:{label}: listed as both a difference and unreproducible")
        con = duckdb.connect(
            config={"allow_unsigned_extensions": "true", "extension_directory": get_ext_dir()}
        )
        con.execute(f"FORCE INSTALL '{ext_path}'")
        con.execute("LOAD semantic_views")
        seen: set[str] = set()
        for statement in parse_recording(path):
            if not statement.reached:
                not_reached += 1  # nor run here, so the state stays as Snowflake's was
                continue
            if NOT_COMPARED.match(statement.sql):
                skipped += 1
                continue
            compared += 1
            ident = f"{fixture}:{statement.label}"
            loosened = statement.label in loose
            difference = compare(statement, con, compare_rows=not loosened)
            if loosened:
                seen.add(statement.label)
                if not QUERY.match(statement.sql):
                    failures.append(f"{ident}: listed as unreproducible, but it is not a query")
                success_only += 1
            if statement.label in allowed:
                seen.add(statement.label)
                if difference is None:
                    failures.append(
                        f"{ident}: approved as a difference, but now agrees with Snowflake;"
                        f" remove it from {APPROVED.name}"
                    )
                else:
                    approved_hits += 1
            elif difference is not None:
                failures.append(f"{ident}: {difference}\n    {statement.sql.splitlines()[0][:160]}")
        for label in sorted((set(allowed) | set(loose)) - seen):
            failures.append(
                f"{fixture}:{label}: approved in {APPROVED.name}, but no such statement is compared"
            )
        con.close()
    for fixture in sorted((set(approved) | set(unreproducible)) - {p.stem for p in recordings}):
        failures.append(f"{fixture}: in {APPROVED.name}, but there is no such recording")

    print(
        f"Snowflake parity: {len(recordings)} recordings, {compared} statements compared "
        f"({approved_hits} approved differences, {success_only} unreproducible so success only),"
        f" {skipped} not compared, {not_reached} never reached Snowflake"
    )
    for failure in failures:
        print(f"FAIL {failure}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
