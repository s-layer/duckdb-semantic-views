#!/usr/bin/env python3
# /// script
# dependencies = [
#   "duckdb==2.0.0.dev2610151406",
#   "adbc-driver-manager>=1.10",
#   "pyarrow>=16",
# ]
# requires-python = ">=3.10"
# ///
# Note: `import adbc_driver_duckdb` resolves to the module bundled inside
# the `duckdb` wheel (see its dist-info/RECORD). There is no
# separate `adbc-driver-duckdb` package on PyPI, so it does not appear in
# the dependencies list above.
"""
uv run examples/transactional_ddl.py

Demonstrates v0.8.0 features:
  - Transactional CREATE / DROP / ALTER SEMANTIC VIEW
  - BEGIN / ROLLBACK actually rolls back catalog changes
  - Works through ADBC's autocommit=False mode (the original motivating use case)

Two scenarios:
  1. Native DuckDB connection with explicit BEGIN / COMMIT / ROLLBACK.
  2. ADBC DBAPI connection (autocommit=False) using commit() / rollback().

Both use the same parser_override mechanism under the hood — DDL is rewritten
into native INSERT / UPDATE / DELETE against the catalog table and runs on
the caller's connection, so transaction semantics work exactly as you would
expect for any other DML.
"""

import os
import tempfile

import adbc_driver_duckdb
import adbc_driver_manager
import adbc_driver_manager.dbapi
import duckdb

EXTENSION_PATH = os.environ.get(
    "SEMANTIC_VIEWS_EXTENSION_PATH",
    "build/debug/semantic_views.duckdb_extension",
)


def setup(con) -> None:
    con.execute(
        """
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            region VARCHAR,
            amount DECIMAL(10,2)
        );
        INSERT INTO orders VALUES
            (1, 'US', 100.00),
            (2, 'EU', 200.00),
            (3, 'US', 150.00);
        """
    )


CREATE_VIEW = """
CREATE SEMANTIC VIEW orders_view AS
  TABLES (o AS orders PRIMARY KEY (id))
  DIMENSIONS (o.region AS o.region)
  METRICS (o.total AS SUM(o.amount))
"""


def list_views(con) -> list[str]:
    rows = con.execute("SELECT name FROM list_semantic_views()").fetchall()
    return [r[0] for r in rows]


def adbc_list_views(conn) -> list[str]:
    with conn.cursor() as cur:
        cur.execute("SELECT name FROM list_semantic_views()")
        return [r[0] for r in cur.fetchall()]


def native_demo() -> None:
    print("=== Scenario 1: native DuckDB BEGIN / ROLLBACK ===\n")
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{EXTENSION_PATH}'")
    setup(con)

    print("Before:        ", list_views(con))

    con.execute("BEGIN")
    con.execute(CREATE_VIEW)
    con.execute("ROLLBACK")
    print("After rollback:", list_views(con))
    print("  -> view did NOT persist (correct).")

    con.execute("BEGIN")
    con.execute(CREATE_VIEW)
    con.execute("COMMIT")
    print("After commit:  ", list_views(con))
    print("  -> view persisted (correct).\n")

    # ALTER and DROP also participate.
    con.execute("BEGIN")
    con.execute("DROP SEMANTIC VIEW orders_view")
    con.execute("ROLLBACK")
    print("After DROP+rollback:", list_views(con))
    print("  -> DROP rolled back, view still present.\n")

    # Reads run in your transaction, so a view created in it is listed (and
    # can be queried) before COMMIT. Other connections see it only after.
    second_view = CREATE_VIEW.replace("orders_view", "orders_view_2")
    con.execute("BEGIN")
    con.execute(second_view)
    print("In-flight (same transaction):", list_views(con))
    con.execute("ROLLBACK")
    print("After rollback:", list_views(con))
    print()

    con.close()


def adbc_demo() -> None:
    print("=== Scenario 2: ADBC autocommit=False ===\n")
    with tempfile.TemporaryDirectory(prefix="sv_adbc_demo_") as tmp:
        db_path = os.path.join(tmp, "demo.duckdb")
        # The high-level adbc_driver_duckdb.dbapi.connect() does not expose
        # DBConfig, so we use AdbcDatabase directly. DuckDB's ADBC driver
        # passes any unrecognised key through duckdb_set_config().
        db = adbc_driver_manager.AdbcDatabase(
            driver=adbc_driver_duckdb.driver_path(),
            entrypoint="duckdb_adbc_init",
            path=db_path,
            allow_unsigned_extensions="true",
        )
        raw = adbc_driver_manager.AdbcConnection(db)
        conn = adbc_driver_manager.dbapi.Connection(db, raw, autocommit=False)
        try:
            with conn.cursor() as cur:
                cur.execute(f"LOAD '{EXTENSION_PATH}'")
                cur.execute(
                    "CREATE TABLE orders (id INTEGER PRIMARY KEY, "
                    "region VARCHAR, amount DECIMAL(10,2))"
                )
                cur.execute(
                    "INSERT INTO orders VALUES "
                    "(1, 'US', 100.00), (2, 'EU', 200.00)"
                )
            conn.commit()

            print("Before:        ", adbc_list_views(conn))

            with conn.cursor() as cur:
                cur.execute(CREATE_VIEW)
            conn.rollback()
            print("After rollback:", adbc_list_views(conn))
            print("  -> view did NOT persist (the v0.8.0 fix).")
            conn.commit()

            with conn.cursor() as cur:
                cur.execute(CREATE_VIEW)
            conn.commit()
            print("After commit:  ", adbc_list_views(conn))
            print("  -> view persisted (correct).")
            conn.commit()
        finally:
            conn.close()


if __name__ == "__main__":
    native_demo()
    adbc_demo()
