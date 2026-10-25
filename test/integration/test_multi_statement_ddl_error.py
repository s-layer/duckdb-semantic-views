#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Semantic-view DDL inside a multi-statement string runs (TECH-DEBT #85).

DuckDB's grammar splits the string and parses each statement, the
semantic-view ones through the extension's grammar productions, so they run
wherever DuckDB's own statements do. Each scenario runs on a fresh in-memory
database and reports independently:

  ddl_after_other_statements   CREATE TABLE; INSERT; CREATE SEMANTIC VIEW in one
                               execute() -- the examples/type_inference.py shape.
  ddl_before_other_statement   CREATE SEMANTIC VIEW ...; SELECT 1 in one execute().
  ddl_after_read_call          SELECT * FROM semantic_view(...); CREATE SEMANTIC
                               VIEW ... in one execute().
  error_shows_its_own_line     an invalid CREATE on the second line of a
                               multi-statement string: the caret says LINE 2.
  ddl_after_describe           SELECT * FROM describe_semantic_view(...); CREATE
                               SEMANTIC VIEW ... -- DuckDB 2.0 runs the read
                               before it parses the DDL.
  caret_after_read_call        an invalid DDL on line 2, after a read call: the
                               caret still points into the query as written.
  ddl_after_read_with_semicolon  a read call whose view's expression holds a `;`
                               in a literal; the DDL after it still runs.
  control_single_statement     DDL alone with a trailing semicolon still works.
  reserved_word_name           a view named `select`: refused unquoted, CREATE
                               included, as DuckDB refuses it in its own DDL;
                               quoted, it works alone and in a multi-statement
                               string, and GET_DDL quotes it so its output runs.
  statement_costs_its_own_length  100 DROPs after a 40000-element SELECT: each
                               DROP is handed only its own tokens, so the string
                               parses in about the time of its parts.

Run: uv run test/integration/test_multi_statement_ddl_error.py (or
`just test-multi-statement-error`).
"""

import sys
import time
from pathlib import Path

import duckdb

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_extension_path

EXT_PATH = get_extension_path()

DDL = (
    "CREATE SEMANTIC VIEW sales AS "
    "TABLES (o AS orders PRIMARY KEY (id)) "
    "DIMENSIONS (o.region AS o.region) "
    "METRICS (o.revenue AS SUM(o.amount))"
)
BAD_DDL = DDL.replace("DIMENSIONS", "DIMENSIONZ", 1)
BAD_MSG = "Unknown clause keyword 'DIMENSIONZ'"


def connect():
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT_PATH}'")
    return con


def error_of(con, sql):
    try:
        con.execute(sql)
    except duckdb.Error as e:
        return str(e)
    return None


def views(con):
    return sorted(r[0] for r in con.execute("SELECT name FROM list_semantic_views()").fetchall())


def scenario_ddl_after_other_statements():
    con = connect()
    try:
        con.execute(
            "CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE); "
            "INSERT INTO orders VALUES (1, 'US', 100); " + DDL + ";"
        )
        rows = con.execute(
            "SELECT * FROM semantic_view('sales', dimensions := ['region'], metrics := ['revenue'])"
        ).fetchall()
    finally:
        con.close()
    assert rows == [("US", 100.0)], f"every statement must run; got {rows!r}"


def scenario_ddl_before_other_statement():
    con = connect()
    try:
        con.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
        con.execute(DDL + "; SELECT 1;")
        names = views(con)
    finally:
        con.close()
    assert names == ["sales"], f"the CREATE must run; got {names!r}"


def scenario_ddl_after_read_call():
    con = connect()
    try:
        con.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
        con.execute(DDL.replace("sales", "existing", 1))
        con.execute("SELECT * FROM semantic_view('existing', metrics := ['revenue']); " + DDL)
        names = views(con)
    finally:
        con.close()
    assert names == ["existing", "sales"], f"the CREATE must run; got {names!r}"


def scenario_error_shows_its_own_line():
    con = connect()
    try:
        msg = error_of(con, "SELECT 1;\n" + BAD_DDL)
    finally:
        con.close()
    assert msg is not None and BAD_MSG in msg, f"expected {BAD_MSG!r}; got: {msg}"
    assert "LINE 2:" in msg, f"the caret must point into line 2; got: {msg}"


def scenario_ddl_after_describe():
    con = connect()
    try:
        con.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
        con.execute(DDL.replace("sales", "existing", 1))
        con.execute("SELECT * FROM describe_semantic_view('existing'); " + DDL)
        names = views(con)
    finally:
        con.close()
    assert names == ["existing", "sales"], f"the CREATE must run; got {names!r}"


def scenario_caret_after_read_call():
    con = connect()
    try:
        con.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
        con.execute(DDL.replace("sales", "existing", 1))
        bad = DDL.replace("DIMENSIONS", "DIMENSIONZ", 1)
        msg = error_of(
            con, "SELECT * FROM semantic_view('existing', metrics := ['revenue']);\n" + bad
        )
    finally:
        con.close()
    assert msg is not None, "an invalid DDL must fail"
    assert "LINE 2:" in msg, f"the caret must point into the query as written; got: {msg}"


def scenario_ddl_after_read_with_semicolon():
    con = connect()
    try:
        con.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
        con.execute(
            "CREATE SEMANTIC VIEW semi_v AS TABLES (o AS orders PRIMARY KEY (id)) "
            "DIMENSIONS (o.label AS CASE WHEN o.region = 'a;b' THEN 'x' ELSE o.region END) "
            "METRICS (o.revenue AS SUM(o.amount))"
        )
        con.execute(
            "SELECT * FROM semantic_view('semi_v', dimensions := ['label'], "
            "metrics := ['revenue']); " + DDL
        )
        names = views(con)
    finally:
        con.close()
    assert names == ["sales", "semi_v"], f"the CREATE must run; got {names!r}"


def scenario_control_single_statement():
    con = connect()
    try:
        con.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
        con.execute(DDL + ";")
        rows = con.execute(
            "SELECT * FROM semantic_view('sales', dimensions := ['region'], metrics := ['revenue'])"
        ).fetchall()
    finally:
        con.close()
    assert rows == [], f"empty table must give no rows; got {rows!r}"


def scenario_reserved_word_name():
    con = connect()
    try:
        con.execute("CREATE TABLE t (id INTEGER, r VARCHAR)")
        unquoted = error_of(
            con,
            "CREATE SEMANTIC VIEW select AS TABLES (t AS t PRIMARY KEY (id)) DIMENSIONS (t.r AS t.r)",
        )
        con.execute(
            'CREATE SEMANTIC VIEW "select" AS TABLES (t AS t PRIMARY KEY (id)) '
            "DIMENSIONS (t.r AS t.r)"
        )
        described = error_of(con, 'DESCRIBE SEMANTIC VIEW "select"; SELECT 1')
        ddl = con.execute("SELECT get_ddl('SEMANTIC_VIEW', 'select')").fetchone()[0]
        con.execute('DROP SEMANTIC VIEW "select"')
        rerun = error_of(con, ddl)
        names = views(con)
    finally:
        con.close()
    assert unquoted is not None and "syntax error" in unquoted, f"got: {unquoted}"
    assert described is None, f"got: {described}"
    assert '"select"' in ddl, f"GET_DDL must quote the name; got: {ddl}"
    assert rerun is None and names == ["select"], f"GET_DDL must run; got: {rerun} / {names}"


def parse_seconds(con, sql):
    """The best of three parses of `sql` (no execution)."""
    best = None
    for _ in range(3):
        start = time.perf_counter()
        con.extract_statements(sql)
        took = time.perf_counter() - start
        best = took if best is None else min(best, took)
    return best


def scenario_statement_costs_its_own_length():
    # Handing every statement the whole string's tokens made a string of K
    # statements cost K times its length: here about ten times its parts.
    drops = "; ".join(f"DROP SEMANTIC VIEW IF EXISTS v{i}" for i in range(100))
    filler = "SELECT [" + ", ".join(["1"] * 40000) + "]"
    con = connect()
    try:
        parts = parse_seconds(con, drops) + parse_seconds(con, filler)
        both = parse_seconds(con, filler + "; " + drops)
    finally:
        con.close()
    assert both < 2.5 * parts, f"the string took {both:.2f}s, its parts {parts:.2f}s"


SCENARIOS = [
    scenario_ddl_after_other_statements,
    scenario_ddl_before_other_statement,
    scenario_ddl_after_read_call,
    scenario_error_shows_its_own_line,
    scenario_ddl_after_describe,
    scenario_caret_after_read_call,
    scenario_ddl_after_read_with_semicolon,
    scenario_control_single_statement,
    scenario_reserved_word_name,
    scenario_statement_costs_its_own_length,
]


def main() -> int:
    failed = []
    for scenario in SCENARIOS:
        name = scenario.__name__.removeprefix("scenario_")
        try:
            scenario()
            print(f"ok    {name}")
        except AssertionError as e:
            failed.append(name)
            print(f"FAIL  {name}: {e}")
    print(f"\n{len(SCENARIOS) - len(failed)}/{len(SCENARIOS)} scenarios passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
