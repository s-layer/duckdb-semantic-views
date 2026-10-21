#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Semantic-view DDL inside a multi-statement string runs (TECH-DEBT #85).

`parser_override` sees the WHOLE query string and declines a string holding
several statements. DuckDB's grammar then splits it and runs each statement,
the semantic-view ones through the extension's grammar productions. Before
CREATE moved onto the grammar, the parse stub refused such a CREATE with "one
semantic-view DDL statement per call" (and, earlier still, blamed the override
setting).

The grammar bridge learns a statement's source text from the query the override
recorded on this thread, and only when every token matches it. The controls
check that a stale record from another query never lends its text or its caret.

Each scenario runs on a fresh in-memory database and reports independently:

  ddl_after_other_statements   CREATE TABLE; INSERT; CREATE SEMANTIC VIEW in one
                               execute() -- the examples/type_inference.py shape.
  ddl_before_other_statement   CREATE SEMANTIC VIEW ...; SELECT 1 in one execute().
  ddl_after_read_call          SELECT * FROM semantic_view(...); CREATE SEMANTIC
                               VIEW ... -- a string the override REWRITES (the
                               read call's search path) rather than declines.
  error_shows_its_own_line     an invalid CREATE on the second line of a
                               multi-statement string: the caret says LINE 2.
  ddl_after_describe           SELECT * FROM describe_semantic_view(...); CREATE
                               SEMANTIC VIEW ... -- a read the override declines.
                               DuckDB 2.0 runs it before it parses the DDL, and it
                               runs its own catalog queries through the override.
  caret_after_read_call        an invalid DDL on line 2, after a read call: the
                               caret still points into the query as written.
  ddl_after_read_with_semicolon  a read call whose view's expression holds a `;`
                               in a literal, so the queries it runs look like
                               several statements; the DDL after it still runs.
  control_override_disabled    the override really is off (SET
                               allow_parser_override_extension='DEFAULT'), DDL
                               sent alone: it runs through the grammar.
  control_single_statement     DDL alone with a trailing semicolon still works.
  control_stale_caret_query    an invalid DDL in a multi-statement string on
                               one database, then the same invalid DDL alone on
                               another database whose override is off: the
                               second error must not quote the first query.
  control_stale_caret_success  a multi-statement DDL that succeeds through the
                               grammar on one database, then an invalid DDL on
                               another whose override is off: likewise. This is
                               the case that leaves a record behind.
  control_stale_caret_rewrite  the same, but the first DDL is one statement the
                               override rewrites and runs.
  control_stale_caret_parse_error  a query that fails to parse before reaching
                               its DDL on one database, then an invalid DDL on
                               another whose override is off: likewise.
  control_stale_source_gap     a CREATE left as the record, then on another
                               database whose override is off a CREATE whose
                               tokens sit at the same offsets but with spaces
                               where the record had `+o.b`: it must store its
                               own body, not the record's.
  control_stale_source_prefix  the same, where a token (`a`) is a prefix of the
                               record's token at that offset (`ab`).
  control_stale_source_tail    the same, where the record runs on past the
                               last token: the error must quote its own query.
  control_stale_source_newline  the same, where every gap has the same length
                               but the record has a line break where the query
                               has a space: it must store its own spacing.
  control_reserved_word_name   a view named `select`, unquoted: the override
                               takes it, the grammar refuses it as DuckDB does
                               (TECH-DEBT #98). Pins today's messages.
  control_stale_other_database  that statement in a multi-statement string on
                               one database ("one statement per call"), then
                               alone on another whose override is off: it must
                               keep "not active", not reuse the first record.
  control_stale_after_success  the same after a one-statement string holding a
                               `;` that succeeds.
  control_stale_after_typo     the same after a string that fails to parse
                               before its DDL.
  control_stale_after_runtime_error  the same after a string whose earlier
                               statement fails as it runs, so DuckDB 2.0 never
                               parses its DDL.
  control_stale_caret_runtime_error  likewise for an invalid CREATE on another
                               database: its error must not quote the first query.
  statement_costs_its_own_length  100 DROPs after a 40000-element SELECT, parsed
                               with the override on and off: each DROP is
                               handed only its own tokens, so the string parses
                               in about the time of its parts. It used to hand
                               each DROP every token of the string.

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
            "CREATE SEMANTIC VIEW semi AS TABLES (o AS orders PRIMARY KEY (id)) "
            "DIMENSIONS (o.label AS CASE WHEN o.region = 'a;b' THEN 'x' ELSE o.region END) "
            "METRICS (o.revenue AS SUM(o.amount))"
        )
        con.execute(
            "SELECT * FROM semantic_view('semi', dimensions := ['label'], "
            "metrics := ['revenue']); " + DDL
        )
        names = views(con)
    finally:
        con.close()
    assert names == ["sales", "semi"], f"the CREATE must run; got {names!r}"


def scenario_control_override_disabled():
    con = connect()
    try:
        con.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
        con.execute("SET allow_parser_override_extension = 'DEFAULT'")
        con.execute(DDL)
        names = views(con)
    finally:
        con.close()
    assert names == ["sales"], f"the CREATE must run through the grammar; got {names!r}"


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


def check_not_quoted_after(first):
    """Run `first` on one database, then BAD_DDL on another whose override is
    off. The second error must be its own, with no caret from `first` (which
    puts its DDL on line 2)."""
    off, on = connect(), connect()
    try:
        on.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
        off.execute("SET allow_parser_override_extension = 'DEFAULT'")
        first(on)
        msg = error_of(off, BAD_DDL)
    finally:
        off.close()
        on.close()
    assert msg is not None and BAD_MSG in msg, f"expected {BAD_MSG!r}; got: {msg}"
    assert "LINE 2:" not in msg, f"must not quote another query; got: {msg}"


def scenario_control_stale_caret_query():
    def first(on):
        msg = error_of(on, "SELECT 'other query';\n" + BAD_DDL)
        assert msg is not None and "LINE 2:" in msg, f"setup: unexpected first error: {msg}"

    check_not_quoted_after(first)


def scenario_control_stale_caret_success():
    # A string the override declines and the grammar runs, with the DDL on its
    # second line. Nothing clears the record after it (an error would: DuckDB
    # then calls the parse stub, which does), so this is the record a later
    # query on the thread finds.
    check_not_quoted_after(lambda on: on.execute("SELECT 1;\n" + DDL))


def scenario_control_stale_caret_rewrite():
    # One statement, on its second line, that the override rewrites and runs.
    check_not_quoted_after(lambda on: on.execute("\n" + DDL))


def scenario_control_stale_caret_parse_error():
    # A query that fails to parse before the override handles any of it, with
    # the DDL on its second line.
    check_not_quoted_after(lambda on: error_of(on, "SELEC 1;\n" + DDL))


def scenario_control_stale_caret_runtime_error():
    bad = DDL.replace("DIMENSIONS", "DIMENSIONZ", 1)
    off, on = connect(), connect()
    try:
        off.execute("SET allow_parser_override_extension = 'DEFAULT'")
        # The DDL starts on line 2 here, and is never parsed: the SELECT fails first.
        first = error_of(on, "SELECT * FROM missing_table;\n" + bad)
        msg = error_of(off, bad)
    finally:
        off.close()
        on.close()
    assert first is not None and "missing_table" in first, f"setup: unexpected first error: {first}"
    assert msg is not None, "an invalid DDL must fail"
    assert "LINE 2:" not in msg, f"must not quote another query; got: {msg}"


STALE_TABLE = "CREATE TABLE orders (id INTEGER, a INTEGER, b INTEGER, ab INTEGER)"
STALE_DDL = (
    "SELECT 1; CREATE SEMANTIC VIEW s AS TABLES (o AS orders PRIMARY KEY (id)) "
    "DIMENSIONS (o.r AS o.a+o.b)"
)


def run_after_record(record, sql):
    """Leave `record` as the recorded query (the override declines it, the
    grammar runs it, nothing clears it), then run `sql` on another database
    whose override is off. Returns that database's error (or None) and its
    GET_DDL of `s` (or None)."""
    off, on = connect(), connect()
    try:
        on.execute(STALE_TABLE)
        off.execute(STALE_TABLE)
        off.execute("SET allow_parser_override_extension = 'DEFAULT'")
        on.execute(record)
        msg = error_of(off, sql)
        ddl = None if msg else off.execute("SELECT get_ddl('SEMANTIC_VIEW', 's')").fetchone()[0]
    finally:
        off.close()
        on.close()
    return msg, ddl


def scenario_control_stale_source_gap():
    # Every token of the second query, its `)` included, is at the same offset
    # in the record; only the bytes between them differ.
    msg, ddl = run_after_record(STALE_DDL, STALE_DDL.replace("+o.b", "    "))
    assert msg is None, f"the CREATE must run; got: {msg}"
    assert "o.a+o.b" not in ddl and "o.r AS o.a\n" in ddl, f"must store its own body; got: {ddl}"


def scenario_control_stale_source_prefix():
    record = STALE_DDL.replace("o.a+o.b", "o.ab")
    msg, ddl = run_after_record(record, record.replace("o.ab)", "o.a )"))
    assert msg is None, f"the CREATE must run; got: {msg}"
    assert "o.ab" not in ddl and "o.r AS o.a\n" in ddl, f"must store its own body; got: {ddl}"


def scenario_control_stale_source_newline():
    record = STALE_DDL.replace("o.a+o.b", "o.a+\no.b")
    msg, ddl = run_after_record(record, record.replace("o.a+\no.b", "o.a+ o.b"))
    assert msg is None, f"the CREATE must run; got: {msg}"
    assert "o.a+\no.b" not in ddl and "o.a+ o.b" in ddl, f"must store its own body; got: {ddl!r}"


def scenario_control_stale_source_tail():
    record = STALE_DDL + " METRICS (o.m AS SUM(o.b))"
    msg, _ = run_after_record(record, record[: record.index(" (o.m")])
    assert msg is not None and "Expected '(' after clause keyword 'METRICS'" in msg, (
        f"expected the METRICS error; got: {msg}"
    )
    assert "SUM(o.b)" not in msg, f"must not quote the record's tail; got: {msg}"


def scenario_control_reserved_word_name():
    # TECH-DEBT #98: the override accepts an unquoted reserved word as a view
    # name; the grammar's ColId, like DuckDB, refuses it. Where the override
    # declines the statement, the stub answers. Quoted, it works on both routes.
    con = connect()
    try:
        con.execute("CREATE TABLE t (id INTEGER, r VARCHAR)")
        con.execute(
            "CREATE SEMANTIC VIEW select AS TABLES (t AS t PRIMARY KEY (id)) DIMENSIONS (t.r AS t.r)"
        )
        multi = error_of(con, "DESCRIBE SEMANTIC VIEW select; SELECT 1")
        quoted_multi = error_of(con, 'DESCRIBE SEMANTIC VIEW "select"; SELECT 1')
        con.execute("SET allow_parser_override_extension = 'DEFAULT'")
        off = error_of(con, "DESCRIBE SEMANTIC VIEW select")
        quoted_off = error_of(con, 'DESCRIBE SEMANTIC VIEW "select"')
    finally:
        con.close()
    assert multi is not None and "one statement per call" in multi, f"got: {multi}"
    assert off is not None and "parser_override is not active" in off, f"got: {off}"
    assert quoted_multi is None and quoted_off is None, f"got: {quoted_multi} / {quoted_off}"


# The parse stub still answers for a statement the grammar does not take, which
# on 2.0 means an unquoted reserved-word view name (TECH-DEBT #98). Its
# multi-statement diagnosis reads a per-thread record of the override's last
# query; these check that a record left by another query never turns the
# override-off message into "one statement per call".
RW_SETUP = (
    "CREATE TABLE t (id INTEGER, r VARCHAR); "
    "CREATE SEMANTIC VIEW select AS TABLES (t AS t PRIMARY KEY (id)) DIMENSIONS (t.r AS t.r)"
)
RW_DDL = "DESCRIBE SEMANTIC VIEW select"
MULTI_HINT = "one statement per call"
OFF_HINT = "parser_override is not active"


def check_not_blamed_after(first_sql, expect_first):
    off, on = connect(), connect()
    try:
        on.execute(RW_SETUP)
        off.execute("SET allow_parser_override_extension = 'DEFAULT'")
        first = error_of(on, first_sql)
        msg = error_of(off, RW_DDL)
    finally:
        off.close()
        on.close()
    assert expect_first(first), f"setup: unexpected first result: {first}"
    assert msg is not None and OFF_HINT in msg, f"must keep its own message; got: {msg}"
    assert MULTI_HINT not in msg, f"must not reuse another query's record; got: {msg}"


def scenario_control_stale_other_database():
    check_not_blamed_after(
        "SELECT 1; " + RW_DDL, lambda first: first is not None and MULTI_HINT in first
    )


def scenario_control_stale_after_success():
    check_not_blamed_after(RW_DDL + "; -- described", lambda first: first is None)


def scenario_control_stale_after_typo():
    check_not_blamed_after("SELEC 1; " + RW_DDL, lambda first: first is not None)


def scenario_control_stale_after_runtime_error():
    check_not_blamed_after(
        "SELECT * FROM missing_table; " + RW_DDL,
        lambda first: first is not None and "missing_table" in first,
    )


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
        for mode in ("FALLBACK", "DEFAULT"):
            con.execute(f"SET allow_parser_override_extension = '{mode}'")
            parts = parse_seconds(con, drops) + parse_seconds(con, filler)
            both = parse_seconds(con, filler + "; " + drops)
            assert both < 2.5 * parts, (
                f"{mode}: the string took {both:.2f}s, its parts {parts:.2f}s"
            )
    finally:
        con.close()


SCENARIOS = [
    scenario_ddl_after_other_statements,
    scenario_ddl_before_other_statement,
    scenario_ddl_after_read_call,
    scenario_error_shows_its_own_line,
    scenario_ddl_after_describe,
    scenario_caret_after_read_call,
    scenario_ddl_after_read_with_semicolon,
    scenario_control_override_disabled,
    scenario_control_single_statement,
    scenario_control_stale_caret_query,
    scenario_control_stale_caret_success,
    scenario_control_stale_caret_rewrite,
    scenario_control_stale_caret_parse_error,
    scenario_control_stale_source_gap,
    scenario_control_stale_source_prefix,
    scenario_control_stale_source_newline,
    scenario_control_stale_source_tail,
    scenario_control_reserved_word_name,
    scenario_control_stale_other_database,
    scenario_control_stale_after_success,
    scenario_control_stale_after_typo,
    scenario_control_stale_after_runtime_error,
    scenario_control_stale_caret_runtime_error,
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
