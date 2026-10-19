#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/advanced_features.py

Demonstrates v0.5.3 advanced semantic features:
  - FACTS: reusable row-level expressions with chaining
  - Derived metrics: metric-on-metric composition
  - Cardinality annotations and fan trap detection
  - Role-playing dimensions with USING RELATIONSHIPS
  - EXPLAIN / DESCRIBE for introspection
"""
import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")

# ============================================================
# Section 1: Setup -- Create physical tables
# ============================================================

con.execute("""
CREATE TABLE line_items (
    id INTEGER, order_id INTEGER,
    extended_price DECIMAL(10,2), discount DECIMAL(3,2),
    tax_rate DECIMAL(3,2), unit_cost DECIMAL(10,2)
);
INSERT INTO line_items VALUES
    (1, 1, 100.00, 0.10, 0.05, 50.00),
    (2, 1, 200.00, 0.20, 0.08, 80.00),
    (3, 2, 150.00, 0.00, 0.10, 60.00);

CREATE TABLE orders (id INTEGER, customer_id INTEGER, region VARCHAR);
INSERT INTO orders VALUES (1, 10, 'East'), (2, 20, 'West');

CREATE TABLE customers (id INTEGER, name VARCHAR, country VARCHAR, state VARCHAR, city VARCHAR);
INSERT INTO customers VALUES
    (10, 'Alice', 'US', 'NY', 'New York'),
    (20, 'Bob',   'US', 'CA', 'Los Angeles');
""")

print("=== Section 1: Physical tables created ===")
print("  line_items: 3 rows (order line-level pricing)")
print("  orders:     2 rows (order headers)")
print("  customers:  2 rows (customer geography)")

# ============================================================
# Section 2: FACTS -- Reusable row-level expressions
# ============================================================

print("\n=== Section 2: FACTS -- Reusable row-level expressions ===")

con.execute("""
CREATE SEMANTIC VIEW sales AS
  TABLES (
    li AS line_items PRIMARY KEY (id),
    o AS orders PRIMARY KEY (id),
    c AS customers PRIMARY KEY (id)
  )
  RELATIONSHIPS (
    li_to_order AS li(order_id) REFERENCES o,
    order_to_customer AS o(customer_id) REFERENCES c
  )
  FACTS (
    li.net_price AS li.extended_price * (1 - li.discount),
    li.tax_amount AS li.net_price * li.tax_rate
  )
  DIMENSIONS (
    o.region  AS o.region,
    c.country AS c.country,
    c.state   AS c.state,
    c.city    AS c.city
  )
  METRICS (
    li.total_net AS SUM(li.net_price),
    li.total_tax AS SUM(li.tax_amount),
    li.total_cost AS SUM(li.unit_cost)
  );
""")

# Fact chain: net_price = extended_price * (1 - discount)
#             tax_amount = net_price * tax_rate  (references another fact!)
# Item 1: net=100*(1-0.10)=90,  tax=90*0.05=4.50
# Item 2: net=200*(1-0.20)=160, tax=160*0.08=12.80
# Item 3: net=150*(1-0.00)=150, tax=150*0.10=15.00
# East (items 1+2): total_net=250, total_tax=17.30
# West (item 3):    total_net=150, total_tax=15.00

print("\nRevenue by region (fact chain: net_price -> tax_amount):")
for row in con.execute("""
    SELECT * FROM semantic_view('sales',
        dimensions := ['region'],
        metrics := ['total_net', 'total_tax']
    ) ORDER BY region
""").fetchall():
    print(f"  {row[0]}: total_net={row[1]}, total_tax={row[2]}")

# ============================================================
# Section 3: Derived metrics -- Metric-on-metric composition
# ============================================================

print("\n=== Section 3: Derived metrics -- Metric-on-metric composition ===")

con.execute("DROP SEMANTIC VIEW sales")
con.execute("""
CREATE SEMANTIC VIEW sales AS
  TABLES (
    li AS line_items PRIMARY KEY (id),
    o AS orders PRIMARY KEY (id),
    c AS customers PRIMARY KEY (id)
  )
  RELATIONSHIPS (
    li_to_order AS li(order_id) REFERENCES o,
    order_to_customer AS o(customer_id) REFERENCES c
  )
  FACTS (
    li.net_price AS li.extended_price * (1 - li.discount)
  )
  DIMENSIONS (
    o.region  AS o.region,
    c.country AS c.country
  )
  METRICS (
    li.revenue AS SUM(li.net_price),
    li.cost    AS SUM(li.unit_cost),
    profit     AS revenue - cost,
    margin     AS profit / revenue * 100
  );
""")

# East: revenue=250, cost=130, profit=120
# West: revenue=150, cost=60,  profit=90

print("\nProfitability by region:")
for row in con.execute("""
    SELECT * FROM semantic_view('sales',
        dimensions := ['region'],
        metrics := ['revenue', 'cost', 'profit']
    ) ORDER BY region
""").fetchall():
    print(f"  {row[0]}: revenue={row[1]}, cost={row[2]}, profit={row[3]}")

# Margin: East=120/250*100=48.0, West=90/150*100=60.0
print("\nMargin by region:")
for row in con.execute("""
    SELECT region, ROUND(margin, 1) FROM (
        SELECT * FROM semantic_view('sales',
            dimensions := ['region'],
            metrics := ['margin']
        )
    ) ORDER BY region
""").fetchall():
    print(f"  {row[0]}: margin={row[1]}%")

# Grand total profit (no dimensions)
print("\nGrand total profit:")
result = con.execute("""
    SELECT * FROM semantic_view('sales', metrics := ['profit'])
""").fetchone()
print(f"  profit={result[0]}")

# ============================================================
# Section 4: Fan trap detection -- Cardinality-aware safety
# ============================================================

print("\n=== Section 4: Fan trap detection -- Cardinality-aware safety ===")

con.execute("DROP SEMANTIC VIEW sales")
con.execute("""
CREATE SEMANTIC VIEW fan_trap_demo AS
  TABLES (
    o AS orders PRIMARY KEY (id),
    li AS line_items PRIMARY KEY (id)
  )
  RELATIONSHIPS (
    li_to_order AS li(order_id) REFERENCES o
  )
  DIMENSIONS (
    o.region     AS o.region,
    li.price_tier AS CASE WHEN li.extended_price > 100 THEN 'high' ELSE 'low' END
  )
  METRICS (
    li.revenue     AS SUM(li.extended_price),
    o.order_count  AS COUNT(*)
  );
""")

# Safe query: li.revenue with o.region -- li->o is many-to-one (safe direction)
print("\nSafe query (revenue by region, many-to-one direction):")
for row in con.execute("""
    SELECT * FROM semantic_view('fan_trap_demo',
        dimensions := ['region'],
        metrics := ['revenue']
    ) ORDER BY region
""").fetchall():
    print(f"  {row[0]}: revenue={row[1]}")

# Fan trap: o.order_count with li.price_tier
# Traversing o->li is one-to-many (fan-out) -- would inflate the count!
print("\nFan trap query (order_count by price_tier -- blocked!):")
try:
    con.execute("""
        SELECT * FROM semantic_view('fan_trap_demo',
            dimensions := ['price_tier'],
            metrics := ['order_count']
        )
    """)
except Exception as e:
    print(f"  Error: {e}")
    print("  (order_count is from orders, but price_tier requires joining")
    print("   line_items. Traversing orders->line_items is one-to-many,")
    print("   which would inflate the count.)")

# ============================================================
# Section 5: Role-playing dimensions with USING RELATIONSHIPS
# ============================================================

print("\n=== Section 5: Role-playing dimensions with USING RELATIONSHIPS ===")

con.execute("DROP SEMANTIC VIEW fan_trap_demo")

con.execute("""
CREATE TABLE airports (airport_code VARCHAR, city VARCHAR, country VARCHAR);
INSERT INTO airports VALUES
    ('SFO', 'San Francisco', 'US'),
    ('JFK', 'New York',      'US'),
    ('LHR', 'London',        'UK');

CREATE TABLE flights (flight_id INTEGER, departure_code VARCHAR, arrival_code VARCHAR, carrier VARCHAR);
INSERT INTO flights VALUES
    (1, 'SFO', 'JFK', 'AA'),
    (2, 'JFK', 'LHR', 'BA'),
    (3, 'LHR', 'SFO', 'AA');
""")

con.execute("""
CREATE SEMANTIC VIEW flight_analytics AS
  TABLES (
    f AS flights PRIMARY KEY (flight_id),
    a AS airports PRIMARY KEY (airport_code)
  )
  RELATIONSHIPS (
    dep_airport AS f(departure_code) REFERENCES a,
    arr_airport AS f(arrival_code) REFERENCES a
  )
  DIMENSIONS (
    a.city    AS a.city,
    a.country AS a.country,
    f.carrier AS f.carrier
  )
  METRICS (
    f.departure_count USING (dep_airport) AS COUNT(*),
    f.arrival_count   USING (arr_airport) AS COUNT(*),
    total_flights     AS departure_count + arrival_count
  );
""")

# Departures by city: each city has 1 departure
print("\nDepartures by city:")
for row in con.execute("""
    SELECT * FROM semantic_view('flight_analytics',
        dimensions := ['city'],
        metrics := ['departure_count']
    ) ORDER BY city
""").fetchall():
    print(f"  {row[0]}: {row[1]}")

# Arrivals by city: each city has 1 arrival
print("\nArrivals by city:")
for row in con.execute("""
    SELECT * FROM semantic_view('flight_analytics',
        dimensions := ['city'],
        metrics := ['arrival_count']
    ) ORDER BY city
""").fetchall():
    print(f"  {row[0]}: {row[1]}")

# Carrier (non-ambiguous dimension) with both metrics
# AA: 2 flights total, BA: 1 flight total
print("\nFlights by carrier (non-ambiguous dimension):")
for row in con.execute("""
    SELECT * FROM semantic_view('flight_analytics',
        dimensions := ['carrier'],
        metrics := ['departure_count', 'arrival_count']
    ) ORDER BY carrier
""").fetchall():
    print(f"  {row[0]}: departures={row[1]}, arrivals={row[2]}")

# Ambiguous query: total_flights references both USING paths,
# city is from airports -- which relationship should resolve it?
print("\nAmbiguous query (total_flights by city -- blocked!):")
try:
    con.execute("""
        SELECT * FROM semantic_view('flight_analytics',
            dimensions := ['city'],
            metrics := ['total_flights']
        )
    """)
except Exception as e:
    print(f"  Error: {e}")
    print("  (total_flights depends on both dep_airport and arr_airport;")
    print("   city is from airports -- ambiguous which path to use.)")

# Total flights by carrier (derived, non-ambiguous)
# AA: 2+2=4, BA: 1+1=2
print("\nTotal flights by carrier (non-ambiguous):")
for row in con.execute("""
    SELECT * FROM semantic_view('flight_analytics',
        dimensions := ['carrier'],
        metrics := ['total_flights']
    ) ORDER BY carrier
""").fetchall():
    print(f"  {row[0]}: {row[1]}")

# ============================================================
# Section 6: EXPLAIN -- See the generated SQL for role-playing
# ============================================================

print("\n=== Section 6: EXPLAIN -- Generated SQL with scoped aliases ===")

for row in con.execute("""
    SELECT * FROM explain_semantic_view('flight_analytics',
        dimensions := ['city'],
        metrics := ['departure_count']
    )
""").fetchall():
    print(f"  {row[0]}")

# ============================================================
# Section 7: DESCRIBE -- Full metadata view
# ============================================================

print("\n=== Section 7: DESCRIBE -- Full metadata view ===")

# DESCRIBE returns one row per property: (object_kind, object_name, parent_entity, property, property_value)
rows = con.execute("DESCRIBE SEMANTIC VIEW flight_analytics").fetchall()

# Group by object kind for readable output
from collections import defaultdict
by_kind = defaultdict(list)
for row in rows:
    by_kind[row[0]].append(row)

for kind in ['TABLE', 'RELATIONSHIP', 'DIMENSION', 'METRIC', 'DERIVED_METRIC']:
    if kind in by_kind:
        print(f"\n  {kind}s:")
        for r in by_kind[kind]:
            print(f"    {r[1]}.{r[3]} = {r[4]}")
