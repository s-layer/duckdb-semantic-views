#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/asof_range_and_bridge_relationships.py

Demonstrates what v0.19.0 adds:
  - ASOF relationships: each order takes the customer's latest address that
    starts on or before the order date
  - range relationships: each event falls in the period whose half-open
    range holds its timestamp, declared with CONSTRAINT ... DISTINCT RANGE
  - many-to-many relationships through a bridge table: a book with two
    authors counts once per country, never once per bridge row
  - a window metric computed over the query's own result rows, beside an
    aggregate metric and grouped across the bridge
  - an unqualified column in a member expression reads the member's own
    table, even when a joined table has a column of the same name
  - a query that would have to pick one of two paths through a diamond is
    refused, and a metric's USING picks its path

Every number printed is also asserted, so the script fails if a result drifts.
"""

from decimal import Decimal

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


print("=== ASOF relationships ===")

con.execute("""
CREATE TABLE customer_address (cust_id INTEGER, start_date DATE, city VARCHAR);
INSERT INTO customer_address VALUES
  (1, '2024-01-01', 'London'), (1, '2024-06-01', 'Paris'), (2, '2024-03-01', 'Oslo');
CREATE TABLE orders (order_id INTEGER, cust_id INTEGER, order_date DATE, amount DECIMAL(10,2));
INSERT INTO orders VALUES
  (1, 1, '2024-02-10', 100), (2, 1, '2024-07-01', 200),
  (3, 1, '2023-12-01', 50),  (4, 2, '2024-04-01', 70);
""")

con.execute("""
CREATE SEMANTIC VIEW order_cities AS
  TABLES (
    o AS orders           PRIMARY KEY (order_id),
    a AS customer_address PRIMARY KEY (cust_id, start_date)
  )
  RELATIONSHIPS (
    order_address AS o (cust_id, order_date) REFERENCES a (cust_id, ASOF start_date)
  )
  DIMENSIONS (a.city AS city, o.customer AS cust_id)
  METRICS (o.revenue AS SUM(amount), o.order_count AS COUNT(*))
""")

# Order 3 predates every address of its customer, so it keeps its row with a
# NULL city.
by_city = show(
    "Revenue by the city the customer lived in when they ordered",
    "SELECT * FROM SEMANTIC_VIEW(order_cities DIMENSIONS city METRICS revenue, order_count) "
    "ORDER BY city NULLS LAST",
)
assert by_city == [
    ("London", Decimal("100.00"), 1),
    ("Oslo", Decimal("70.00"), 1),
    ("Paris", Decimal("200.00"), 1),
    (None, Decimal("50.00"), 1),
], by_city

# Both tables have a `cust_id` column. The dimension `o.customer AS cust_id`
# writes it bare, so it reads the orders' column, the member's own table's,
# even when the query joins the addresses too.
by_customer = show(
    "Revenue by customer, filtered on the joined address table",
    "SELECT * FROM SEMANTIC_VIEW(order_cities DIMENSIONS customer METRICS revenue "
    "WHERE city IS NOT NULL) ORDER BY customer",
)
assert by_customer == [(1, Decimal("300.00")), (2, Decimal("70.00"))], by_customer


print("\n=== Range relationships ===")

con.execute("""
CREATE TABLE periods (period_id INTEGER, start_ts TIMESTAMP, end_ts TIMESTAMP, label VARCHAR);
INSERT INTO periods VALUES
  (1, '2024-01-01', '2024-02-01', 'jan'),
  (2, '2024-02-01', '2024-03-01', 'feb'),
  (3, '2024-03-01', NULL,         'later');
CREATE TABLE events (event_id INTEGER, event_ts TIMESTAMP, v INTEGER);
INSERT INTO events VALUES
  (1, '2024-01-01', 1), (2, '2024-02-01', 10), (3, '2024-02-29 23:00', 100),
  (4, '2025-06-01', 1000), (5, '2023-06-01', 10000);
""")

con.execute("""
CREATE SEMANTIC VIEW event_periods AS
  TABLES (
    e AS events  PRIMARY KEY (event_id),
    p AS periods PRIMARY KEY (period_id)
      CONSTRAINT period_range DISTINCT RANGE BETWEEN start_ts AND end_ts EXCLUSIVE
  )
  RELATIONSHIPS (
    event_period AS e (event_ts) REFERENCES p (BETWEEN start_ts AND end_ts EXCLUSIVE)
  )
  DIMENSIONS (p.label AS label)
  METRICS (e.total_v AS SUM(v), p.period_count AS COUNT(*))
""")

# The start is included and the end excluded, so event 2 (on 1 February) is
# February's; an open end bound reaches event 4; event 5 is in no period.
by_period = show(
    "Event totals by period",
    "SELECT * FROM SEMANTIC_VIEW(event_periods DIMENSIONS label METRICS total_v) "
    "ORDER BY label NULLS LAST",
)
assert by_period == [("feb", 110), ("jan", 1), ("later", 1000), (None, 10000)], by_period

totals = show(
    "Each metric counts its own table's rows",
    "SELECT * FROM SEMANTIC_VIEW(event_periods METRICS total_v, period_count)",
)
assert totals == [(11111, 3)], totals


print("\n=== Many-to-many through a bridge table ===")

con.execute("""
CREATE TABLE authors (author_id INTEGER, name VARCHAR, country VARCHAR);
INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'), (3, 'Cleo', 'UK');
CREATE TABLE books (book_id INTEGER, title VARCHAR, pages INTEGER);
INSERT INTO books VALUES
  (10, 'Duo', 100), (11, 'Mixed', 200), (12, 'Solo', 300), (13, 'Orphan', 50);
CREATE TABLE book_authors (book_id INTEGER, author_id INTEGER);
INSERT INTO book_authors VALUES (10, 1), (10, 2), (11, 1), (11, 3), (12, 3);
""")

con.execute("""
CREATE SEMANTIC VIEW library AS
  TABLES (
    a  AS authors      PRIMARY KEY (author_id),
    b  AS books        PRIMARY KEY (book_id),
    ba AS book_authors PRIMARY KEY (book_id, author_id)
  )
  RELATIONSHIPS (
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b
  )
  DIMENSIONS (a.country AS country, b.title AS title)
  METRICS (
    b.total_pages   AS SUM(pages),
    b.book_count    AS COUNT(*),
    b.running_pages AS SUM(total_pages) OVER (ORDER BY b.title)
  )
""")

# Duo has two French authors and counts once for FR; Mixed has a French and a
# UK author and counts under both; Orphan has no author and lands under NULL.
by_country = show(
    "Pages and books by author country",
    "SELECT * FROM SEMANTIC_VIEW(library DIMENSIONS country "
    "METRICS total_pages, book_count) ORDER BY country NULLS LAST",
)
assert by_country == [("FR", 300, 2), ("UK", 500, 2), (None, 50, 1)], by_country

# The window runs over the query's own result rows: the French authors'
# books, each once, with a running total in title order.
running = show(
    "French authors' books with a running page total",
    "SELECT * FROM SEMANTIC_VIEW(library DIMENSIONS title "
    "METRICS total_pages, running_pages WHERE country = 'FR') ORDER BY title",
)
assert running == [("Duo", 100, 100), ("Mixed", 200, 300)], running

grand = show(
    "With no dimension, each book counts once",
    "SELECT * FROM SEMANTIC_VIEW(library METRICS total_pages, book_count)",
)
assert grand == [(650, 4)], grand


print("\n=== Two paths through a diamond ===")

# Orders reach regions through their customer and through their store.
con.execute("""
CREATE TABLE regions (region_id INTEGER, region_name VARCHAR);
INSERT INTO regions VALUES (1, 'EU'), (2, 'US');
CREATE TABLE customers (cust_id INTEGER, region_id INTEGER);
INSERT INTO customers VALUES (1, 1), (2, 2);
CREATE TABLE stores (store_id INTEGER, region_id INTEGER);
INSERT INTO stores VALUES (1, 2), (2, 1);
CREATE TABLE sales (sale_id INTEGER, cust_id INTEGER, store_id INTEGER, amount INTEGER);
INSERT INTO sales VALUES (1, 1, 1, 10), (2, 2, 1, 20), (3, 1, 2, 40);
""")

con.execute("""
CREATE SEMANTIC VIEW sales_regions AS
  TABLES (
    o AS sales     PRIMARY KEY (sale_id),
    c AS customers PRIMARY KEY (cust_id),
    s AS stores    PRIMARY KEY (store_id),
    r AS regions   PRIMARY KEY (region_id)
  )
  RELATIONSHIPS (
    o_cust   AS o (cust_id)   REFERENCES c,
    o_store  AS o (store_id)  REFERENCES s,
    c_region AS c (region_id) REFERENCES r,
    s_region AS s (region_id) REFERENCES r
  )
  DIMENSIONS (r.region_name AS region_name)
  METRICS (
    o.revenue      AS SUM(amount),
    o.rev_by_cust  USING (o_cust)  AS SUM(amount),
    o.rev_by_store USING (o_store) AS SUM(amount)
  )
""")

expect_error(
    "Revenue by region with no USING would have to pick a path: refused",
    "SELECT * FROM SEMANTIC_VIEW(sales_regions DIMENSIONS region_name METRICS revenue)",
    "by more than one path",
)

by_region = show(
    "USING names each metric's path: by customer region and by store region",
    "SELECT * FROM SEMANTIC_VIEW(sales_regions DIMENSIONS region_name "
    "METRICS rev_by_cust, rev_by_store) ORDER BY region_name",
)
assert by_region == [("EU", 50, 40), ("US", 20, 30)], by_region

print("\nAll v0.19.0 examples passed.")
