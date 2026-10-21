#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/filtering_and_named_filters.py

Demonstrates v0.12.0 pre-aggregation filtering — Snowflake's
`SEMANTIC_VIEW( ... WHERE <predicate> )`, spelled here as the `where_clause`
query parameter:
  - why an outer SQL WHERE cannot express it, with the two numbers side by side
  - filtering on a member that is NOT in the output (the case that motivates it)
  - named filters: `LABELS = (FILTER)` members composed by name
  - parenthesized substitution, so a member's own operator precedence survives
  - the predicate applied inside each grain CTE of a multi-grain query
  - the boundary: a metric in the predicate is rejected

Each section prints both formulations where they differ, so the distinction is
visible rather than asserted.
"""
import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")

# ============================================================
# Setup
#
#   customers <--customer_id-- orders
#
# `orders` is the base table; `customers` is a PARENT, so a metric on it
# aggregates at its own grain (see per_grain_metrics.py).
#
# Two regions, orders spanning 2023 and 2024, so a date filter changes the
# answer rather than merely reordering it.
# ============================================================

con.execute("""
    CREATE TABLE customers (
        id INTEGER, region VARCHAR, segment VARCHAR, balance DECIMAL(10,2)
    )
""")
con.execute("""
    INSERT INTO customers VALUES
        (1, 'East', 'enterprise', 500.00),
        (2, 'East', 'smb',        300.00),
        (3, 'West', 'enterprise', 900.00)
""")

con.execute("""
    CREATE TABLE orders (
        id INTEGER, customer_id INTEGER, amount DECIMAL(10,2),
        channel VARCHAR, ordered_at DATE
    )
""")
con.execute("""
    INSERT INTO orders VALUES
        (1, 1, 100.00, 'web',    DATE '2023-11-02'),
        (2, 1, 250.00, 'web',    DATE '2024-01-15'),
        (3, 2,  50.00, 'retail', DATE '2024-02-20'),
        (4, 3, 400.00, 'retail', DATE '2023-12-10'),
        (5, 3, 150.00, 'web',    DATE '2024-03-05')
""")


def show(label, sql):
    rows = con.execute(sql).fetchall()
    print(f"  {label}")
    for row in rows:
        print("    " + " | ".join(str(v) for v in row))


# ============================================================
# 1. Outer WHERE vs where_clause
#
# An outer WHERE runs on the assembled result, so it can only name columns the
# query returned, and only after the metrics were aggregated. `where_clause`
# runs before aggregation, so it changes what each metric measures.
# ============================================================
con.execute("""
    CREATE SEMANTIC VIEW order_metrics AS
    TABLES (
        o AS orders    PRIMARY KEY (id),
        c AS customers PRIMARY KEY (id)
    )
    RELATIONSHIPS (
        order_customer AS o(customer_id) REFERENCES c
    )
    DIMENSIONS (
        c.region     AS c.region,
        o.channel    AS o.channel,
        o.ordered_at AS o.ordered_at
    )
    METRICS (
        o.revenue     AS SUM(o.amount),
        o.order_count AS COUNT(*)
    )
""")

print("=" * 60)
print("1. Outer WHERE filters AFTER aggregation; where_clause BEFORE")
print("=" * 60)

show(
    "unfiltered revenue by region:",
    """SELECT * FROM semantic_view('order_metrics',
           dimensions := ['region'], metrics := ['revenue']
       ) ORDER BY region""",
)
show(
    "\n  outer WHERE region = 'East' -- drops a row, revenue unchanged:",
    """SELECT * FROM semantic_view('order_metrics',
           dimensions := ['region'], metrics := ['revenue']
       ) WHERE region = 'East'""",
)
show(
    "\n  where_clause on ordered_at -- every revenue is RECOMPUTED:",
    """SELECT * FROM semantic_view('order_metrics',
           dimensions := ['region'], metrics := ['revenue'],
           where_clause := 'ordered_at >= DATE ''2024-01-01'''
       ) ORDER BY region""",
)
print(
    "\n  The date filter is not expressible as an outer WHERE at this grain:\n"
    "  `ordered_at` is not in the output, so there is nothing to filter by.\n"
    "  Adding it to `dimensions` answers a different question -- one row per\n"
    "  day rather than one per region."
)

# ============================================================
# 2. Named filters -- LABELS = (FILTER)
#
# A boolean member that exists to be reused in a predicate rather than
# selected as output. Declared once, composed by name.
# ============================================================
print()
print("=" * 60)
print("2. Named filters: LABELS = (FILTER)")
print("=" * 60)

con.execute("""
    CREATE OR REPLACE SEMANTIC VIEW order_metrics AS
    TABLES (
        o AS orders    PRIMARY KEY (id),
        c AS customers PRIMARY KEY (id)
    )
    RELATIONSHIPS (
        order_customer AS o(customer_id) REFERENCES c
    )
    FACTS (
        o.is_recent  AS o.ordered_at >= DATE '2024-01-01'
            LABELS = (FILTER),
        o.is_large   AS o.amount >= 150.00
            LABELS = (FILTER),
        -- an OR-valued member, to show substitution keeps its grouping
        o.web_or_big AS o.channel = 'web' OR o.amount >= 400.00
            LABELS = (FILTER)
    )
    DIMENSIONS (
        c.region  AS c.region,
        c.segment AS c.segment
    )
    METRICS (
        o.revenue     AS SUM(o.amount),
        o.order_count AS COUNT(*)
    )
""")

show(
    "is_recent AND is_large:",
    """SELECT * FROM semantic_view('order_metrics',
           dimensions := ['region'], metrics := ['revenue', 'order_count'],
           where_clause := 'is_recent AND is_large'
       ) ORDER BY region""",
)

# Each member is substituted parenthesized, so `web_or_big AND is_recent`
# means (web OR big) AND recent -- not web OR (big AND recent), which a bare
# textual splice would have produced.
show(
    "\n  web_or_big AND is_recent -- reads as (web OR big) AND recent:",
    """SELECT * FROM semantic_view('order_metrics',
           dimensions := ['region'], metrics := ['revenue', 'order_count'],
           where_clause := 'web_or_big AND is_recent'
       ) ORDER BY region""",
)
print(
    "\n  Members substitute parenthesized, so a member that binds looser than\n"
    "  its surrounding context keeps its own grouping."
)

# ============================================================
# 3. The predicate inside a multi-grain query
#
# `total_balance` sits on `customers`, `order_count` on `orders` -- two grains.
# The predicate goes into EACH grain CTE, so both metrics see only matching
# rows. On the outer query it would filter the already-combined result, and
# because the grains are joined FULL OUTER that would drop whole groups.
# ============================================================
print()
print("=" * 60)
print("3. The predicate reaches inside each grain of a multi-grain query")
print("=" * 60)

con.execute("""
    CREATE OR REPLACE SEMANTIC VIEW accounts AS
    TABLES (
        o AS orders    PRIMARY KEY (id),
        c AS customers PRIMARY KEY (id)
    )
    RELATIONSHIPS (
        order_customer AS o(customer_id) REFERENCES c
    )
    FACTS (
        c.is_enterprise AS c.segment = 'enterprise' LABELS = (FILTER),
        o.is_recent     AS o.ordered_at >= DATE '2024-01-01' LABELS = (FILTER)
    )
    DIMENSIONS (
        c.region AS c.region
    )
    METRICS (
        o.order_count   AS COUNT(*),
        c.total_balance AS SUM(c.balance)
    )
""")

show(
    "unfiltered (order_count and total_balance are at different grains):",
    """SELECT * FROM semantic_view('accounts',
           dimensions := ['region'], metrics := ['order_count', 'total_balance']
       ) ORDER BY region""",
)
show(
    "\n  where_clause := 'is_enterprise' -- applied within each grain CTE:",
    """SELECT * FROM semantic_view('accounts',
           dimensions := ['region'], metrics := ['order_count', 'total_balance'],
           where_clause := 'is_enterprise'
       ) ORDER BY region""",
)
print(
    "\n  East drops from 3 orders / 800.00 to 2 / 500.00 -- both metrics were\n"
    "  recomputed over the matching rows, each at its own grain. West is\n"
    "  unchanged because its only customer is already enterprise."
)

print("\n  The generated SQL, showing the predicate inside BOTH grain CTEs:")
plan = con.execute(
    """SELECT * FROM explain_semantic_view('accounts',
           dimensions := ['region'], metrics := ['order_count', 'total_balance'],
           where_clause := 'is_enterprise'
       )"""
).fetchall()
expanded = "\n".join(str(r[0]) for r in plan).split("-- DuckDB Plan:")[0]
for line in expanded.splitlines():
    if line.strip():
        print(f"    {line}")

# ============================================================
# 4. The boundaries
#
# Two rejections, for different reasons. Both are reported rather than
# answered wrongly.
# ============================================================
print()
print("=" * 60)
print("4. Boundaries: what the predicate may not do")
print("=" * 60)

for label, query, why in [
    (
        "where_clause := 'order_count > 1'",
        """SELECT * FROM semantic_view('accounts',
               dimensions := ['region'], metrics := ['order_count'],
               where_clause := 'order_count > 1'
           )""",
        "a metric has no value before aggregation runs",
    ),
    (
        "where_clause := 'is_recent' with a parent-grain metric",
        """SELECT * FROM semantic_view('accounts',
               dimensions := ['region'], metrics := ['order_count', 'total_balance'],
               where_clause := 'is_recent'
           )""",
        "reaching the filter's table fans out the parent metric",
    ),
]:
    try:
        con.execute(query).fetchall()
        print(f"  UNEXPECTED: {label} was accepted")
    except duckdb.Error as exc:
        first_line = str(exc).strip().splitlines()[0]
        print(f"\n  {label}")
        print(f"  -> rejected ({why}):")
        print(f"    {first_line[:150]}")

print(
    "\nThe predicate is evaluated before the metrics are computed, so it can\n"
    "name dimensions and facts but not metrics. And the tables it reaches are\n"
    "fan-out checked exactly as a queried dimension's are -- filtering on an\n"
    "`orders` fact while a metric aggregates `customers` would multiply that\n"
    "metric, so it is reported instead. Filter on a member reachable from each\n"
    "metric's table without fanning, or use an outer WHERE when you genuinely\n"
    "mean 'after aggregation'."
)

con.execute("DROP SEMANTIC VIEW accounts")
con.execute("DROP SEMANTIC VIEW order_metrics")
