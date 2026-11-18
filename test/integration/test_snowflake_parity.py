#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
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
- a statement Snowflake refused fails here too (the message is not compared).

Snowflake is the reference for what a semantic view means, so a difference is a bug here
unless test/snowflake_parity/approved_differences.toml lists it with the reason it is
expected. An approved difference that no longer differs also fails the run, so the list
cannot go stale.

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


def parse_recording(path: Path) -> list[Recorded]:
    """The statements of a recorded run, with what Snowflake returned for each."""
    statements = []
    for section in path.read_text().split("\n## ")[1:]:
        label = section.split("\n", 1)[0].strip()
        sql = re.search(r"```sql\n(.*?)\n```", section, re.S)
        assert sql is not None, f"{path.name}: section {label!r} has no SQL"
        after = section[sql.end() :]
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
        if columns == ["status"]:  # DDL: Snowflake reports a status line, not rows
            columns, rows = [], []
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


def compare(statement: Recorded, con: duckdb.DuckDBPyConnection) -> str | None:
    """None when we do what Snowflake did, else what differs."""
    try:
        result = con.sql(to_duckdb(statement.sql))
        rows = result.fetchall() if result is not None else []
        columns = [c.lower() for c in result.columns] if result is not None else []
    except duckdb.Error as exc:
        if statement.error is not None:
            return None
        return f"Snowflake succeeded, we failed: {str(exc).splitlines()[0][:300]}"
    if statement.error is not None:
        return f"Snowflake refused ({statement.error.splitlines()[0][:120]}), we succeeded"
    assert statement.columns is not None and statement.rows is not None
    if not statement.columns or not QUERY.match(statement.sql):
        return None  # DDL or DML: success is the whole answer (row counts are reported apart)
    expected_columns = [c.lower() for c in statement.columns]
    if columns != expected_columns:
        return f"columns: Snowflake {expected_columns}, ours {columns}"
    scales = _scales(statement.rows, len(columns))
    ours = sorted(_key(_rounded(list(row), scales)) for row in rows)
    theirs = sorted(_key(list(row)) for row in statement.rows)
    if ours != theirs:
        return f"rows: Snowflake {theirs}, ours {ours}"
    return None


def main() -> int:
    approved: dict[str, dict[str, str]] = tomllib.loads(APPROVED.read_text())
    ext_path = get_extension_path()
    if not ext_path.exists():
        print(f"ERROR: extension not found at {ext_path}")
        return 1

    failures: list[str] = []
    compared = skipped = approved_hits = 0
    recordings = sorted(RECORDED.glob("*.md"))
    assert recordings, f"no recordings in {RECORDED}"
    for path in recordings:
        fixture = path.stem
        allowed = approved.get(fixture, {})
        con = duckdb.connect(
            config={"allow_unsigned_extensions": "true", "extension_directory": get_ext_dir()}
        )
        con.execute(f"FORCE INSTALL '{ext_path}'")
        con.execute("LOAD semantic_views")
        seen: set[str] = set()
        for statement in parse_recording(path):
            if NOT_COMPARED.match(statement.sql):
                skipped += 1
                continue
            compared += 1
            ident = f"{fixture}:{statement.label}"
            difference = compare(statement, con)
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
        for label in sorted(set(allowed) - seen):
            failures.append(
                f"{fixture}:{label}: approved in {APPROVED.name}, but no such statement is compared"
            )
        con.close()
    for fixture in sorted(set(approved) - {p.stem for p in recordings}):
        failures.append(f"{fixture}: in {APPROVED.name}, but there is no such recording")

    print(
        f"Snowflake parity: {len(recordings)} recordings, {compared} statements compared "
        f"({approved_hits} approved differences), {skipped} not compared"
    )
    for failure in failures:
        print(f"FAIL {failure}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
