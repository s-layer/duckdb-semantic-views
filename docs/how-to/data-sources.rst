.. meta::
   :description: Connect semantic views to Parquet files, CSV, Apache Iceberg tables, Postgres, and mixed data sources that DuckDB can query

.. _howto-data-sources:

==========================================
How to Use Different Data Sources
==========================================

This guide shows how to define semantic views over tables from various data sources that DuckDB supports. Semantic views work over any table that DuckDB can see: Parquet files, CSV files, Iceberg tables, Postgres tables, or any other source accessible through a DuckDB extension.

**Prerequisites:**

- Completed the :ref:`tutorial-getting-started` tutorial
- DuckDB installed with the relevant data source extensions


.. _howto-ds-parquet:

Parquet Files
=============

Create a DuckDB table from a Parquet file, then define a semantic view over it:

.. code-block:: sql

   CREATE TABLE orders AS SELECT * FROM read_parquet('orders.parquet');
   CREATE TABLE customers AS SELECT * FROM read_parquet('customers.parquet');

   CREATE SEMANTIC VIEW analytics AS
   TABLES (
       o AS orders    PRIMARY KEY (order_id),
       c AS customers PRIMARY KEY (customer_id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (
       c.name   AS c.customer_name,
       o.region AS o.region
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

Alternatively, create a view over the Parquet file and use that in the semantic view:

.. code-block:: sql

   CREATE VIEW orders AS SELECT * FROM read_parquet('orders.parquet');


.. _howto-ds-csv:

CSV Files
=========

.. code-block:: sql

   CREATE TABLE products AS SELECT * FROM read_csv('products.csv',
       auto_detect=true
   );

Then define a semantic view over the ``products`` table as normal.


.. _howto-ds-iceberg:

Iceberg Tables
==============

Load the ``iceberg`` extension and scan Iceberg tables:

.. code-block:: sql

   INSTALL iceberg;
   LOAD iceberg;

   CREATE TABLE orders AS
       SELECT * FROM iceberg_scan('s3://my-bucket/warehouse/orders');

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (order_id)
   )
   DIMENSIONS (
       o.region   AS o.region,
       o.category AS o.category
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   );

S3 Credentials
--------------

DuckDB needs S3 credentials to read from cloud storage. Create a secret before running ``iceberg_scan``, either with explicit keys or from the AWS SDK credential chain:

.. code-block:: sql

   -- Explicit keys
   CREATE SECRET (
       TYPE s3,
       KEY_ID 'AKIA...',
       SECRET '...',
       REGION 'us-east-1'
   );

   -- Or: credentials from environment variables, ~/.aws config,
   -- or an instance profile (uses DuckDB's aws extension)
   CREATE SECRET (
       TYPE s3,
       PROVIDER credential_chain
   );

See the DuckDB `S3 API <https://duckdb.org/docs/stable/core_extensions/httpfs/s3api>`_ and `aws extension <https://duckdb.org/docs/stable/core_extensions/aws>`_ documentation for the other secret options, such as named profiles and assumed roles.

Iceberg Catalog Types
---------------------

The ``iceberg_scan`` function reads directly from the Iceberg metadata path. If your tables are managed by an Iceberg catalog (Hive metastore, AWS Glue, REST catalog), point to the metadata location that the catalog provides:

.. code-block:: sql

   -- Direct metadata path
   CREATE TABLE orders AS
       SELECT * FROM iceberg_scan('s3://bucket/warehouse/orders/metadata/v3.metadata.json');

   -- Or use the table path if the latest metadata pointer exists
   CREATE TABLE orders AS
       SELECT * FROM iceberg_scan('s3://bucket/warehouse/orders');

Using a View Instead of a Table
-------------------------------

Creating a ``VIEW`` instead of a ``TABLE`` keeps queries reading the latest Iceberg snapshot rather than a static copy:

.. code-block:: sql

   CREATE VIEW orders AS
       SELECT * FROM iceberg_scan('s3://my-bucket/warehouse/orders');

This is useful when the underlying Iceberg table changes frequently. The trade-off is that each :ref:`semantic-view query <ref-semantic-view-function>` re-scans the Iceberg metadata.

.. _howto-ds-schema-evolution:

Schema Evolution
----------------

If columns are added or removed from the Iceberg table, update the semantic view to match:

- **New column:** Add a dimension or metric referencing the new column, then ``CREATE OR REPLACE SEMANTIC VIEW`` to update the definition.
- **Removed column:** Any dimension or metric referencing the dropped column will cause a query-time SQL error. Update the semantic view definition to remove those references.

.. tip::

   For a DuckDB + Iceberg + analytics application stack, semantic views give
   the application a stable query interface over Iceberg tables. The
   application queries :ref:`SEMANTIC_VIEW(...) <ref-semantic-view-function>` with
   dimension and metric names, and the extension writes the joins and
   aggregation. Three pages cover the rest of that stack:

   - :ref:`explanation-txn-ddl-readonly` -- define the views once in a writable
     database file, then open it read-only in the application.
   - :ref:`explanation-txn-ddl-attach` -- run semantic-view DDL from the
     database you loaded the extension into, not from an ``ATTACH``-ed one.
   - :ref:`howto-filtering-app` -- pass a user's date range or segment into a
     query without building SQL from request input.


.. _howto-ds-postgres:

Postgres via the ``postgres`` Extension
=======================================

Attach a Postgres database with DuckDB's ``postgres`` extension, then either copy its tables into DuckDB or reference them where they are. To copy them:

.. code-block:: sql

   INSTALL postgres;
   LOAD postgres;

   ATTACH 'dbname=mydb user=myuser host=localhost' AS pg (TYPE POSTGRES);

   CREATE TABLE orders AS SELECT * FROM pg.public.orders;
   CREATE TABLE customers AS SELECT * FROM pg.public.customers;

Then define a semantic view over the local ``orders`` and ``customers`` tables as usual.

To read from Postgres at query time instead of from a copy, name the attached tables with their catalog-qualified names in the ``TABLES`` clause:

.. code-block:: sql

   CREATE SEMANTIC VIEW pg_analytics AS
   TABLES (
       o AS pg.public.orders    PRIMARY KEY (order_id),
       c AS pg.public.customers PRIMARY KEY (customer_id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (
       c.name AS c.customer_name
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

Each :ref:`semantic-view query <ref-semantic-view-function>` then scans Postgres through the attachment, so ``pg`` must be attached in every session that queries the view. Run the ``CREATE SEMANTIC VIEW`` itself while your session is on the database you loaded the extension into -- do not ``USE pg`` first. :ref:`explanation-txn-ddl-attach` explains why.


.. _howto-ds-mixed:

Mixed Sources
=============

Semantic views work with any combination of sources. The only requirement is that each table exists in DuckDB at query time.

.. code-block:: sql

   -- Orders from Iceberg
   CREATE TABLE orders AS
       SELECT * FROM iceberg_scan('s3://bucket/warehouse/orders');

   -- Customers from Postgres
   CREATE TABLE customers AS
       SELECT * FROM pg.public.customers;

   -- Products from a local Parquet file
   CREATE TABLE products AS
       SELECT * FROM read_parquet('products.parquet');

   CREATE SEMANTIC VIEW analytics AS
   TABLES (
       o AS orders    PRIMARY KEY (order_id),
       c AS customers PRIMARY KEY (customer_id),
       p AS products  PRIMARY KEY (product_id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       order_product  AS o(product_id)  REFERENCES p
   )
   DIMENSIONS (
       c.customer AS c.name,
       p.product  AS p.name,
       o.region   AS o.region
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   );


.. _howto-ds-catalog:

Catalog-Qualified Table Names
=============================

If your tables live in a specific catalog or schema, use the fully qualified table name in the ``TABLES`` clause:

.. code-block:: sql

   CREATE SEMANTIC VIEW analytics AS
   TABLES (
       o AS my_catalog.my_schema.orders PRIMARY KEY (order_id)
   )
   DIMENSIONS (
       o.region AS o.region
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

The extension quotes each segment of the table name separately (``"my_catalog"."my_schema"."orders"``) in the generated SQL.


.. _howto-ds-troubleshooting:

Troubleshooting
===============

**Catalog Error: Table with name ... does not exist**
   ``CREATE SEMANTIC VIEW`` does not check that the tables in ``TABLES``
   exist; they are looked up when you query the view. This error means a source
   table is missing from the current session -- a ``CREATE TABLE ... AS`` copy
   was dropped, or an attached database such as ``pg`` has not been attached
   again after reconnecting. Recreate the table or re-run the ``ATTACH``.

**Binder Error: Table "o" does not have a column named ...**
   A column that a dimension, metric or fact reads was removed from the source,
   for example by Iceberg schema evolution. Update the view with ``CREATE OR
   REPLACE SEMANTIC VIEW`` (see :ref:`howto-ds-schema-evolution`).

**semantic-view DDL was issued against database 'pg', but the semantic view catalog lives in a different database**
   The session had switched to an attached database with ``USE`` when it ran
   ``CREATE``, ``ALTER`` or ``DROP SEMANTIC VIEW``. Switch back to the database
   you loaded the extension into and run the DDL again. Qualified table names
   such as ``pg.public.orders`` inside the view body are fine.

**diamond: '...' is reachable from multiple tables**
   The view reaches one table from two different source tables, which makes
   the join path ambiguous. This often happens when two sources share a lookup
   table, such as ``regions``. Declare the shared table under a second alias;
   see :ref:`howto-rp-diamond`.


.. _howto-ds-related:

Related
=======

- :ref:`ref-create-semantic-view` -- Full ``TABLES`` and ``RELATIONSHIPS`` syntax
- :ref:`explanation-transactional-ddl` -- Read-only databases, attached databases and other deployment limits
- :ref:`howto-filtering` -- Scope a query to a date range or segment before aggregation
- :ref:`howto-materializations` -- Route common queries to a pre-aggregated table instead of re-scanning the source
