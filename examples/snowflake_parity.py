#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/snowflake_parity.py

Demonstrates v0.5.4 features closing the gap with Snowflake's semantic view DDL:
  - UNIQUE constraints and automatic cardinality inference
  - ALTER SEMANTIC VIEW ... RENAME TO
  - SHOW SEMANTIC DIMENSIONS / METRICS / FACTS with LIKE, STARTS WITH, LIMIT
  - SHOW SEMANTIC DIMENSIONS IN view FOR METRIC (fan-trap-aware filtering)
"""
import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")

# ============================================================
# Setup: E-commerce schema (customers, orders, line_items)
# ============================================================

con.execute("""
CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name VARCHAR,
    email VARCHAR UNIQUE
);
INSERT INTO customers VALUES
    (1, 'Alice', 'alice@example.com'),
    (2, 'Bob',   'bob@example.com'),
    (3, 'Carol', 'carol@example.com');

CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER,
    region VARCHAR
);
INSERT INTO orders VALUES
    (1, 1, 'West'),
    (2, 1, 'West'),
    (3, 2, 'East'),
    (4, 3, 'East');

CREATE TABLE line_items (
    id INTEGER PRIMARY KEY,
    order_id INTEGER,
    amount DECIMAL(10,2),
    category VARCHAR
);
INSERT INTO line_items VALUES
    (1, 1, 50.00,  'Electronics'),
    (2, 1, 30.00,  'Books'),
    (3, 2, 100.00, 'Electronics'),
    (4, 3, 75.00,  'Books'),
    (5, 4, 200.00, 'Electronics');
""")

print("=== Setup: E-commerce tables created ===")
print("  customers:  3 rows (id PK, email UNIQUE)")
print("  orders:     4 rows (id PK, customer_id FK)")
print("  line_items: 5 rows (id PK, order_id FK)")

# ============================================================
# Section 1: UNIQUE constraints and cardinality inference
# ============================================================

print("\n=== Section 1: UNIQUE constraints and cardinality inference ===")

# Note: UNIQUE on customers(email) is declared in the CREATE TABLE above.
# The extension infers cardinality from constraints (PRIMARY KEY and UNIQUE)
# without requiring explicit MANY TO ONE keywords in RELATIONSHIPS.

con.execute("""
CREATE SEMANTIC VIEW ecommerce AS
  TABLES (
    li AS line_items PRIMARY KEY (id),
    o  AS orders      PRIMARY KEY (id),
    c  AS customers   PRIMARY KEY (id)
  )
  RELATIONSHIPS (
    item_order    AS li(order_id)    REFERENCES o,
    order_cust    AS o(customer_id)  REFERENCES c
  )
  FACTS (
    li.net_amount AS li.amount
  )
  DIMENSIONS (
    c.customer_name  AS c.name,
    c.customer_email AS c.email,
    o.region         AS o.region,
    li.category      AS li.category
  )
  METRICS (
    li.total_revenue AS SUM(li.net_amount),
    li.item_count    AS COUNT(*)
  );
""")

# Cardinality is inferred: li -> o is MANY TO ONE, o -> c is MANY TO ONE.
# No explicit cardinality annotations needed because PKs are declared.

print("\nRevenue by customer (cardinality inferred from PKs):")
for row in con.execute("""
    SELECT * FROM semantic_view('ecommerce',
        dimensions := ['customer_name'],
        metrics := ['total_revenue']
    ) ORDER BY customer_name
""").fetchall():
    print(f"  {row[0]}: {row[1]}")

print("\nRevenue by region:")
for row in con.execute("""
    SELECT * FROM semantic_view('ecommerce',
        dimensions := ['region'],
        metrics := ['total_revenue', 'item_count']
    ) ORDER BY region
""").fetchall():
    print(f"  {row[0]}: revenue={row[1]}, items={row[2]}")

# ============================================================
# Section 2: ALTER SEMANTIC VIEW RENAME TO
# ============================================================

print("\n=== Section 2: ALTER SEMANTIC VIEW RENAME TO ===")

# Rename the view
con.execute("ALTER SEMANTIC VIEW ecommerce RENAME TO shop")

print("Renamed 'ecommerce' -> 'shop'")

# Query under the new name
result = con.execute("""
    SELECT * FROM semantic_view('shop',
        metrics := ['total_revenue']
    )
""").fetchone()
print(f"Grand total (queried as 'shop'): {result[0]}")

# IF EXISTS on a non-existent view -- silent no-op
con.execute("ALTER SEMANTIC VIEW IF EXISTS nonexistent RENAME TO something")
print("ALTER IF EXISTS on non-existent view: silent no-op (no error)")

# ============================================================
# Section 3: SHOW SEMANTIC commands with filtering
# ============================================================

print("\n=== Section 3: SHOW SEMANTIC commands with filtering ===")

# List all semantic views
print("\nSHOW SEMANTIC VIEWS:")
for row in con.execute("SHOW SEMANTIC VIEWS").fetchall():
    print(f"  {row[1]}")  # columns: created_on, name, kind, database_name, schema_name, comment

# Show dimensions in a view
print("\nSHOW SEMANTIC DIMENSIONS IN shop:")
for row in con.execute("SHOW SEMANTIC DIMENSIONS IN shop").fetchall():
    print(f"  {row}")

# Show metrics in a view
print("\nSHOW SEMANTIC METRICS IN shop:")
for row in con.execute("SHOW SEMANTIC METRICS IN shop").fetchall():
    print(f"  {row}")

# Show facts in a view (requires FACTS clause)
print("\nSHOW SEMANTIC FACTS IN shop:")
for row in con.execute("SHOW SEMANTIC FACTS IN shop").fetchall():
    print(f"  {row}")

# Filter dimensions with LIKE (LIKE clause comes before IN)
print("\nSHOW SEMANTIC DIMENSIONS LIKE '%customer%' IN shop:")
for row in con.execute("SHOW SEMANTIC DIMENSIONS LIKE '%customer%' IN shop").fetchall():
    print(f"  {row}")

# Filter metrics with STARTS WITH (IN comes before STARTS WITH)
print("\nSHOW SEMANTIC METRICS IN shop STARTS WITH 'total':")
for row in con.execute("SHOW SEMANTIC METRICS IN shop STARTS WITH 'total'").fetchall():
    print(f"  {row}")

# Limit results
print("\nSHOW SEMANTIC DIMENSIONS IN shop LIMIT 2:")
for row in con.execute("SHOW SEMANTIC DIMENSIONS IN shop LIMIT 2").fetchall():
    print(f"  {row}")

# ============================================================
# Section 4: SHOW SEMANTIC DIMENSIONS FOR METRIC (fan-trap-aware)
# ============================================================

print("\n=== Section 4: SHOW SEMANTIC DIMENSIONS FOR METRIC (fan-trap-aware) ===")

# Create a view with potential fan traps
con.execute("DROP SEMANTIC VIEW shop")

con.execute("""
CREATE SEMANTIC VIEW fan_demo AS
  TABLES (
    o  AS orders     PRIMARY KEY (id),
    li AS line_items PRIMARY KEY (id)
  )
  RELATIONSHIPS (
    item_order AS li(order_id) REFERENCES o
  )
  DIMENSIONS (
    o.region       AS o.region,
    li.category    AS li.category
  )
  METRICS (
    li.revenue     AS SUM(li.amount),
    o.order_count  AS COUNT(*)
  );
""")

# Show all dimensions that are safe for a given metric.
# For li.revenue (source: line_items), traversing li->o is MANY TO ONE (safe),
# so o.region is available.
print("\nDimensions safe for 'revenue' (from line_items):")
for row in con.execute("SHOW SEMANTIC DIMENSIONS IN fan_demo FOR METRIC revenue").fetchall():
    print(f"  {row}")

# For o.order_count (source: orders), traversing o->li would be ONE TO MANY
# (fan trap!), so li.category is filtered out.
print("\nDimensions safe for 'order_count' (from orders):")
for row in con.execute("SHOW SEMANTIC DIMENSIONS IN fan_demo FOR METRIC order_count").fetchall():
    print(f"  {row}")

print("\n(Notice: 'category' is filtered out for order_count because")
print(" traversing orders->line_items is ONE TO MANY -- a fan trap!)")

# Clean up
con.execute("DROP SEMANTIC VIEW fan_demo")

print("\n=== All v0.5.4 features demonstrated successfully ===")
