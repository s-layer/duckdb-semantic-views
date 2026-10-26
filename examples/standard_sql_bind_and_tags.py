#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/standard_sql_bind_and_tags.py

Demonstrates what v0.17.0 adds:
  - querying a semantic view by name with standard SQL, as in Snowflake:
    `GROUP BY` picks the dimensions, `AGG(metric)` the metrics, `WHERE`
    filters before aggregation and `HAVING` after it
  - the same query inside a CTE, a join, `CREATE TABLE AS`, and from Python
    through `con.sql()`
  - what the query form refuses: a metric outside `AGG`, a dimension missing
    from `GROUP BY`, `COUNT(*)` alone
  - `CREATE SEMANTIC VIEW` binding every member, so a member DuckDB cannot
    bind refuses the CREATE instead of failing at the first query
  - Snowflake-style tags on tables, members and the view: shown by DESCRIBE,
    and written back by GET_DDL and the YAML export

Every number printed is also asserted, so the script fails if a result drifts.
"""

import json

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


# ============================================================
# Setup
#
#   customers <--customer_id-- orders
# ============================================================

con.execute("CREATE TABLE customers (id INTEGER, name VARCHAR, region VARCHAR)")
con.execute("""
    INSERT INTO customers VALUES
        (1, 'Acme',    'East'),
        (2, 'Globex',  'East'),
        (3, 'Initech', 'West')
""")
con.execute("""
    CREATE TABLE orders (
        id INTEGER, customer_id INTEGER, channel VARCHAR, amount INTEGER
    )
""")
con.execute("""
    INSERT INTO orders VALUES
        (1, 1, 'web',   100),
        (2, 1, 'store',  50),
        (3, 2, 'web',    30),
        (4, 3, 'web',    80),
        (5, 3, 'store',  20)
""")

# Tags sit where Snowflake puts them: after a member's other annotations,
# after a table's, and after the view's last clause. No CREATE TAG is needed.
con.execute("""
    CREATE SEMANTIC VIEW sales AS
    TABLES (
        o AS orders    PRIMARY KEY (id) WITH TAG (governance.pii = 'none'),
        c AS customers PRIMARY KEY (id)
    )
    RELATIONSHIPS (order_customer AS o(customer_id) REFERENCES c)
    FACTS (
        o.amt AS o.amount
    )
    DIMENSIONS (
        c.region  AS c.region  COMMENT = 'Customer region'
            WITH TAG (s_layer_kind = 'dimension', s_layer_label = 'Region'),
        o.channel AS o.channel WITH TAG (s_layer_kind = 'dimension')
    )
    METRICS (
        o.revenue     AS SUM(o.amount) WITH TAG (s_layer_kind = 'metric'),
        o.order_count AS COUNT(*)
    )
    COMMENT = 'Orders by region and channel'
    WITH TAG (owner = 'analytics')
""")


# ============================================================
# 1. Standard SQL against the view
# ============================================================

print("\n1. Standard SQL against the view")

by_region = show(
    "GROUP BY picks the dimensions; AGG() picks the metrics",
    "SELECT region, AGG(revenue) AS revenue, AGG(order_count) AS orders "
    "FROM sales GROUP BY region ORDER BY region",
)
assert by_region == [("East", 180, 3), ("West", 100, 2)], by_region

# The same query in Snowflake's construct form, for comparison.
construct = rows(
    "SELECT * FROM SEMANTIC_VIEW(sales DIMENSIONS region METRICS revenue, order_count) "
    "ORDER BY region"
)
assert construct == by_region, construct

web_only = show(
    "WHERE filters rows before the metrics are computed",
    "SELECT region, AGG(revenue) AS web_revenue FROM sales "
    "WHERE channel = 'web' GROUP BY region ORDER BY region",
)
assert web_only == [("East", 130), ("West", 80)], web_only

big = show(
    "HAVING filters the result after aggregation",
    "SELECT region, channel, AGG(revenue) AS revenue FROM sales "
    "GROUP BY ALL HAVING AGG(revenue) >= 80 ORDER BY region, channel",
)
assert big == [("East", "web", 130), ("West", "web", 80)], big

ratio = show(
    "Expressions around AGG() are computed on the result",
    "SELECT region, AGG(revenue) / AGG(order_count) AS avg_order "
    "FROM sales GROUP BY region ORDER BY region",
)
assert ratio == [("East", 60.0), ("West", 50.0)], ratio

adhoc = show(
    "Any other aggregate is an ad-hoc metric over the view's facts",
    "SELECT channel, MAX(amt) AS largest FROM sales GROUP BY channel ORDER BY channel",
)
assert adhoc == [("store", 50), ("web", 100)], adhoc


# ============================================================
# 2. It goes anywhere a query goes
# ============================================================

print("\n2. Anywhere a query goes")

cte = show(
    "In a CTE, joined with an ordinary table",
    """
    WITH r AS (SELECT region, AGG(revenue) AS rev FROM sales GROUP BY region)
    SELECT c.name, r.rev
    FROM customers c JOIN r USING (region)
    ORDER BY c.name
    """,
)
assert cte == [("Acme", 180), ("Globex", 180), ("Initech", 100)], cte

con.execute(
    "CREATE TABLE revenue_by_channel AS "
    "SELECT channel, AGG(revenue) AS revenue FROM sales GROUP BY channel"
)
ctas = show(
    "CREATE TABLE ... AS stores the result",
    "SELECT * FROM revenue_by_channel ORDER BY channel",
)
assert ctas == [("store", 70), ("web", 210)], ctas

rel = con.sql("SELECT region, AGG(order_count) AS n FROM sales GROUP BY region")
from_python = sorted(rel.fetchall())
print("\n  From Python, con.sql() returns a relation over the same query")
print(f"    {from_python}")
assert from_python == [("East", 3), ("West", 2)], from_python


# ============================================================
# 3. What the query form refuses
# ============================================================

print("\n3. Refused, as in Snowflake")

expect_error(
    "A metric must be passed to AGG()",
    "SELECT region, revenue FROM sales GROUP BY region",
    "sales",
)
expect_error(
    "A selected dimension must be in GROUP BY",
    "SELECT region, channel, AGG(revenue) FROM sales GROUP BY region",
    "sales",
)
expect_error(
    "COUNT(*) alone names no table to count; the view's order_count metric does",
    "SELECT region, COUNT(*) FROM sales GROUP BY region",
    "To count rows",
)


# ============================================================
# 4. CREATE binds every member
# ============================================================

print("\n4. CREATE SEMANTIC VIEW binds every member")

expect_error(
    "A member DuckDB cannot bind refuses the CREATE",
    """
    CREATE SEMANTIC VIEW broken AS
    TABLES (o AS orders PRIMARY KEY (id))
    DIMENSIONS (o.fortnight AS date_trunc('fortnight', DATE '2026-01-01'))
    METRICS (o.revenue AS SUM(o.amount))
    """,
    "dimension 'fortnight' does not bind",
)
expect_error(
    "So does a column the table does not have",
    """
    CREATE SEMANTIC VIEW broken AS
    TABLES (o AS orders PRIMARY KEY (id))
    DIMENSIONS (o.region AS o.no_such_column)
    METRICS (o.revenue AS SUM(o.amount))
    """,
    "does not bind",
)
assert rows("SELECT count(*) FROM list_semantic_views() WHERE name = 'broken'") == [(0,)]
print("\n  Neither view was created.")


# ============================================================
# 5. Tags
# ============================================================

print("\n5. Tags")

tag_rows = show(
    "DESCRIBE shows each object's tags as a JSON object",
    "SELECT object_kind, object_name, property_value FROM describe_semantic_view('sales') "
    "WHERE property = 'TAGS' ORDER BY object_kind, object_name",
)
tags = {(kind, name): json.loads(value) for kind, name, value in tag_rows}
assert tags == {
    ("", ""): {"owner": "analytics"},
    ("DIMENSION", "channel"): {"s_layer_kind": "dimension"},
    ("DIMENSION", "region"): {"s_layer_kind": "dimension", "s_layer_label": "Region"},
    ("METRIC", "revenue"): {"s_layer_kind": "metric"},
    ("TABLE", "orders"): {"governance.pii": "none"},
}, tags

kinds = show(
    "Read one tag across members with json_extract_string",
    "SELECT object_name, json_extract_string(property_value, '$.s_layer_kind') AS kind "
    "FROM describe_semantic_view('sales') "
    "WHERE property = 'TAGS' AND object_kind IN ('DIMENSION', 'METRIC') ORDER BY object_name",
)
assert kinds == [("channel", "dimension"), ("region", "dimension"), ("revenue", "metric")], kinds

ddl = rows("SELECT GET_DDL('SEMANTIC_VIEW', 'sales')")[0][0]
assert "WITH TAG (s_layer_kind = 'dimension', s_layer_label = 'Region')" in ddl, ddl
assert ddl.rstrip().endswith("WITH TAG (owner = 'analytics')"), ddl
print("\n  GET_DDL writes the tags back:")
for line in ddl.splitlines():
    if "TAG" in line:
        print("    " + line.strip())

# Replaying GET_DDL under another name gives the same tags.
con.execute(ddl.replace("SEMANTIC VIEW sales ", "SEMANTIC VIEW sales_copy ", 1))
copy_rows = rows(
    "SELECT object_kind, object_name, property_value FROM describe_semantic_view('sales_copy') "
    "WHERE property = 'TAGS' ORDER BY object_kind, object_name"
)
assert copy_rows == tag_rows, copy_rows
print("\n  Replaying it under another name gives the same tags.")

yaml_text = rows("SELECT read_yaml_from_semantic_view('sales')")[0][0]
assert "tags:" in yaml_text and "tag: s_layer_kind" in yaml_text, yaml_text
print("\n  The YAML export carries them in Snowflake's shape:")
start = yaml_text.index("tags:", yaml_text.index("name: region"))
for line in yaml_text[start:].splitlines()[:7]:
    print("    " + line)

print("\nDone.")
