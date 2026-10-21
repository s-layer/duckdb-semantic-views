#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
uv run _notes/demo.py
"""
import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")

# --- Sample data: a small e-commerce schema ---
con.execute("""
CREATE TABLE customers (id INTEGER, name VARCHAR, city VARCHAR);
INSERT INTO customers VALUES
    (1, 'Alice', 'Portland'),
    (2, 'Bob',   'Seattle'),
    (3, 'Carol', 'Portland');

CREATE TABLE products (id INTEGER, name VARCHAR, category VARCHAR);
INSERT INTO products VALUES
    (10, 'Widget',  'Hardware'),
    (20, 'Gadget',  'Hardware'),
    (30, 'Service', 'Software');

CREATE TABLE orders (
    id INTEGER, customer_id INTEGER, product_id INTEGER,
    amount DECIMAL(10,2), ordered_at DATE
);
INSERT INTO orders VALUES
    (1, 1, 10, 25.00,  '2024-01-15'),
    (2, 1, 20, 50.00,  '2024-01-20'),
    (3, 2, 10, 25.00,  '2024-02-10'),
    (4, 2, 30, 100.00, '2024-02-14'),
    (5, 3, 20, 50.00,  '2024-03-01');
""")

# --- Define semantic view with PK/FK relationships ---
con.execute("""
CREATE SEMANTIC VIEW shop AS
TABLES (
    o AS orders      PRIMARY KEY (id),
    c AS customers   PRIMARY KEY (id),
    p AS products    PRIMARY KEY (id)
)
RELATIONSHIPS (
    order_customer AS o(customer_id) REFERENCES c,
    order_product  AS o(product_id)  REFERENCES p
)
DIMENSIONS (
    c.customer   AS c.name,
    c.city       AS c.city,
    p.product    AS p.name,
    p.category   AS p.category,
    o.month      AS date_trunc('month', o.ordered_at)
)
METRICS (
    o.revenue     AS sum(o.amount),
    o.order_count AS count(*)
);
""")

# 1. Revenue by customer (only customers table joined)
print("=== Revenue by customer ===")
for row in con.execute("""
    SELECT * FROM semantic_view('shop',
        dimensions := ['customer'],
        metrics := ['revenue', 'order_count']
    ) ORDER BY revenue DESC
""").fetchall():
    print(row)

# 2. Revenue by product category (only products table joined)
print("\n=== Revenue by category ===")
for row in con.execute("""
    SELECT * FROM semantic_view('shop',
        dimensions := ['category'],
        metrics := ['revenue']
    ) ORDER BY revenue DESC
""").fetchall():
    print(row)

# 3. Cross-table: customer × product (both tables joined through orders)
print("\n=== Customer × Product ===")
for row in con.execute("""
    SELECT * FROM semantic_view('shop',
        dimensions := ['customer', 'product'],
        metrics := ['revenue']
    ) ORDER BY customer, product
""").fetchall():
    print(row)

# 4. Monthly revenue (time dimension via date_trunc)
print("\n=== Monthly revenue ===")
for row in con.execute("""
    SELECT * FROM semantic_view('shop',
        dimensions := ['month'],
        metrics := ['revenue', 'order_count']
    ) ORDER BY month
""").fetchall():
    print(row)

# 5. Grand total (metrics only, no dimensions)
print("\n=== Grand total ===")
print(con.execute("""
    SELECT * FROM semantic_view('shop',
        metrics := ['revenue', 'order_count']
    )
""").fetchone())

# 6. See the generated SQL
print("\n=== Explain (customer + revenue) ===")
for row in con.execute("""
    SELECT * FROM explain_semantic_view('shop',
        dimensions := ['customer'],
        metrics := ['revenue']
    )
""").fetchall():
    print(row[0])

# 7. DESCRIBE the view
print("\n=== Describe ===")
for row in con.execute("DESCRIBE SEMANTIC VIEW shop").fetchall():
    print(row)
