#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.1"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/lod_m2m_references_and_window_members.py

Demonstrates what v0.20.0 adds, each as in Snowflake:
  - level-of-detail (LOD) expressions: FIXED DIMENSIONS computes a value at
    its own grain (orders per customer), INCLUDE DIMENSIONS at the query's
    dimensions plus some (the average customer's revenue in each region), and
    EXCLUDE DIMENSIONS at the query's dimensions minus some (a category's share
    of its region's revenue)
  - direct many-to-many relationships: a relationship to a non-key column
    counts each order once under each channel its promotion code runs on
  - dimension references: an order's region is read through its customer
  - window functions in facts and dimensions: a sale's rank among all the
    sales, also in a query filtered to one category
  - an unnamed relationship, which gets a generated SYS_RELATIONSHIP_ name

Every number printed is also asserted, so the script fails if a result drifts.
"""

from decimal import Decimal

import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")


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


# ============================================================
# 1. LOD expressions: FIXED, INCLUDE and EXCLUDE DIMENSIONS
# ============================================================

print("=== 1. Level-of-detail expressions ===")
con.execute("""
CREATE TABLE customers (customer_id INTEGER, name VARCHAR, region VARCHAR);
INSERT INTO customers VALUES
    (1, 'Alice', 'North'), (2, 'Bob', 'North'), (3, 'Carol', 'South'), (4, 'Dan', 'South');
CREATE TABLE orders (order_id INTEGER, customer_id INTEGER, category VARCHAR, amount DECIMAL(10,2));
INSERT INTO orders VALUES
    (101, 1, 'Electronics', 400), (102, 1, 'Electronics', 200),
    (103, 1, 'Clothing',    300), (104, 1, 'Clothing',    100),
    (105, 2, 'Electronics', 200), (106, 2, 'Clothing',    100),
    (107, 3, 'Electronics', 300), (108, 3, 'Electronics', 200),
    (109, 3, 'Clothing',    100), (110, 4, 'Electronics', 200);
""")
# The relationship has no name: it gets a generated SYS_RELATIONSHIP_<id>.
con.execute("""
CREATE SEMANTIC VIEW sales
TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders)
RELATIONSHIPS (orders(customer_id) REFERENCES customers)
FACTS (
    orders.order_amount AS amount,
    orders_per_customer AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id)
)
DIMENSIONS (
    customers.customer_id AS customer_id,
    customers.region      AS region,
    orders.order_id       AS order_id,
    orders.category       AS category
)
METRICS (
    orders.total_revenue    AS SUM(amount),
    avg_orders_per_customer AS AVG(orders_per_customer),
    avg_customer_revenue    AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id)),
    pct_of_region_revenue   AS SUM(orders.order_amount)
                               / AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (orders.category))
)
""")

rel = show(
    "The unnamed relationship's generated name",
    "SELECT DISTINCT object_name FROM (DESCRIBE SEMANTIC VIEW sales) WHERE object_kind = 'RELATIONSHIP'",
)
assert len(rel) == 1 and rel[0][0].startswith("SYS_RELATIONSHIP_"), rel

by_region = show(
    "Each customer counted once (FIXED), the average customer's revenue (INCLUDE)",
    """SELECT * FROM SEMANTIC_VIEW(sales
           DIMENSIONS customers.region
           METRICS avg_orders_per_customer, avg_customer_revenue
       ) ORDER BY region""",
)
# North: Alice 4 orders / 1000, Bob 2 / 300. South: Carol 3 / 600, Dan 1 / 200.
assert by_region == [("North", 3.0, 650.0), ("South", 2.0, 400.0)], by_region

shares = show(
    "A category's share of its region's revenue (EXCLUDE): North 500 and 800 of 1300",
    """SELECT region, category, round(pct_of_region_revenue, 4) AS share
       FROM SEMANTIC_VIEW(sales
           DIMENSIONS customers.region, orders.category
           METRICS pct_of_region_revenue
       ) ORDER BY region, category""",
)
assert shares == [
    ("North", "Clothing", 0.3846),
    ("North", "Electronics", 0.6154),
    ("South", "Clothing", 0.125),
    ("South", "Electronics", 0.875),
], shares

# ============================================================
# 2. Direct many-to-many relationships
# ============================================================

print("\n=== 2. Direct many-to-many relationships ===")
con.execute("""
CREATE TABLE promo_orders (order_id INTEGER, promo_code VARCHAR, amount DECIMAL(10,2));
INSERT INTO promo_orders VALUES
    (1, 'SPRING', 100), (2, 'SPRING', 40), (3, 'WELCOME', 60), (4, 'NOPE', 30), (5, NULL, 20);
CREATE TABLE promotions (promo_code VARCHAR, channel VARCHAR);
INSERT INTO promotions VALUES
    ('SPRING', 'Web'), ('SPRING', 'Web'), ('SPRING', 'Email'), ('WELCOME', 'Web');
CREATE SEMANTIC VIEW promo_sales
TABLES (o AS promo_orders PRIMARY KEY (order_id), p AS promotions)
RELATIONSHIPS (order_promotions AS o(promo_code) REFERENCES p(promo_code))
DIMENSIONS (p.channel AS channel)
METRICS (o.revenue AS SUM(amount), o.order_count AS COUNT(*))
""")
by_channel = show(
    "Each order once per channel its code runs on",
    """SELECT * FROM SEMANTIC_VIEW(promo_sales
           DIMENSIONS p.channel METRICS o.revenue, o.order_count
       ) ORDER BY channel NULLS LAST""",
)
assert by_channel == [
    ("Email", Decimal("140.00"), 2),
    ("Web", Decimal("200.00"), 3),
    (None, Decimal("50.00"), 2),
], by_channel

web = show(
    "Filtered on the channel, each order counted once",
    "SELECT * FROM SEMANTIC_VIEW(promo_sales METRICS o.revenue, o.order_count WHERE p.channel = 'Web')",
)
assert web == [(Decimal("200.00"), 3)], web

# ============================================================
# 3. Dimension references
# ============================================================

print("\n=== 3. Dimension references ===")
con.execute("""
CREATE TABLE ref_regions (region_id INTEGER, region_name VARCHAR);
INSERT INTO ref_regions VALUES (1, 'Europe'), (2, 'Americas');
CREATE TABLE ref_customers (customer_id INTEGER, name VARCHAR, region_id INTEGER);
INSERT INTO ref_customers VALUES (10, 'Ann', 1), (20, 'Bob', 2), (30, 'Cid', NULL);
CREATE TABLE ref_orders (order_id INTEGER, customer_id INTEGER, amount DECIMAL(10,2));
INSERT INTO ref_orders VALUES (1, 10, 30), (2, 10, 20), (3, 20, 50), (4, 30, 15);
CREATE SEMANTIC VIEW regional_orders
TABLES (
    region   AS ref_regions   PRIMARY KEY (region_id),
    customer AS ref_customers PRIMARY KEY (customer_id),
    orders   AS ref_orders    PRIMARY KEY (order_id)
)
RELATIONSHIPS (
    customer_region AS customer(region_id) REFERENCES region,
    order_customer  AS orders(customer_id) REFERENCES customer
)
DIMENSIONS (
    region.region_name       AS region_name,
    customer.customer_region AS region.region_name,
    orders.order_region      AS customer.customer_region
)
METRICS (orders.revenue AS SUM(amount))
""")
by_order_region = show(
    "Orders grouped by their customer's region, a reference to a reference",
    """SELECT * FROM SEMANTIC_VIEW(regional_orders
           DIMENSIONS orders.order_region METRICS orders.revenue
       ) ORDER BY 1 NULLS LAST""",
)
assert by_order_region == [
    ("Americas", Decimal("50.00")),
    ("Europe", Decimal("50.00")),
    (None, Decimal("15.00")),
], by_order_region

# ============================================================
# 4. Window functions in facts and dimensions
# ============================================================

print("\n=== 4. Window facts and dimensions ===")
con.execute("""
CREATE TABLE win_sales (id INTEGER, category VARCHAR, units INTEGER, revenue DECIMAL(10,2));
INSERT INTO win_sales VALUES
    (1, 'A', 5, 50), (2, 'B', 2, 30), (3, 'A', 8, 70), (4, 'B', 1, 10), (5, 'A', 3, 20);
CREATE SEMANTIC VIEW ranked_sales
TABLES (sales AS win_sales PRIMARY KEY (id))
FACTS (sales.revenue_by_category AS SUM(revenue) OVER (PARTITION BY category))
DIMENSIONS (
    sales.sale_id    AS id,
    sales.category   AS category,
    sales.units_rank AS ROW_NUMBER() OVER (ORDER BY units DESC)
)
METRICS (sales.revenue AS SUM(revenue), sales.max_cat_rev AS MAX(revenue_by_category))
""")
ranks = show(
    "Category A's sales keep their rank among all the sales",
    """SELECT * FROM SEMANTIC_VIEW(ranked_sales
           DIMENSIONS sales.sale_id, sales.units_rank WHERE sales.category = 'A'
       ) ORDER BY sale_id""",
)
assert ranks == [(1, 2), (3, 1), (5, 3)], ranks

top2 = show(
    "Revenue of the two sales with the most units",
    "SELECT * FROM SEMANTIC_VIEW(ranked_sales METRICS sales.revenue WHERE sales.units_rank <= 2)",
)
assert top2 == [(Decimal("120.00"),)], top2

cat = show(
    "A metric over a window fact, filtered to category B",
    "SELECT * FROM SEMANTIC_VIEW(ranked_sales METRICS sales.max_cat_rev WHERE sales.category = 'B')",
)
assert cat == [(Decimal("40.00"),)], cat

print("\nAll v0.20.0 examples passed.")
