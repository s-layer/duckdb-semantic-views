.. meta::
   :description: Define a logical table in TABLES by a query, to join, filter or pre-aggregate source tables inside the semantic view itself

.. _howto-query-tables:

========================================
How to Define a Logical Table by a Query
========================================

.. versionadded:: 0.18.0

This guide shows how to make a ``TABLES`` entry a query instead of a physical table, as Snowflake allows. Use it when the table you want to model does not exist as one table: customer details spread over two tables, only the paid orders, or a per-customer total that a dimension buckets. The query lives in the semantic view's definition, so :ref:`GET_DDL <ref-get-ddl>` and the YAML export carry it with the rest of the model.

**Prerequisites:**

- A working multi-table semantic view (see :ref:`tutorial-multi-table`)
- Familiarity with the ``TABLES`` and ``RELATIONSHIPS`` clauses (see :ref:`ref-create-tables`)


.. _howto-query-tables-setup:

Set Up Example Tables
=====================

The examples model customers, whose city sits in a separate address table, and orders, some of which were refunded:

.. code-block:: sql

   CREATE TABLE customers (id INTEGER, name VARCHAR, segment VARCHAR);
   INSERT INTO customers VALUES
       (1, 'Acme',    'enterprise'),
       (2, 'Globex',  'smb'),
       (3, 'Initech', 'smb');

   CREATE TABLE customer_addresses (customer_id INTEGER, city VARCHAR, country VARCHAR);
   INSERT INTO customer_addresses VALUES
       (1, 'Oslo', 'NO'),
       (2, 'Rome', 'IT'),
       (3, 'Oslo', 'NO');

   CREATE TABLE orders (id INTEGER, customer_id INTEGER, amount DECIMAL(10,2), status VARCHAR);
   INSERT INTO orders VALUES
       (10, 1, 500.00, 'paid'),
       (11, 1, 250.00, 'paid'),
       (12, 2, 120.00, 'paid'),
       (13, 3,  80.00, 'refunded'),
       (14, 3, 300.00, 'paid');


.. _howto-query-tables-define:

Join and Filter Source Tables in TABLES
=======================================

Write the query in parentheses after the alias and ``AS``. The alias is required, and the table's other keywords -- ``PRIMARY KEY``, ``UNIQUE``, ``COMMENT``, ``WITH SYNONYMS``, tags -- follow the closing parenthesis, as they follow a table name.

Here ``o`` keeps only paid orders, and ``c`` joins each customer to its address so that ``city`` is a column of the customer:

.. code-block:: sql
   :emphasize-lines: 3-8

   CREATE SEMANTIC VIEW customer_sales
   TABLES (
       o AS (SELECT * FROM orders WHERE status = 'paid')
           PRIMARY KEY (id) COMMENT = 'Paid orders only',
       c AS (
           SELECT * FROM customers
           JOIN customer_addresses ON customers.id = customer_addresses.customer_id
       ) PRIMARY KEY (id) WITH SYNONYMS = ('customer_details')
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (
       c.city    AS c.city,
       c.segment AS c.segment
   )
   METRICS (
       o.revenue        AS SUM(o.amount),
       o.order_count    AS COUNT(*),
       c.customer_count AS COUNT(*)
   );

Members read the query's output columns through the alias (``c.city``), and the relationship joins ``o`` to ``c`` as it would join two physical tables. Every query of the view reads each table through its query, so the refunded order never reaches ``revenue``:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(customer_sales
       DIMENSIONS city
       METRICS revenue, order_count, customer_count
   ) ORDER BY city;

.. code-block:: text

   ┌──────┬─────────┬─────────────┬────────────────┐
   │ city │ revenue │ order_count │ customer_count │
   ├──────┼─────────┼─────────────┼────────────────┤
   │ Oslo │ 1050.00 │           3 │              2 │
   │ Rome │  120.00 │           1 │              1 │
   └──────┴─────────┴─────────────┴────────────────┘

.. warning::

   ``PRIMARY KEY`` on a query-defined table is a promise you make about the query's output, as it is on a physical table: the extension does not check it. If the query can return two rows for one key -- a customer with two addresses, say -- every metric joined through that table counts the customer's rows twice. Make the query return one row per key, for example by picking one address per customer, before declaring the key.

Put a filter that defines the table -- "only paid orders", "no internal test accounts" -- in the table's query, where every metric sees it. A filter that only some questions need belongs in the ``WHERE`` inside ``SEMANTIC_VIEW(...)`` instead (see :ref:`howto-filtering`).


.. _howto-query-tables-aggregate:

Pre-Aggregate to a New Grain
============================

The query may aggregate. That gives you a table at a grain no source table has, such as one row per customer with the customer's lifetime revenue, which a dimension can then bucket. Add it to the view as a table the customer references:

.. code-block:: sql
   :emphasize-lines: 9-12,16,21

   CREATE OR REPLACE SEMANTIC VIEW customer_sales
   TABLES (
       o AS (SELECT * FROM orders WHERE status = 'paid')
           PRIMARY KEY (id) COMMENT = 'Paid orders only',
       c AS (
           SELECT * FROM customers
           JOIN customer_addresses ON customers.id = customer_addresses.customer_id
       ) PRIMARY KEY (id) WITH SYNONYMS = ('customer_details'),
       ltv AS (
           SELECT customer_id, SUM(amount) AS lifetime
           FROM orders WHERE status = 'paid' GROUP BY customer_id
       ) PRIMARY KEY (customer_id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       customer_ltv   AS c(id)          REFERENCES ltv
   )
   DIMENSIONS (
       c.city    AS c.city,
       c.segment AS c.segment,
       ltv.value_tier AS CASE WHEN ltv.lifetime >= 500 THEN 'high' ELSE 'standard' END
   )
   METRICS (
       o.revenue        AS SUM(o.amount),
       o.order_count    AS COUNT(*),
       c.customer_count AS COUNT(*)
   );

``customer_ltv`` joins on the customer's own key: its foreign-key column ``c.id`` is ``c``'s primary key, so each customer has at most one ``ltv`` row and the extension infers the relationship as one-to-one. Count customers and their revenue per tier:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(customer_sales
       DIMENSIONS value_tier
       METRICS customer_count, revenue
   ) ORDER BY value_tier;

.. code-block:: text

   ┌────────────┬────────────────┬─────────┐
   │ value_tier │ customer_count │ revenue │
   ├────────────┼────────────────┼─────────┤
   │ high       │              1 │  750.00 │
   │ standard   │              2 │  420.00 │
   └────────────┴────────────────┴─────────┘

Acme's two paid orders total 750, so Acme is ``high``; Globex (120) and Initech (300) are ``standard``.


.. _howto-query-tables-files:

Read Files or Table Functions Directly
======================================

The query can read anything a DuckDB ``SELECT`` can, table functions included, so a view can sit over Parquet files without a separate ``CREATE TABLE`` or ``CREATE VIEW``:

.. code-block:: sql

   CREATE SEMANTIC VIEW file_sales
   TABLES (
       o AS (SELECT * FROM read_parquet('data/orders/*.parquet')) PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.status AS o.status
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

Each query of the view re-reads the files, as a DuckDB view over them would (see :ref:`howto-data-sources`). Prefer a DuckDB ``CREATE VIEW`` when several semantic views, or other queries, read the same source: it gives the source one definition. Prefer a query-defined table when the shape belongs to this model alone and should travel with it in ``GET_DDL`` and YAML.


.. _howto-query-tables-resolution:

How Table Names in the Query Resolve
====================================

The query is bound at ``CREATE``, like every table in ``TABLES``, so a query that names a missing table or column refuses the ``CREATE``. Its table names resolve the way a DuckDB view body's do:

- **In the schema the view was created in**, whichever schema the querying session is in. A view created from ``main`` keeps reading ``main.orders`` after ``USE staging``.
- **Never as a CTE of the querying statement.** ``WITH orders AS (...) SELECT * FROM SEMANTIC_VIEW(customer_sales ...)`` still reads the ``orders`` table.
- **A temporary table** the query reads is found in the session that has it.

Write a schema-qualified name (``analytics.orders``) in the query when the view is meant to read a table outside its own schema.

The query is one ``SELECT``. It cannot hold a ``;`` or a prepared-statement parameter (``$1``, ``?``), since nothing supplies a value when the view is read: put a value that varies per query in the ``WHERE`` inside ``SEMANTIC_VIEW(...)``, or in a :ref:`view variable <howto-variables>` read by a member.


.. _howto-query-tables-inspect:

Inspect and Export the Definition
=================================

:ref:`explain_semantic_view() <ref-explain-semantic-view>` shows each table's query in the generated SQL, in place of a table name:

.. code-block:: duckdb-sql

   SELECT * FROM explain_semantic_view('customer_sales',
       dimensions := ['city'],
       metrics := ['revenue']
   );

.. code-block:: text

   -- Expanded SQL:
   SELECT
       c.city AS "city",
       SUM(o.amount) AS "revenue"
   FROM (
   SELECT * FROM orders WHERE status = 'paid'
   ) AS "o"
   LEFT JOIN (
   SELECT * FROM customers
              JOIN customer_addresses ON customers.id = customer_addresses.customer_id
   ) AS "c" ON "o"."customer_id" = "c"."id"
   GROUP BY
       1

:ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` shows the query in a ``DEFINITION`` row, in place of the ``BASE_TABLE_*`` rows a physical table has:

.. code-block:: sql

   SELECT object_name, property, property_value
   FROM (DESCRIBE SEMANTIC VIEW customer_sales)
   WHERE object_kind = 'TABLE' AND object_name = 'o';

.. code-block:: text

   ┌─────────────┬─────────────┬────────────────────────────────────────────┐
   │ object_name │  property   │               property_value               │
   ├─────────────┼─────────────┼────────────────────────────────────────────┤
   │ o           │ DEFINITION  │ SELECT * FROM orders WHERE status = 'paid' │
   │ o           │ PRIMARY_KEY │ ["id"]                                     │
   │ o           │ COMMENT     │ Paid orders only                           │
   └─────────────┴─────────────┴────────────────────────────────────────────┘

:ref:`GET_DDL <ref-get-ddl>` writes the query back between its parentheses, and :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>` writes it as ``definition:`` in place of ``table:``. In a YAML definition you write by hand, give a table one or the other:

.. code-block:: yaml

   tables:
     - alias: o
       definition: SELECT * FROM orders WHERE status = 'paid'
       pk_columns: [id]

SQL comments in the query are not kept. In the DDL form they are dropped when the view is stored, and a YAML ``definition`` that contains one is refused.

.. warning::

   A view with a query-defined table cannot be read by an extension release older than 0.18.0, which reports its stored definition as invalid (``missing field 'table'``). Upgrade every DuckDB process that opens the database file before you create such a view.


.. _howto-query-tables-troubleshooting:

Troubleshooting
===============

**An inline query in TABLES needs an alias: write 'alias AS (<query>)'**
   A query in ``TABLES`` has no alias to name its columns by. Write ``paid AS (SELECT ...)``.

**semantic view 'customer_sales': dimension 'city' does not bind: Catalog Error: Table with name ... does not exist**
   A table's query names a table that does not exist, or that exists in another schema than the one the view is created in. The error names the first member that reads the table, or the table itself (``table 'c' does not bind``) when no member does. Create the table, or qualify its name in the query.

**semantic view 'customer_sales': dimension 'city' does not bind**
   A member reads a column the query does not return. Check the query's output columns: a column the query renames (``SUM(amount) AS lifetime``) is read by its new name.

**Invalid inline query for table 'o': it holds the prepared statement parameter $1**
   A table's query cannot take a parameter. Filter with the ``WHERE`` of each ``SEMANTIC_VIEW(...)`` query, which can (see :ref:`howto-filtering-app`).

**table 'o' has an invalid inline query: it holds a SQL comment**
   Remove the comment from the YAML ``definition``.

See :ref:`ref-err-inline-query` for every message.


.. _howto-query-tables-related:

Related
=======

- :ref:`Tables defined by a query <ref-create-inline-query>` -- Syntax and rules in the ``CREATE SEMANTIC VIEW`` reference
- :ref:`howto-data-sources` -- Connecting Parquet, CSV, Iceberg and database sources
- :ref:`howto-fan-traps` -- What a key that is not unique does to metrics
- :ref:`ref-yaml-format` -- The ``definition`` field in the YAML format
