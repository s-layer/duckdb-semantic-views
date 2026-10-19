#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/function_discovery.py

Demonstrates v0.13.0 features:
  - Every function the extension registers is documented in duckdb_functions():
    a description, real parameter names, runnable examples and categories --
    everything a tool or AI agent connected over SQL needs to find and call it.
  - The table functions behind SHOW / DESCRIBE SEMANTIC ... point callers at
    the statement to use instead.
  - The "semantic view not found" error suggests SHOW SEMANTIC VIEWS.
  - list_semantic_views() as a FROM source: exporting every view's DDL.

The walkthrough plays the part of an agent that knows nothing about the
extension beyond its name: everything it learns comes from SQL.
"""

import os

import duckdb

EXTENSION_PATH = os.environ.get(
    "SEMANTIC_VIEWS_EXTENSION_PATH",
    "build/debug/semantic_views.duckdb_extension",
)

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute(f"LOAD '{EXTENSION_PATH}'")

# ============================================================
# Section 1: Setup -- a table and a semantic view to talk about
# ============================================================

con.execute("""
CREATE TABLE orders (id INTEGER PRIMARY KEY, region VARCHAR, amount DECIMAL(10,2));
INSERT INTO orders VALUES (1, 'US', 100), (2, 'EU', 200), (3, 'US', 50);
""")
con.execute("""
CREATE SEMANTIC VIEW sales AS
  TABLES (o AS orders PRIMARY KEY (id))
  DIMENSIONS (o.region AS o.region)
  METRICS (o.revenue AS SUM(o.amount))
""")
print("=== Section 1: created table `orders` and semantic view `sales` ===")

# ============================================================
# Section 2: What does this extension offer?
# ============================================================
# Every registration is tagged with the `semantic_views` category, so one
# query lists them all -- with real parameter names, not col0/col1.

print("\n=== Section 2: the extension's functions, from duckdb_functions() ===")
rows = con.execute("""
    SELECT DISTINCT function_name, function_type, categories[2] AS kind
    FROM duckdb_functions()
    WHERE list_contains(categories, 'semantic_views')
    ORDER BY kind DESC, function_name
""").fetchall()
for name, ftype, kind in rows:
    print(f"  [{kind:8}] {ftype:6}  {name}")

# ============================================================
# Section 3: How do I call the main query function?
# ============================================================
# The description explains the arguments; the example is runnable as-is
# (it assumes a view like `sales`, which Section 1 created).

print("\n=== Section 3: reading semantic_view()'s signature and example ===")
params, description, examples = con.execute("""
    SELECT parameters, description, examples
    FROM duckdb_functions() WHERE function_name = 'semantic_view'
""").fetchone()
print(f"  parameters:  {params}")
print(f"  description: {description}")
print(f"  example:     {examples[0]}")
print("  running the example:")
for row in con.execute(examples[0]).fetchall():
    print(f"    {row}")

# get_ddl has two overloads; both carry real names.
print("\n  get_ddl overloads:")
for (params,) in con.execute("""
    SELECT parameters FROM duckdb_functions()
    WHERE function_name = 'get_ddl' ORDER BY len(parameters)
""").fetchall():
    print(f"    get_ddl({', '.join(params)})")

# ============================================================
# Section 4: Functions behind a statement redirect to it
# ============================================================
# show_semantic_metrics() is what `SHOW SEMANTIC METRICS IN <view>` runs.
# Its description says so, and its example is the statement itself.

print("\n=== Section 4: a DDL-backing function points at its statement ===")
description, examples = con.execute("""
    SELECT description, examples
    FROM duckdb_functions() WHERE function_name = 'show_semantic_metrics'
""").fetchone()
print(f"  description: {description}")
print(f"  example:     {examples[0]}")
print("  running the example:")
for row in con.execute(examples[0]).fetchall():
    print(f"    {row}")

# ============================================================
# Section 5: The not-found error points at the statement too
# ============================================================

print("\n=== Section 5: a misspelled view name ===")
try:
    con.execute("FROM semantic_view('sale', metrics := ['revenue'])").fetchall()
except duckdb.Error as e:
    print(f"  {e}")

# ============================================================
# Section 6: list_semantic_views() as a FROM source
# ============================================================
# The one reason to call a listing function directly: SHOW cannot be a FROM
# source. Qualifying and quoting each name part keeps the lookup unambiguous
# even when two schemas hold a view of the same name.

print("\n=== Section 6: export every view's DDL ===")
con.execute("CREATE SCHEMA staging")
con.execute("""
CREATE SEMANTIC VIEW staging.sales AS
  TABLES (o AS main.orders PRIMARY KEY (id))
  DIMENSIONS (o.region AS o.region)
  METRICS (o.revenue AS SUM(o.amount))
""")
export_sql = con.execute("""
    SELECT examples[2] FROM duckdb_functions()
    WHERE function_name = 'list_semantic_views'
""").fetchone()[0]
print(f"  documented query: {export_sql}")
schemas = [r[0] for r in con.execute("SELECT schema_name FROM list_semantic_views()").fetchall()]
for schema, (ddl,) in zip(schemas, con.execute(export_sql).fetchall()):
    print(f"  --- {schema}.sales")
    for line in ddl.splitlines():
        print(f"  {line}")
print("  (GET_DDL renders a bare name by default; pass true as its third")
print("   argument to schema-qualify the CREATE statement.)")
