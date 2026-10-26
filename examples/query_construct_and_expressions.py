#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/query_construct_and_expressions.py

Demonstrates the v0.16.0 query surface:
  - the `SEMANTIC_VIEW(v DIMENSIONS ... METRICS ... FACTS ... WHERE ...)`
    construct, Snowflake's spelling, beside the `semantic_view()` call it
    stands for
  - expressions as query items in DIMENSIONS, METRICS and FACTS, named with
    `AS` aliases
  - WHERE inside the construct: a filter applied before the metrics aggregate
  - prepared-statement parameters in that WHERE, and `params` for
    `semantic_view()`'s `where_clause`
  - clause order as column order, and `column_order` for `semantic_view()`
  - the construct in a join, with a table alias and column aliases
  - SHOW and DESCRIBE used as subqueries and CTEs
  - what an expression item may not do: read a column the view does not
    declare, mix two tables in a dimension, or hold a parameter

Every number printed is also asserted, so the script fails if a result drifts.
"""

from datetime import date
from decimal import Decimal

import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")


def rows(sql, params=None):
    """Run a query and return its rows."""
    return con.execute(sql, params).fetchall() if params else con.execute(sql).fetchall()


def show(label, sql, params=None):
    """Print a query's column names and rows, and return the rows."""
    result = con.execute(sql, params) if params else con.execute(sql)
    names = [d[0] for d in result.description]
    data = result.fetchall()
    print(f"\n  {label}")
    print("    " + " | ".join(names))
    for row in data:
        print("    " + " | ".join(str(v) for v in row))
    return data


def expect_error(label, sql, needle, params=None):
    """Run a query that must fail, print the first line of its error."""
    try:
        rows(sql, params)
    except duckdb.Error as exc:
        first_line = str(exc).strip().splitlines()[0]
        assert needle in first_line, first_line
        print(f"\n  {label}")
        print(f"    -> {first_line[:150]}")
    else:
        raise SystemExit(f"expected an error from: {sql}")


# ============================================================
# Setup
#
#   customers <--customer_id-- orders
#
# `orders` is the base table. `cost` is kept behind a PRIVATE fact: the view
# uses it, but no query may return it.
# ============================================================

con.execute("CREATE TABLE customers (id INTEGER, name VARCHAR, region VARCHAR, tier VARCHAR)")
con.execute("""
    INSERT INTO customers VALUES
        (1, 'Acme',    'East', 'gold'),
        (2, 'Globex',  'East', 'silver'),
        (3, 'Initech', 'West', 'gold')
""")
con.execute("""
    CREATE TABLE orders (
        id INTEGER, customer_id INTEGER, amount DECIMAL(10,2), cost DECIMAL(10,2),
        channel VARCHAR, ordered_at DATE
    )
""")
con.execute("""
    INSERT INTO orders VALUES
        (1, 1, 100.00,  60.00, 'web',    DATE '2023-12-15'),
        (2, 1, 250.00, 150.00, 'web',    DATE '2024-01-10'),
        (3, 2,  80.00,  50.00, 'retail', DATE '2024-02-03'),
        (4, 3, 300.00, 200.00, 'retail', DATE '2024-03-21'),
        (5, 3, 150.00,  90.00, 'web',    DATE '2024-04-02')
""")

con.execute("""
    CREATE SEMANTIC VIEW shop AS
    TABLES (
        o AS orders    PRIMARY KEY (id),
        c AS customers PRIMARY KEY (id)
    )
    RELATIONSHIPS (
        order_customer AS o(customer_id) REFERENCES c
    )
    FACTS (
        o.amount AS o.amount,
        PRIVATE o.unit_cost AS o.cost
    )
    DIMENSIONS (
        c.customer   AS c.name,
        c.region     AS c.region,
        c.tier       AS c.tier,
        o.channel    AS o.channel,
        o.ordered_at AS o.ordered_at
    )
    METRICS (
        o.revenue     AS SUM(o.amount),
        o.order_count AS COUNT(*),
        o.margin      AS SUM(o.amount - o.unit_cost)
    )
""")

# ============================================================
# 1. The SEMANTIC_VIEW(...) construct
#
# The view name and the items are identifiers, not strings, and no comma
# follows the view name. It is the same query as the semantic_view() call.
# ============================================================
print("=" * 60)
print("1. SEMANTIC_VIEW(...) and the semantic_view() call it stands for")
print("=" * 60)

construct = show(
    "SEMANTIC_VIEW(shop DIMENSIONS region METRICS revenue, order_count):",
    """SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS region
           METRICS revenue, order_count
       ) ORDER BY region""",
)
function_call = show(
    "semantic_view('shop', dimensions := [...], metrics := [...]):",
    """SELECT * FROM semantic_view('shop',
           dimensions := ['region'],
           metrics := ['revenue', 'order_count']
       ) ORDER BY region""",
)
assert construct == function_call
assert construct == [("East", Decimal("430.00"), 3), ("West", Decimal("450.00"), 2)]

# ============================================================
# 2. Expressions as query items
#
# An item can be an expression over the view's members. The expansion treats
# it as a member it adds for this one query, so it is grouped, joined and
# checked for fan traps like a declared one.
# ============================================================
print()
print("=" * 60)
print("2. Expression items, named with AS")
print("=" * 60)

by_year = show(
    "a dimension expression and three metric expressions:",
    """SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS date_part('year', ordered_at) AS yr
           METRICS revenue,
                   revenue / order_count AS avg_order,
                   max(amount) AS biggest
       ) ORDER BY yr""",
)
assert [(r[0], r[1], round(float(r[2]), 2), r[3]) for r in by_year] == [
    (2023, Decimal("100.00"), 100.0, Decimal("100.00")),
    (2024, Decimal("780.00"), 195.0, Decimal("300.00")),
]

# A dimension expression uses dimensions and facts of one table: region and
# tier are both on `customers`.
segments = show(
    "region || '/' || tier AS segment:",
    """SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS region || '/' || tier AS segment
           METRICS revenue
       ) ORDER BY segment""",
)
assert segments == [
    ("East/gold", Decimal("350.00")),
    ("East/silver", Decimal("80.00")),
    ("West/gold", Decimal("450.00")),
]

# An aggregate in METRICS is computed over its own table: count(DISTINCT
# customer) counts customers, not orders, whatever else the query asks for.
customers = show(
    "count(DISTINCT customer) beside an orders metric:",
    """SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS region
           METRICS count(DISTINCT customer) AS customers, order_count
       ) ORDER BY region""",
)
assert customers == [("East", 2, 3), ("West", 1, 2)]

# A FACTS expression is a row-level value, one row per order.
facts = show(
    "FACTS amount, round(amount * 0.9, 2) AS after_discount:",
    """SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS channel
           FACTS amount, round(amount * 0.9, 2) AS after_discount
       ) ORDER BY amount""",
)
assert [(r[0], r[1], float(r[2])) for r in facts] == [
    ("retail", Decimal("80.00"), 72.0),
    ("web", Decimal("100.00"), 90.0),
    ("web", Decimal("150.00"), 135.0),
    ("web", Decimal("250.00"), 225.0),
    ("retail", Decimal("300.00"), 270.0),
]

# ============================================================
# 3. WHERE inside the construct
#
# It filters the rows the metrics aggregate, before they aggregate, so each
# number is recomputed. It can name a member that is not in the output.
# ============================================================
print()
print("=" * 60)
print("3. WHERE filters before aggregation")
print("=" * 60)

since_2024 = show(
    "WHERE ordered_at >= DATE '2024-01-01':",
    """SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS region
           METRICS revenue
           WHERE ordered_at >= DATE '2024-01-01'
       ) ORDER BY region""",
)
assert since_2024 == [("East", Decimal("330.00")), ("West", Decimal("450.00"))]

gold = show(
    "WHERE tier = 'gold' (tier is not in the output):",
    """SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS region
           METRICS revenue, order_count
           WHERE tier = 'gold'
       ) ORDER BY region""",
)
assert gold == [("East", Decimal("350.00"), 2), ("West", Decimal("450.00"), 2)]

outer = show(
    "an outer WHERE filters the result instead, after aggregation:",
    """SELECT * FROM SEMANTIC_VIEW(shop DIMENSIONS region METRICS revenue)
       WHERE revenue > 440""",
)
assert outer == [("West", Decimal("450.00"))]

# ============================================================
# 4. Parameters
#
# A prepared-statement parameter in the construct's WHERE is one of the
# statement's own parameters, so a value from application code is bound,
# never spliced into SQL text.
# ============================================================
print()
print("=" * 60)
print("4. Prepared-statement parameters in WHERE")
print("=" * 60)

bound = show(
    "WHERE channel = ? AND ordered_at >= ?, bound from Python:",
    """SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS region
           METRICS revenue
           WHERE channel = ? AND ordered_at >= ?
       ) ORDER BY region""",
    ["web", date(2024, 1, 1)],
)
assert bound == [("East", Decimal("250.00")), ("West", Decimal("150.00"))]

con.execute("""
    PREPARE by_tier AS
    SELECT * FROM SEMANTIC_VIEW(shop DIMENSIONS region METRICS revenue WHERE tier = $1)
    ORDER BY region
""")
silver = show(
    "PREPARE ... WHERE tier = $1; EXECUTE by_tier('silver'):", "EXECUTE by_tier('silver')"
)
assert silver == [("East", Decimal("80.00"))]

# semantic_view() takes the values in `params`; where_clause names them $1, $2.
retail = show(
    "semantic_view(..., where_clause := 'channel = $1', params := row('retail')):",
    """SELECT * FROM semantic_view('shop',
           dimensions := ['region'],
           metrics := ['revenue'],
           where_clause := 'channel = $1',
           params := row('retail')
       ) ORDER BY region""",
)
assert retail == [("East", Decimal("80.00")), ("West", Decimal("300.00"))]

# ============================================================
# 5. Column order
#
# The construct's columns come in the order its clauses are written.
# semantic_view() says the same with column_order.
# ============================================================
print()
print("=" * 60)
print("5. Clause order is column order")
print("=" * 60)

metrics_first = con.execute(
    "SELECT * FROM SEMANTIC_VIEW(shop METRICS revenue AS total DIMENSIONS region AS r) ORDER BY r"
)
names = [d[0] for d in metrics_first.description]
first = metrics_first.fetchall()
print(f"\n  SEMANTIC_VIEW(shop METRICS revenue AS total DIMENSIONS region AS r): {names}")
for row in first:
    print("    " + " | ".join(str(v) for v in row))
assert names == ["total", "r"]

listed = con.execute(
    """SELECT * FROM semantic_view('shop',
           dimensions := ['region AS r'],
           metrics := ['revenue AS total'],
           column_order := ['metrics', 'dimensions']
       ) ORDER BY r"""
)
assert [d[0] for d in listed.description] == names
assert listed.fetchall() == first
print("  semantic_view(..., column_order := ['metrics', 'dimensions']) gives the same columns")

# ============================================================
# 6. The construct composes like a table
#
# It stands wherever a table can: a join, a CTE, a subquery, a view body, with
# a table alias and column aliases.
# ============================================================
print()
print("=" * 60)
print("6. In a join, with a table alias and column aliases")
print("=" * 60)

con.execute("CREATE TABLE targets (region VARCHAR, target DECIMAL(10,2))")
con.execute("INSERT INTO targets VALUES ('East', 400.00), ('West', 500.00)")

against_target = show(
    "targets JOIN SEMANTIC_VIEW(...) AS s(r, rev):",
    """SELECT t.region, s.rev, t.target, s.rev >= t.target AS met
       FROM targets t
       JOIN SEMANTIC_VIEW(shop DIMENSIONS region METRICS revenue) AS s(r, rev)
         ON s.r = t.region
       ORDER BY t.region""",
)
assert against_target == [
    ("East", Decimal("430.00"), Decimal("400.00"), True),
    ("West", Decimal("450.00"), Decimal("500.00"), False),
]

# ============================================================
# 7. SHOW and DESCRIBE as subqueries
#
# Their output is a relation like any other: filter it, aggregate it, put it
# in a CTE.
# ============================================================
print()
print("=" * 60)
print("7. SHOW and DESCRIBE as subqueries")
print("=" * 60)

metric_types = show(
    "SELECT name, data_type FROM (SHOW SEMANTIC METRICS IN shop):",
    "SELECT name, data_type FROM (SHOW SEMANTIC METRICS IN shop) ORDER BY name",
)
assert [r[0] for r in metric_types] == ["margin", "order_count", "revenue"]
assert dict(metric_types)["order_count"] == "BIGINT"

kinds = show(
    "objects per kind, from (DESCRIBE SEMANTIC VIEW shop):",
    """SELECT object_kind, count(DISTINCT object_name) AS objects
       FROM (DESCRIBE SEMANTIC VIEW shop)
       WHERE object_kind IN ('TABLE', 'RELATIONSHIP', 'DIMENSION', 'METRIC')
       GROUP BY object_kind
       ORDER BY object_kind""",
)
assert kinds == [("DIMENSION", 5), ("METRIC", 3), ("RELATIONSHIP", 1), ("TABLE", 2)]

dims = show(
    "WITH d AS (SHOW SEMANTIC DIMENSIONS IN shop) ... per table:",
    """WITH d AS (SHOW SEMANTIC DIMENSIONS IN shop)
       SELECT table_name, string_agg(name, ', ' ORDER BY name) AS dimensions
       FROM d
       GROUP BY table_name
       ORDER BY table_name""",
)
assert dims == [("customers", "customer, region, tier"), ("orders", "channel, ordered_at")]

# ============================================================
# 8. What an expression item may not do
#
# Each is refused, rather than answered from data the view does not expose.
# ============================================================
print()
print("=" * 60)
print("8. Boundaries")
print("=" * 60)

expect_error(
    "a PRIVATE fact cannot be used:",
    "SELECT * FROM SEMANTIC_VIEW(shop DIMENSIONS region METRICS sum(unit_cost) AS cost)",
    "unit_cost",
)
expect_error(
    "nor the raw column under it (an expression reads only the view's members):",
    "SELECT * FROM SEMANTIC_VIEW(shop DIMENSIONS region METRICS sum(amount * 0 + cost) AS cost)",
    "'cost' in 'sum(amount * 0 + cost)' is not a dimension, fact or metric",
)
expect_error(
    "a dimension expression over two tables (region is on customers, channel on orders):",
    "SELECT * FROM SEMANTIC_VIEW(shop DIMENSIONS region || channel AS rc METRICS revenue)",
    "region || channel",
)
expect_error(
    "a parameter outside WHERE:",
    "SELECT * FROM SEMANTIC_VIEW(shop DIMENSIONS region METRICS revenue * ? AS scaled)",
    "parameter",
    [2],
)

print(
    "\nThe margin metric still reads `cost` through its PRIVATE fact; only a\n"
    "query cannot return it."
)
margin = rows("SELECT * FROM SEMANTIC_VIEW(shop DIMENSIONS region METRICS margin) ORDER BY region")
print("    " + str(margin))
assert margin == [("East", Decimal("170.00")), ("West", Decimal("160.00"))]

con.execute("DROP SEMANTIC VIEW shop")
