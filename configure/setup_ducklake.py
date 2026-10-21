#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Set up a local DuckLake catalog with jaffle-shop sample data.

Downloads jaffle-shop CSV seeds from dbt-labs/jaffle-shop on GitHub,
creates a DuckLake catalog backed by a local DuckDB file, and loads
the data into DuckLake-managed tables.

Usage:
    python3 configure/setup_ducklake.py

The script is idempotent -- safe to run multiple times. Downloaded
files are cached in test/data/seeds/. DuckLake catalog and data
directories are recreated from scratch on each run.

Data files are gitignored per project convention.
"""

import os
import sys
import shutil
import urllib.request
from pathlib import Path

# Project root (one level up from configure/)
PROJECT_ROOT = Path(__file__).resolve().parent.parent

# Paths
SEEDS_DIR = PROJECT_ROOT / "test" / "data" / "seeds"
DATA_DIR = PROJECT_ROOT / "test" / "data"
CATALOG_DB = DATA_DIR / "test_catalog.duckdb"
DUCKLAKE_FILE = DATA_DIR / "jaffle.ducklake"
JAFFLE_DATA_DIR = DATA_DIR / "jaffle_data"

# jaffle-shop seed files on GitHub (under seeds/jaffle-data/)
SEED_BASE_URL = "https://raw.githubusercontent.com/dbt-labs/jaffle-shop/main/seeds/jaffle-data"
SEED_FILES = [
    "raw_orders.csv",
    "raw_customers.csv",
    "raw_items.csv",
]


def download_seeds():
    """Download jaffle-shop CSV files if not already cached."""
    SEEDS_DIR.mkdir(parents=True, exist_ok=True)
    for filename in SEED_FILES:
        dest = SEEDS_DIR / filename
        if dest.exists():
            print(f"  [cached] {filename}")
            continue
        url = f"{SEED_BASE_URL}/{filename}"
        print(f"  [download] {filename} from {url}")
        try:
            urllib.request.urlretrieve(url, dest)
        except Exception as e:
            print(f"  [skip] Failed to download {filename}: {e}")
            print("  Network access may not be available (CI). Skipping download.")
            return False
    return True


def create_ducklake_catalog():
    """Create a DuckLake catalog and load jaffle-shop data."""
    try:
        import duckdb
    except ImportError:
        print("ERROR: duckdb Python package not found.")
        print("Install with: pip install duckdb")
        sys.exit(1)

    # Clean up previous catalog files for idempotency
    for path in [CATALOG_DB, DUCKLAKE_FILE]:
        if path.exists():
            path.unlink()
    # Clean WAL files
    for wal in DATA_DIR.glob("*.wal"):
        wal.unlink()
    if JAFFLE_DATA_DIR.exists():
        shutil.rmtree(JAFFLE_DATA_DIR)

    DATA_DIR.mkdir(parents=True, exist_ok=True)
    JAFFLE_DATA_DIR.mkdir(parents=True, exist_ok=True)

    # Set extension directory inside the project to avoid needing ~/.duckdb
    ext_dir = str(DATA_DIR / "duckdb_extensions")
    os.makedirs(ext_dir, exist_ok=True)
    con = duckdb.connect(str(CATALOG_DB), config={"extension_directory": ext_dir})

    print("  Installing DuckLake extension...")
    con.execute("INSTALL ducklake")
    con.execute("LOAD ducklake")

    ducklake_uri = f"ducklake:{DUCKLAKE_FILE}"
    data_path = str(JAFFLE_DATA_DIR) + "/"
    print(f"  Creating DuckLake catalog at {DUCKLAKE_FILE}")
    con.execute(f"ATTACH '{ducklake_uri}' AS jaffle (DATA_PATH '{data_path}')")

    for filename in SEED_FILES:
        csv_path = SEEDS_DIR / filename
        if not csv_path.exists():
            print(f"  [skip] {filename} not found -- download may have failed")
            continue
        table_name = csv_path.stem  # e.g., raw_orders
        print(f"  Loading {filename} -> jaffle.{table_name}")
        con.execute(
            f"CREATE OR REPLACE TABLE jaffle.{table_name} AS SELECT * FROM read_csv('{csv_path}')"
        )

    # Verify
    tables = con.execute(
        "SELECT table_name FROM information_schema.tables "
        "WHERE table_catalog = 'jaffle' ORDER BY table_name"
    ).fetchall()
    print(f"  DuckLake tables: {[t[0] for t in tables]}")

    con.close()
    print("  DuckLake catalog setup complete.")


def main():
    print("Setting up DuckLake/Iceberg test environment...")
    print()

    print("Step 1: Download jaffle-shop seed data")
    seeds_ok = download_seeds()
    print()

    if not seeds_ok:
        print("Seed download failed. Cannot create DuckLake catalog.")
        print("Re-run with network access to download seed files.")
        sys.exit(1)

    print("Step 2: Create DuckLake catalog")
    create_ducklake_catalog()
    print()

    print("Done. Run `just test-iceberg` to execute the integration test.")


if __name__ == "__main__":
    main()
