#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/yaml_and_materializations.py

Demonstrates v0.7.0 features:
  - YAML definition format (inline and file-based)
  - Materialization declarations and transparent routing
  - YAML export and round-trip
  - Materialization introspection (EXPLAIN, DESCRIBE, SHOW)
"""
import duckdb
import tempfile
import os

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")

# ============================================================
# Setup: Sales data with a pre-aggregated summary table
# ============================================================

con.execute("""
CREATE TABLE sales (
    id INTEGER PRIMARY KEY,
    region VARCHAR,
    product VARCHAR,
    amount DECIMAL(10,2),
    sale_date DATE
);
INSERT INTO sales VALUES
    (1, 'East', 'Widget',  100.00, '2024-01-15'),
    (2, 'West', 'Gadget',  200.00, '2024-01-20'),
    (3, 'East', 'Widget',  150.00, '2024-02-10'),
    (4, 'West', 'Widget',  300.00, '2024-02-15'),
    (5, 'East', 'Gadget',   75.00, '2024-03-01'),
    (6, 'West', 'Gadget',  125.00, '2024-03-10');

-- Pre-aggregated table (e.g. maintained by dbt or a scheduled job)
CREATE TABLE sales_by_region AS
    SELECT region,
           SUM(amount) AS total_revenue,
           COUNT(*) AS order_count
    FROM sales GROUP BY region;
""")

print("=== Setup: sales (6 rows) + sales_by_region (pre-aggregated) ===")

# ============================================================
# Section 1: Create a semantic view from inline YAML
# ============================================================

print("\n=== Section 1: Inline YAML definition ===")

con.execute("""
CREATE SEMANTIC VIEW sales_view FROM YAML $$
base_table: sales
tables:
  - alias: s
    table: sales
    pk_columns:
      - id
dimensions:
  - name: region
    expr: s.region
    source_table: s
  - name: product
    expr: s.product
    source_table: s
metrics:
  - name: total_revenue
    expr: SUM(s.amount)
    source_table: s
  - name: order_count
    expr: COUNT(s.id)
    source_table: s
materializations:
  - name: by_region
    table: sales_by_region
    dimensions:
      - region
    metrics:
      - total_revenue
      - order_count
$$
""")

print("Created 'sales_view' from inline YAML with materialization 'by_region'")

# Query it
print("\nRevenue by region:")
for row in con.execute("""
    SELECT * FROM semantic_view('sales_view',
        dimensions := ['region'],
        metrics := ['total_revenue']
    ) ORDER BY region
""").fetchall():
    print(f"  {row[0]}: {row[1]}")

# ============================================================
# Section 2: YAML file loading
# ============================================================

print("\n=== Section 2: YAML file loading ===")

yaml_content = """\
base_table: sales
tables:
  - alias: s
    table: sales
    pk_columns:
      - id
dimensions:
  - name: region
    expr: s.region
    source_table: s
  - name: product
    expr: s.product
    source_table: s
metrics:
  - name: total_revenue
    expr: SUM(s.amount)
    source_table: s
"""

with tempfile.NamedTemporaryFile(
    mode="w", suffix=".yaml", delete=False
) as f:
    f.write(yaml_content)
    yaml_path = f.name

try:
    con.execute(
        f"CREATE SEMANTIC VIEW from_file FROM YAML FILE '{yaml_path}'"
    )
    result = con.execute("""
        SELECT * FROM semantic_view('from_file',
            dimensions := ['product'],
            metrics := ['total_revenue']
        ) ORDER BY product
    """).fetchall()
    print(f"Created 'from_file' from {os.path.basename(yaml_path)}")
    print("\nRevenue by product:")
    for row in result:
        print(f"  {row[0]}: {row[1]}")
finally:
    os.unlink(yaml_path)

# ============================================================
# Section 3: Materialization routing
# ============================================================

print("\n=== Section 3: Materialization routing ===")

# When dimensions+metrics exactly match a materialization, the query
# reads from the pre-aggregated table instead of the raw source.

explain = con.execute("""
    SELECT * FROM explain_semantic_view('sales_view',
        dimensions := ['region'],
        metrics := ['total_revenue', 'order_count']
    )
""").fetchall()

print("EXPLAIN with exact match (region + total_revenue + order_count):")
for row in explain:
    line = row[0]
    if line.startswith("--") or line.startswith("SELECT") or line.startswith("FROM"):
        print(f"  {line}")

# When there's no exact match, falls back to raw expansion
print("\nEXPLAIN with no match (product + total_revenue):")
explain2 = con.execute("""
    SELECT * FROM explain_semantic_view('sales_view',
        dimensions := ['product'],
        metrics := ['total_revenue']
    )
""").fetchall()
for row in explain2:
    line = row[0]
    if line.startswith("--") or line.startswith("SELECT") or line.startswith("FROM"):
        print(f"  {line}")

# ============================================================
# Section 4: YAML export and round-trip
# ============================================================

print("\n=== Section 4: YAML export and round-trip ===")

yaml_out = con.execute(
    "SELECT read_yaml_from_semantic_view('sales_view')"
).fetchone()[0]
print(f"Exported YAML ({len(yaml_out)} chars):")
for line in yaml_out.split("\n")[:8]:
    print(f"  {line}")
print("  ...")

# Round-trip: create a new view from the exported YAML
con.execute(
    f"CREATE SEMANTIC VIEW roundtrip FROM YAML $yaml${yaml_out}$yaml$"
)
yaml_out2 = con.execute(
    "SELECT read_yaml_from_semantic_view('roundtrip')"
).fetchone()[0]
print(f"\nRound-trip fidelity: {'MATCH' if yaml_out == yaml_out2 else 'MISMATCH'}")

# ============================================================
# Section 5: Materialization introspection
# ============================================================

print("\n=== Section 5: Materialization introspection ===")

# DESCRIBE shows materialization metadata
print("DESCRIBE SEMANTIC VIEW (materialization rows):")
desc = con.execute("DESCRIBE SEMANTIC VIEW sales_view").fetchall()
for row in desc:
    if row[0] == "MATERIALIZATION":
        print(f"  {row[1]}.{row[3]} = {row[4]}")

# SHOW SEMANTIC MATERIALIZATIONS
print("\nSHOW SEMANTIC MATERIALIZATIONS IN sales_view:")
for row in con.execute(
    "SHOW SEMANTIC MATERIALIZATIONS IN sales_view"
).fetchall():
    print(f"  name={row[3]}, table={row[4]}, dims={row[5]}, mets={row[6]}")

# Clean up
con.execute("DROP SEMANTIC VIEW sales_view")
con.execute("DROP SEMANTIC VIEW from_file")
con.execute("DROP SEMANTIC VIEW roundtrip")

print("\n=== All v0.7.0 features demonstrated successfully ===")
