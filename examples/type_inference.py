#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0.dev2610151406"]
# requires-python = ">=3.10"
# ///
"""
uv run examples/type_inference.py

Demonstrates member data types in SHOW / DESCRIBE (TECH-DEBT #51):
  - DATA_TYPE in DESCRIBE, SHOW SEMANTIC DIMENSIONS/METRICS, SHOW COLUMNS
  - an aggregate reports its own type (COUNT -> BIGINT, AVG -> DOUBLE,
    SUM of DECIMAL(10,2) -> DECIMAL(38,2), SUM of INTEGER -> BIGINT as
    semantic_view() returns it)
  - the type is worked out when SHOW runs, so it follows a source column
    whose type changes after CREATE, and an unbindable member reports ''

(Before this, v0.7.1 inferred types at CREATE for file-backed databases only,
and from v0.10.0 the column was empty.)

Requires a debug build: just build
"""
import duckdb
import os
import tempfile

tmpdir = tempfile.mkdtemp()
db_path = os.path.join(tmpdir, "type_inference_demo.duckdb")
con = duckdb.connect(db_path, config={"allow_unsigned_extensions": "true"})
con.execute("LOAD 'build/debug/semantic_views.duckdb_extension'")

con.execute("""
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    region VARCHAR,
    ordered_at DATE,
    quantity INTEGER,
    unit_price DOUBLE,
    total DECIMAL(10,2)
);
INSERT INTO orders VALUES
    (1, 'US',  '2024-01-15', 3,  25.00, 75.00),
    (2, 'EU',  '2024-01-20', 1,  50.00, 50.00),
    (3, 'US',  '2024-02-10', 2, 100.00, 200.00),
    (4, 'EU',  '2024-02-14', 5,  10.00, 50.00);
""")

con.execute("""
CREATE SEMANTIC VIEW sales AS
TABLES (
    o AS orders PRIMARY KEY (id)
)
DIMENSIONS (
    o.region    AS o.region,
    o.order_day AS o.ordered_at
)
METRICS (
    o.order_count AS COUNT(*),
    o.avg_price   AS AVG(o.unit_price),
    o.total_qty   AS SUM(o.quantity),
    o.gross_total AS SUM(o.total)
);
""")

# DESCRIBE shows DATA_TYPE for each dimension and metric
print("=== DESCRIBE SEMANTIC VIEW ===")
for row in con.execute("DESCRIBE SEMANTIC VIEW sales").fetchall():
    kind, name, parent, prop, val = row
    if prop == "DATA_TYPE":
        print(f"  {kind:12s} {name:14s} DATA_TYPE = {val or '(empty)'}")

# SHOW SEMANTIC DIMENSIONS with each dimension's data_type
print("\n=== SHOW SEMANTIC DIMENSIONS ===")
for row in con.execute("SHOW SEMANTIC DIMENSIONS IN sales").fetchall():
    print(f"  {row[4]:14s} data_type={row[5] or '(empty)'}")

# SHOW SEMANTIC METRICS: an aggregate reports its own type, not its input's
print("\n=== SHOW SEMANTIC METRICS ===")
for row in con.execute("SHOW SEMANTIC METRICS IN sales").fetchall():
    print(f"  {row[4]:14s} data_type={row[5] or '(empty)'}")

# SHOW COLUMNS for a unified view
print("\n=== SHOW COLUMNS IN SEMANTIC VIEW ===")
for row in con.execute("SHOW COLUMNS IN SEMANTIC VIEW sales").fetchall():
    print(f"  {row[3]:14s} data_type={row[4] or '(empty)':10s} kind={row[5]}")

# The type is probed when SHOW runs, not stored at CREATE: retype a column and
# the metric over it follows.
con.execute("ALTER TABLE orders ALTER quantity TYPE DOUBLE")
print("\n=== after ALTER TABLE orders ALTER quantity TYPE DOUBLE ===")
for row in con.execute("SHOW SEMANTIC METRICS LIKE 'total_qty' IN sales").fetchall():
    print(f"  {row[4]:14s} data_type={row[5] or '(empty)'}")

# A member whose source no longer binds reports an empty data_type; the rest
# of the view is still typed and SHOW does not fail.
con.execute("ALTER TABLE orders DROP COLUMN unit_price")
print("\n=== after ALTER TABLE orders DROP COLUMN unit_price ===")
for row in con.execute("SHOW SEMANTIC METRICS IN sales").fetchall():
    print(f"  {row[4]:14s} data_type={row[5] or '(empty)'}")

con.close()

# Cleanup
import shutil
shutil.rmtree(tmpdir)
