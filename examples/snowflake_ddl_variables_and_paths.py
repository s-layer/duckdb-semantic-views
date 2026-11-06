#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/snowflake_ddl_variables_and_paths.py

Demonstrates what v0.18.0 adds:
  - Snowflake DDL pasted as written: no AS before TABLES, LABELS before AS,
    Cortex Analyst metadata (SAMPLE_VALUES, IS_ENUM, AI_SQL_GENERATION, ...)
    accepted, stored and shown by DESCRIBE
  - metrics with different USING paths queried together: departures and
    arrivals by city, each through its own relationship, outer-merged
  - a metric written over another metric on its own table
  - a logical table defined by a query
  - view variables: declared with defaults, set per query
  - CREATE OR ALTER, which keeps the view's tags, and
    verify_semantic_view_yaml, which checks a YAML definition and creates
    nothing
  - ALTER ... SET TAG and paging with SHOW SEMANTIC VIEWS ... LIMIT n FROM

Every number printed is also asserted, so the script fails if a result drifts.
"""

import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")


def rows(sql):
    """Run a query and return its rows."""
    return con.execute(sql).fetchall()


def show(label, sql):
    """Print a query's column names and rows, and return the rows."""
    result = con.execute(sql)
    names = [d[0] for d in result.description]
    data = result.fetchall()
    print(f"\n  {label}")
    print("    " + " | ".join(names))
    for row in data:
        print("    " + " | ".join(str(v) for v in row))
    return data


def expect_error(label, sql, needle):
    """Run a statement that must fail, print the first line of its error."""
    try:
        rows(sql)
    except duckdb.Error as exc:
        first_line = str(exc).strip().splitlines()[0]
        assert needle in first_line, first_line
        print(f"\n  {label}")
        print(f"    -> {first_line[:150]}")
    else:
        raise SystemExit(f"expected an error from: {sql}")


# --- Data ---------------------------------------------------------------

con.execute("""
    CREATE TABLE airports (code VARCHAR, city VARCHAR);
    INSERT INTO airports VALUES ('LHR', 'London'), ('CDG', 'Paris'), ('OSL', 'Oslo');

    CREATE TABLE flights (id INTEGER, dep VARCHAR, arr VARCHAR, seats INTEGER);
    INSERT INTO flights VALUES
        (1, 'LHR', 'CDG', 180), (2, 'LHR', 'CDG', 150),
        (3, 'CDG', 'LHR', 200), (4, 'LHR', 'OSL', 120);

    CREATE TABLE customers (id INTEGER, name VARCHAR);
    CREATE TABLE addresses (customer_id INTEGER, country VARCHAR);
    INSERT INTO customers VALUES (1, 'Ana'), (2, 'Ben'), (3, 'Cleo');
    INSERT INTO addresses VALUES (1, 'PT'), (2, 'UK'), (3, 'PT');

    CREATE TABLE orders (id INTEGER, customer_id INTEGER, amount INTEGER);
    INSERT INTO orders VALUES (10, 1, 900), (11, 1, 2500), (12, 2, 400), (13, 3, 3100);
""")

print("=" * 72)
print("1. Snowflake DDL pastes as written")
print("=" * 72)

# No AS before TABLES, LABELS = (FILTER) between the name and AS, and the
# Cortex Analyst clauses. The metadata changes no number.
con.execute("""
    CREATE SEMANTIC VIEW flight_stats
      TABLES (
        f AS flights PRIMARY KEY (id),
        a AS airports PRIMARY KEY (code)
      )
      RELATIONSHIPS (
        departure AS f (dep) REFERENCES a,
        arrival   AS f (arr) REFERENCES a
      )
      FACTS (
        f.is_wide LABELS = (FILTER) AS f.seats >= 180
      )
      DIMENSIONS (
        a.city AS a.city
          COMMENT = 'Airport city'
          SAMPLE_VALUES ('London', 'Paris', 'Oslo')
          IS_ENUM
      )
      METRICS (
        f.flight_count AS COUNT(*),
        f.departures USING (departure) AS f.flight_count,
        f.arrivals   USING (arrival)   AS f.flight_count
      )
      COMMENT = 'Departures and arrivals per city'
      AI_SQL_GENERATION 'Round seat counts to whole numbers.'
""")

kinds = show(
    "DESCRIBE keeps the Cortex Analyst metadata:",
    """SELECT object_kind, object_name, property, property_value
       FROM describe_semantic_view('flight_stats')
       WHERE property IN ('SAMPLE_VALUES', 'IS_ENUM', 'LABELS')
          OR object_kind = 'CUSTOM_INSTRUCTIONS'
       ORDER BY ALL""",
)
assert {r[2] for r in kinds} >= {"SAMPLE_VALUES", "IS_ENUM", "LABELS"}, kinds
assert any(r[0] == "CUSTOM_INSTRUCTIONS" for r in kinds), kinds

print()
print("=" * 72)
print("2. Metrics with different USING paths, queried together")
print("=" * 72)

# London: 3 departures, 1 arrival; Paris: 1 and 2; Oslo has no departures,
# so it shows NULL there rather than vanishing.
data = show(
    "Departures and arrivals by city:",
    """SELECT * FROM SEMANTIC_VIEW(flight_stats DIMENSIONS city METRICS departures, arrivals)
       ORDER BY city""",
)
assert data == [("London", 3, 1), ("Oslo", None, 1), ("Paris", 1, 2)], data

data = show(
    "The same query in standard SQL, filtered to wide-body flights:",
    """SELECT city, AGG(departures) AS departures FROM flight_stats
       WHERE is_wide GROUP BY city ORDER BY city""",
)
assert data == [("London", 1), ("Paris", 1)], data

expect_error(
    "A metric with no USING cannot be grouped by a role-played city:",
    "SELECT * FROM SEMANTIC_VIEW(flight_stats DIMENSIONS city METRICS flight_count)",
    "ambiguous",
)

print()
print("=" * 72)
print("3. A table defined by a query, and view variables")
print("=" * 72)

con.execute("""
    CREATE SEMANTIC VIEW order_stats
      TABLES (
        o AS orders PRIMARY KEY (id),
        customer_info AS (
          SELECT * FROM customers JOIN addresses ON customers.id = addresses.customer_id
        ) PRIMARY KEY (id) COMMENT = 'Customers with their address'
      )
      RELATIONSHIPS (order_customer AS o (customer_id) REFERENCES customer_info)
      VARIABLES (
        min_amount INTEGER DEFAULT 1000 COMMENT 'smallest order counted as large',
        fx_rate DOUBLE := 1.0
      )
      DIMENSIONS (
        customer_info.country AS customer_info.country,
        o.size AS CASE WHEN o.amount >= min_amount THEN 'large' ELSE 'small' END
      )
      METRICS (
        o.revenue AS SUM(o.amount * fx_rate)
      )
""")

data = show(
    "Revenue by country and size, with the defaults (min_amount 1000):",
    """SELECT * FROM SEMANTIC_VIEW(order_stats DIMENSIONS country, size METRICS revenue)
       ORDER BY ALL""",
)
assert data == [
    ("PT", "large", 5600.0),
    ("PT", "small", 900.0),
    ("UK", "small", 400.0),
], data

data = show(
    "The same with min_amount = 3000 and revenue in another currency:",
    """SELECT * FROM SEMANTIC_VIEW(
         order_stats DIMENSIONS country, size METRICS revenue
         VARIABLES min_amount = 3000, fx_rate => 0.5
       ) ORDER BY ALL""",
)
assert data == [
    ("PT", "large", 1550.0),
    ("PT", "small", 1700.0),
    ("UK", "small", 200.0),
], data

data = show(
    "The table function takes them as a struct:",
    """SELECT * FROM semantic_view('order_stats', dimensions := ['size'],
         metrics := ['revenue'], variables := {'min_amount': 3000}) ORDER BY ALL""",
)
assert data == [("large", 3100.0), ("small", 3800.0)], data

print()
print("=" * 72)
print("4. CREATE OR ALTER, tags and verify_semantic_view_yaml")
print("=" * 72)

con.execute("ALTER SEMANTIC VIEW order_stats SET TAG owner = 'finance'")

# Restate the view without the variables and with a COMMENT: the statement
# is the whole definition, but the view's tags stay.
con.execute("""
    CREATE OR ALTER SEMANTIC VIEW order_stats
      TABLES (o AS orders PRIMARY KEY (id))
      DIMENSIONS (o.customer_id AS o.customer_id)
      METRICS (o.revenue AS SUM(o.amount))
      COMMENT = 'Revenue per customer'
""")

tags = rows("""SELECT property_value FROM describe_semantic_view('order_stats')
               WHERE object_kind = '' AND property = 'TAGS'""")
print(f"\n  Tags after CREATE OR ALTER: {tags[0][0]}")
assert "finance" in tags[0][0], tags

data = show(
    "The altered view:",
    "SELECT * FROM SEMANTIC_VIEW(order_stats DIMENSIONS customer_id METRICS revenue) ORDER BY 1",
)
assert data == [(1, 3400), (2, 400), (3, 3100)], data

message = rows("""SELECT * FROM verify_semantic_view_yaml('order_check', $$
tables:
  - alias: o
    table: orders
    pk_columns: [id]
dimensions:
  - name: customer_id
    expr: o.customer_id
    source_table: o
metrics: []
$$)""")[0][0]
print(f"\n  verify_semantic_view_yaml: {message}")
assert message.startswith("YAML file is valid"), message
assert rows("SELECT count(*) FROM (SHOW SEMANTIC VIEWS LIKE 'order_check')") == [(0,)]

names = [r[0] for r in rows("SELECT name FROM (SHOW SEMANTIC VIEWS LIMIT 1 FROM 'flight_stats')")]
print(f"\n  SHOW SEMANTIC VIEWS LIMIT 1 FROM 'flight_stats': {names}")
assert names == ["order_stats"], names

print("\nAll assertions passed.")
