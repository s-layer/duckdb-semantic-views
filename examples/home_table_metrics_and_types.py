#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/home_table_metrics_and_types.py

Demonstrates v0.14.0 features:
  - Every metric is aggregated from its own table (Snowflake's rule): a
    line-item metric counts a line item with no order, a childless order gets
    no row, and a metric's value no longer changes with what it is queried with.
  - SHOW / DESCRIBE report each member's data_type again.
  - A USING metric can aggregate a fact on a role-played table, and each role
    returns its own number; a metric that cannot name a role is rejected.
  - Relationship names match the way DuckDB matches identifiers.
  - CREATE rejects duplicate relationship names and unconnected tables.
  - Semantic-view DDL in a multi-statement string says what is wrong.
"""

import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")


def show(title, sql):
    print(f"\n-- {title}")
    for row in con.execute(sql).fetchall():
        print("  ", row)


def expect_error(title, sql):
    print(f"\n-- {title}")
    try:
        con.execute(sql)
    except duckdb.Error as e:
        print("   error:", str(e).splitlines()[0][:160])
    else:
        raise SystemExit(f"expected an error from: {sql}")


# ============================================================
# Section 1: Every metric from its own table
#
#   orders <--order_id-- line_items
#
# Order 3 has no line items; line item 5 has no order (order_id NULL).
# ============================================================

con.execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, region VARCHAR)")
con.execute("INSERT INTO orders VALUES (1, 'US'), (2, 'EU'), (3, 'EU')")
con.execute("CREATE TABLE line_items (id INTEGER, order_id INTEGER, qty INTEGER)")
con.execute(
    "INSERT INTO line_items VALUES (1, 1, 2), (2, 1, 3), (3, 2, 1), (4, 2, 4), (5, NULL, 7)"
)

con.execute("""
CREATE SEMANTIC VIEW sales AS
  TABLES (
    o  AS orders     PRIMARY KEY (id),
    li AS line_items PRIMARY KEY (id)
  )
  RELATIONSHIPS (li_order AS li(order_id) REFERENCES o)
  DIMENSIONS (o.region AS o.region)
  METRICS (
    o.order_count AS COUNT(*),
    li.total_qty  AS SUM(li.qty),
    li.item_count AS COUNT(*)
  )
""")

# The orphan line item (qty 7) is counted under a NULL region, and order 3
# (no line items) has no row: the metric is computed from line_items itself.
show(
    "total_qty by region, queried alone",
    "SELECT * FROM semantic_view('sales', dimensions := ['region'], metrics := ['total_qty']) "
    "ORDER BY region NULLS LAST",
)

# Queried next to an order-table metric, total_qty keeps the same values; the
# childless EU order shows up through order_count.
show(
    "total_qty and order_count by region",
    "SELECT * FROM semantic_view('sales', dimensions := ['region'], "
    "metrics := ['total_qty', 'order_count']) ORDER BY region NULLS LAST",
)

show(
    "the expanded SQL starts from the metric's own table",
    "SELECT explain_output FROM explain_semantic_view('sales', metrics := ['item_count']) "
    "WHERE explain_output LIKE '%FROM%' LIMIT 1",
)

# ============================================================
# Section 2: data_type is reported again
# ============================================================


def show_types(sql):
    print(f"\n-- {sql} (name, data_type)")
    rel = con.execute(sql)
    cols = [d[0] for d in rel.description]
    for row in rel.fetchall():
        print("  ", row[cols.index("name")], row[cols.index("data_type")])


show_types("SHOW SEMANTIC METRICS IN sales")
show_types("SHOW SEMANTIC DIMENSIONS IN sales")

# ============================================================
# Section 3: Role-played facts
#
#   flights --dep_airport--> airports
#           --"Arr Airport"--> airports
#
# Every flight departs SFO (elevation 13) and arrives at DEN (5434).
# ============================================================

con.execute("CREATE TABLE airports (code VARCHAR PRIMARY KEY, elevation INTEGER)")
con.execute("INSERT INTO airports VALUES ('SFO', 13), ('DEN', 5434)")
con.execute("CREATE TABLE flights (id INTEGER PRIMARY KEY, dep VARCHAR, arr VARCHAR)")
con.execute("INSERT INTO flights VALUES (1, 'SFO', 'DEN'), (2, 'SFO', 'DEN')")

con.execute("""
CREATE SEMANTIC VIEW trips AS
  TABLES (
    f AS flights  PRIMARY KEY (id),
    a AS airports PRIMARY KEY (code)
  )
  RELATIONSHIPS (
    dep_airport   AS f(dep) REFERENCES a,
    "Arr Airport" AS f(arr) REFERENCES a
  )
  FACTS (a.elev AS a.elevation)
  METRICS (
    f.dep_elev USING (DEP_AIRPORT)   AS SUM(elev),
    f.arr_elev USING ("Arr Airport") AS SUM(elev),
    a.any_elev AS SUM(elev)
  )
""")

# USING (DEP_AIRPORT) names the relationship declared as dep_airport: names
# match the way DuckDB matches identifiers. Each role reads its own airport.
show(
    "departure vs arrival elevation, summed over flights",
    "SELECT * FROM semantic_view('trips', metrics := ['dep_elev', 'arr_elev'])",
)

# A metric declared on the role-played table cannot say which relationship
# reaches it, so it is rejected instead of silently using the first one. Here
# the fan-trap check speaks first (each airport has many flights); with
# one-to-one relationships the error names the ambiguity instead.
expect_error(
    "a metric on the role-played table",
    "SELECT * FROM semantic_view('trips', metrics := ['any_elev'])",
)

# ============================================================
# Section 4: Definitions CREATE now rejects
# ============================================================

expect_error(
    "two relationships with the same name (r and R are one name)",
    """CREATE SEMANTIC VIEW bad_names AS
         TABLES (f AS flights PRIMARY KEY (id), a AS airports PRIMARY KEY (code))
         RELATIONSHIPS (r AS f(dep) REFERENCES a, R AS f(arr) REFERENCES a)
         METRICS (f.n AS COUNT(*))""",
)
expect_error(
    "a table no relationship connects",
    """CREATE SEMANTIC VIEW bad_graph AS
         TABLES (f AS flights PRIMARY KEY (id), o AS orders PRIMARY KEY (id))
         METRICS (f.n AS COUNT(*))""",
)

# ============================================================
# Section 5: One statement per call
# ============================================================

expect_error(
    "semantic-view DDL sent together with another statement",
    "SELECT 1; CREATE SEMANTIC VIEW one_stmt AS TABLES (o AS orders PRIMARY KEY (id)) "
    "METRICS (o.n AS COUNT(*))",
)
