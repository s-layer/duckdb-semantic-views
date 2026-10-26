#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0", "pandas"]
# requires-python = ">=3.10"
# ///
"""
The standard-SQL query form from Python.

`SELECT ... AGG(m) ... FROM sv GROUP BY ...` turns into the SEMANTIC_VIEW(...)
construct when DuckDB's binder cannot bind the name. From Python that binder
also sees replacement scans (a DataFrame or relation read by its variable
name), and a query string may hold several statements. Each scenario runs on a
fresh in-memory database and reports independently:

  con_execute               con.execute() runs the form.
  con_sql                   con.sql() runs the form and returns a relation.
  module_sql                duckdb.sql() runs it on the default connection.
  relation_read_by_name     a relation over the view, read by its variable name
                            in a later query (the replacement-scan path).
  dataframe_named_like_view a pandas DataFrame named like the view keeps its
                            meaning: an aggregate reads the DataFrame, and AGG
                            over it is DuckDB's error, not the view's rows.
  relation_named_like_view  the same for a DuckDB relation.
  multi_statement           the form after another statement in one string
                            (`SET threads = 1; SELECT ...`), and before one.

Run: uv run test/integration/test_standard_sql_python.py (or
`just test-standard-sql-python`).
"""

import sys
from pathlib import Path

import duckdb
import pandas

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_extension_path

EXT_PATH = get_extension_path()

QUERY = "SELECT region, AGG(revenue) AS revenue FROM sales GROUP BY region ORDER BY region"
WANT = [("East", 30.0), ("West", 5.0)]


def connect():
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXT_PATH}'")
    con.execute(
        "CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE); "
        "INSERT INTO orders VALUES (1, 'East', 10), (2, 'East', 20), (3, 'West', 5)"
    )
    con.execute(
        "CREATE SEMANTIC VIEW sales AS "
        "TABLES (o AS orders PRIMARY KEY (id)) "
        "DIMENSIONS (o.region AS o.region) "
        "METRICS (o.revenue AS SUM(o.amount))"
    )
    return con


def scenario_con_execute():
    con = connect()
    try:
        rows = con.execute(QUERY).fetchall()
    finally:
        con.close()
    assert rows == WANT, f"got {rows!r}"


def scenario_con_sql():
    con = connect()
    try:
        rows = con.sql(QUERY).fetchall()
    finally:
        con.close()
    assert rows == WANT, f"got {rows!r}"


def scenario_module_sql():
    con = connect()
    previous = duckdb.default_connection()
    duckdb.set_default_connection(con)
    try:
        rows = duckdb.sql(QUERY).fetchall()
    finally:
        duckdb.set_default_connection(previous)
        con.close()
    assert rows == WANT, f"got {rows!r}"


def scenario_relation_read_by_name():
    con = connect()
    try:
        by_region = con.sql("SELECT region, AGG(revenue) AS revenue FROM sales GROUP BY region")  # noqa: F841
        rows = con.sql("SELECT region, revenue * 2 FROM by_region ORDER BY region").fetchall()
    finally:
        con.close()
    assert rows == [("East", 60.0), ("West", 10.0)], f"got {rows!r}"


def agg_errors(results):
    for how, result in results.items():
        assert isinstance(result, str) and "agg" in result.lower(), (
            f"con.{how}(): AGG over the Python object must be DuckDB's error, not the view's "
            f"rows; got {result!r}"
        )


# DuckDB finds a Python object by its name in the frame that calls execute() or
# sql(), so each call below is made in the frame that defines `sales`.


def scenario_dataframe_named_like_view():
    con = connect()
    results = {}
    try:
        sales = pandas.DataFrame({"region": ["North"], "revenue": [7.0]})  # noqa: F841
        rows = con.sql("SELECT region, SUM(revenue) FROM sales GROUP BY region").fetchall()
        for how, run in (("execute", con.execute), ("sql", con.sql)):
            try:
                results[how] = run(QUERY).fetchall()
            except duckdb.Error as e:
                results[how] = str(e)
    finally:
        con.close()
    assert rows == [("North", 7.0)], f"an aggregate must read the DataFrame; got {rows!r}"
    agg_errors(results)


def scenario_relation_named_like_view():
    con = connect()
    results = {}
    try:
        sales = con.sql("SELECT 'North' AS region, 7.0 AS revenue")  # noqa: F841
        for how, run in (("execute", con.execute), ("sql", con.sql)):
            try:
                results[how] = run(QUERY).fetchall()
            except duckdb.Error as e:
                results[how] = str(e)
    finally:
        con.close()
    agg_errors(results)


def scenario_multi_statement():
    con = connect()
    try:
        after = con.execute("SET threads = 1; " + QUERY).fetchall()
        con.execute(
            "CREATE TABLE totals AS SELECT region, AGG(revenue) AS revenue FROM sales "
            "GROUP BY region; SELECT 1"
        )
        before = con.execute("SELECT * FROM totals ORDER BY region").fetchall()
    finally:
        con.close()
    assert after == WANT, f"the form after another statement; got {after!r}"
    assert before == WANT, f"the form before another statement; got {before!r}"


SCENARIOS = [
    scenario_con_execute,
    scenario_con_sql,
    scenario_module_sql,
    scenario_relation_read_by_name,
    scenario_dataframe_named_like_view,
    scenario_relation_named_like_view,
    scenario_multi_statement,
]


def main() -> int:
    failed = []
    for scenario in SCENARIOS:
        name = scenario.__name__.removeprefix("scenario_")
        try:
            scenario()
            print(f"ok    {name}")
        except Exception as e:
            failed.append(name)
            print(f"FAIL  {name}: {type(e).__name__}: {e}")
    print(f"\n{len(SCENARIOS) - len(failed)}/{len(SCENARIOS)} scenarios passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
