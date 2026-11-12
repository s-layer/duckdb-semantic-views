#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/per_grain_metrics.py

Demonstrates v0.12.0 per-grain ("own-grain") metric aggregation — each metric is
computed at the grain of its own table, the way Snowflake computes multi-grain
queries:
  - a metric on a PARENT of the base table, queried alone and grouped
  - metrics at two different grains queried together (fan trap)
  - metrics on two different child tables of one parent (chasm trap)
  - a derived metric whose components span grains
  - a dimension below a metric's grain: each row counted once per group

Each section prints the value the base-anchored formulation used to produce, so
the difference per-grain aggregation makes is visible rather than asserted.
"""
import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")

# ============================================================
# Setup
#
#   customers <--customer_id-- orders <--order_id-- line_items
#                                     <--order_id-- shipments
#
# `orders` is the base table, so `customers` is a PARENT (its rows repeat once
# per order when joined from orders) and `line_items` / `shipments` are two
# CHILDREN of it — the chasm-trap shape.
#
# Carol has no orders; order 4 has no line items. Those absences are exactly
# what per-grain aggregation gets right and a base-anchored join does not.
# ============================================================

con.execute("""
CREATE TABLE customers (id INTEGER PRIMARY KEY, name VARCHAR, region VARCHAR, credit INTEGER);
INSERT INTO customers VALUES
    (1, 'Alice', 'West', 500),
    (2, 'Bob',   'East', 300),
    (3, 'Carol', 'North', 900);  -- no orders, and the only customer in North

CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER, status VARCHAR, amount INTEGER);
INSERT INTO orders VALUES
    (1, 1, 'open',   100),
    (2, 1, 'closed', 250),
    (3, 2, 'open',    75),
    (4, 2, 'open',    40);       -- no line items

CREATE TABLE line_items (order_id INTEGER, sku VARCHAR, qty INTEGER);
INSERT INTO line_items VALUES
    (1, 'a', 2), (1, 'b', 3), (2, 'c', 1), (3, 'd', 5);

CREATE TABLE shipments (order_id INTEGER, carrier VARCHAR, weight INTEGER);
INSERT INTO shipments VALUES
    (1, 'air', 10), (1, 'sea', 40), (2, 'air', 15);
""")

con.execute("""
CREATE SEMANTIC VIEW sales AS
  TABLES (
    o  AS orders     PRIMARY KEY (id),
    c  AS customers  PRIMARY KEY (id),
    li AS line_items,
    s  AS shipments
  )
  RELATIONSHIPS (
    order_to_customer AS o(customer_id) REFERENCES c,
    item_to_order     AS li(order_id)   REFERENCES o,
    shipment_to_order AS s(order_id)    REFERENCES o
  )
  DIMENSIONS (
    c.region   AS c.region,
    o.status   AS o.status,
    li.sku     AS li.sku
  )
  METRICS (
    c.total_credit  AS SUM(c.credit),
    o.order_count   AS COUNT(*),
    o.order_amount  AS SUM(o.amount),
    li.item_qty     AS SUM(li.qty),
    s.ship_weight   AS SUM(s.weight),
    qty_per_order   AS item_qty / order_count
  );
""")

print("=== Setup ===")
print("  customers:  3 rows (Carol, the only North customer, has no orders)")
print("  orders:     4 rows (order 4 has no line items)")
print("  line_items: 4 rows")
print("  shipments:  3 rows")


def show(title, note, **kwargs):
    print(f"\n{title}")
    if note:
        print(f"  ({note})")
    rows = con.execute(
        "SELECT * FROM semantic_view('sales', "
        + ", ".join(f"{k} := {v!r}" for k, v in kwargs.items())
        + ")"
    ).fetchall()
    for row in sorted(rows, key=lambda r: tuple(str(c) for c in r)):
        print("   ", "  ".join("NULL" if c is None else str(c) for c in row))


# ============================================================
# Section 1: a metric on a parent of the base table
# ============================================================

print("\n=== Section 1: a metric on a PARENT of the base table ===")

show(
    "Total customer credit:",
    "500 + 300 + 900 = 1700 — each customer once. Anchored at orders it would "
    "be 1600: Alice counted twice (two orders), Carol dropped entirely",
    metrics=["total_credit"],
)

show(
    "Credit by region:",
    "West = Alice 500, East = Bob 300, North = Carol 900 — Carol has no orders "
    "but her credit is still hers to count",
    dimensions=["region"],
    metrics=["total_credit"],
)

# ============================================================
# Section 2: two grains in one query (fan trap)
# ============================================================

print("\n=== Section 2: metrics at two grains (fan trap) ===")

show(
    "Orders and their line-item quantity:",
    "4 orders, 11 units. Over a single orders x line_items join, COUNT(*) "
    "would report 5 — order 1 duplicated by its two items, order 4 "
    "NULL-extended",
    metrics=["order_count", "item_qty"],
)

show(
    "The same pair, by order status:",
    "open: 3 orders and 10 units; closed: 1 order and 1 unit",
    dimensions=["status"],
    metrics=["order_count", "item_qty"],
)

show(
    "Customer credit alongside order count, by region:",
    "North exists only at the customer grain — Carol has no orders — so the "
    "NULL-safe FULL OUTER JOIN keeps the group with a NULL order count instead "
    "of dropping it",
    dimensions=["region"],
    metrics=["total_credit", "order_count"],
)

# ============================================================
# Section 3: two children of one parent (chasm trap)
# ============================================================

print("\n=== Section 3: two child tables of one parent (chasm trap) ===")

show(
    "Line-item quantity and shipment weight:",
    "11 units and 65 kg. Joined together in one query they would multiply each "
    "other: order 1's 2 items x 2 shipments = 4 rows",
    metrics=["item_qty", "ship_weight"],
)

# ============================================================
# Section 4: a derived metric spanning grains
# ============================================================

print("\n=== Section 4: a derived metric whose components span grains ===")

show(
    "Units per order (item_qty / order_count):",
    "11 / 4 = 2.75. Each component is aggregated at its own grain first, so "
    "the denominator is the true order count, not the fanned one",
    metrics=["qty_per_order"],
)

# ============================================================
# Section 5: a dimension below a metric's grain
# ============================================================

print("\n=== Section 5: a dimension below a metric's grain ===")

# Order 1 has line items with SKUs a and b, so it counts under both; order 4
# has no line items and counts under NULL. Each order is counted once per
# group, never once per line item, as in Snowflake.
by_sku = con.execute(
    "SELECT * FROM semantic_view('sales', dimensions := ['sku'], metrics := ['order_count']) "
    "ORDER BY sku NULLS LAST"
).fetchall()
print("\n  order_count by sku (each order once per SKU):")
for row in by_sku:
    print(f"    {row[0]}: {row[1]}")
assert by_sku == [("a", 1), ("b", 1), ("c", 1), ("d", 1), (None, 1)], by_sku

# Alice has an open and a closed order, so her credit counts under both;
# Carol has no orders and counts under NULL.
by_status = con.execute(
    "SELECT * FROM semantic_view('sales', dimensions := ['status'], metrics := ['total_credit']) "
    "ORDER BY status NULLS LAST"
).fetchall()
print("\n  total_credit by status (each customer once per status):")
for row in by_status:
    print(f"    {row[0]}: {row[1]}")
assert by_status == [("closed", 500), ("open", 800), (None, 900)], by_status

con.execute("DROP SEMANTIC VIEW sales")
