.. meta::
   :description: Write a dimension as a dimension of a table its table references, such as an order's region read through its customer, so the logic is declared once

.. _howto-dimension-references:

============================================
How to Reuse a Dimension of a Related Table
============================================

.. versionadded:: 0.20.0

This guide shows how to define a dimension as a dimension of another table, such as each order's region read through its customer. Snowflake calls this a reference to another logical table's dimension. The expression is written once, on the table that holds it, and every dimension that references it reads the same value along its own relationships.

**Prerequisites:**

- A working multi-table semantic view (see :ref:`tutorial-multi-table`)
- Familiarity with the ``RELATIONSHIPS`` clause (see :ref:`ref-create-semantic-view`)


.. _howto-dimref-setup:

Set Up Example Tables
=====================

Two regions, three customers (Cid has no region) and four orders:

.. code-block:: sql

   CREATE TABLE ref_regions (region_id INTEGER, region_name VARCHAR);
   INSERT INTO ref_regions VALUES (1, 'Europe'), (2, 'Americas');

   CREATE TABLE ref_customers (customer_id INTEGER, name VARCHAR, region_id INTEGER);
   INSERT INTO ref_customers VALUES (10, 'Ann', 1), (20, 'Bob', 2), (30, 'Cid', NULL);

   CREATE TABLE ref_orders (order_id INTEGER, customer_id INTEGER, amount DECIMAL(10,2));
   INSERT INTO ref_orders VALUES (1, 10, 30), (2, 10, 20), (3, 20, 50), (4, 30, 15);


.. _howto-dimref-declare:

Reference Another Table's Dimension
===================================

Write the other table's dimension, ``<alias>.<dimension name>``, where a column expression would go. ``customer.customer_region`` reads the region's ``region_name`` dimension, and ``orders.order_region`` reads ``customer.customer_region`` in turn. A reference can also sit inside an expression, as in ``region_label``:

.. code-block:: sql
   :emphasize-lines: 13-15

   CREATE SEMANTIC VIEW regional_orders
   TABLES (
       region   AS ref_regions   PRIMARY KEY (region_id),
       customer AS ref_customers PRIMARY KEY (customer_id),
       orders   AS ref_orders    PRIMARY KEY (order_id)
   )
   RELATIONSHIPS (
       customer_region AS customer(region_id) REFERENCES region,
       order_customer  AS orders(customer_id) REFERENCES customer
   )
   DIMENSIONS (
       region.region_name       AS region_name,
       customer.customer_region AS region.region_name,
       orders.order_region      AS customer.customer_region,
       customer.region_label    AS UPPER(region.region_name),
       customer.customer_name   AS name
   )
   METRICS (
       orders.revenue AS SUM(amount)
   );

Each customer reads the name of its own region. Cid, who has none, gets ``NULL``:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(regional_orders
       DIMENSIONS customer.customer_name, customer.customer_region, customer.region_label
   ) ORDER BY 1;

.. code-block:: text

   ┌───────────────┬─────────────────┬──────────────┐
   │ customer_name │ customer_region │ region_label │
   ├───────────────┼─────────────────┼──────────────┤
   │ Ann           │ Europe          │ EUROPE       │
   │ Bob           │ Americas        │ AMERICAS     │
   │ Cid           │ NULL            │ NULL         │
   └───────────────┴─────────────────┴──────────────┘


.. _howto-dimref-query:

Group and Filter by a Reference
===============================

A reference is queried like any dimension. ``orders.order_region`` groups each order by its customer's region, two relationships away:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(regional_orders
       DIMENSIONS orders.order_region
       METRICS orders.revenue
   ) ORDER BY 1 NULLS LAST;

.. code-block:: text

   ┌──────────────┬───────────────┐
   │ order_region │    revenue    │
   ├──────────────┼───────────────┤
   │ Americas     │         50.00 │
   │ Europe       │         50.00 │
   │ NULL         │         15.00 │
   └──────────────┴───────────────┘

A filter works the same way: ``WHERE orders.order_region = 'Europe'`` keeps Ann's two orders, 50.00. ``DESCRIBE SEMANTIC VIEW``, ``GET_DDL`` and the YAML export show a reference as written, ``region.region_name``.

.. tip::

   A reference is read along the referencing dimension's own path, whatever the query's metrics are. If orders also reached regions through a store, ``customer.customer_region`` would still be the customer's region, also for a metric whose ``USING`` names the store relationship.


.. _howto-dimref-limits:

What Is Refused
===============

``CREATE`` refuses, as Snowflake does:

- **A reference to a table that references the dimension's table.** A customer has several orders, so an order dimension has no single value per customer:

  .. code-block:: text

     Parser Error: dimension 'customer.order_amount' references dimension 'orders.amount_d', but table 'customer' does not reach table 'orders' along its relationships, so 'amount_d' has no single value per row of 'customer'. As in Snowflake, a dimension can reference a dimension of a table it references, directly or through other tables, not of a table that references it.

- **A reference to a table reached by more than one path**, such as a role-played table, since the path is not said.
- **References that form a cycle.**

See :ref:`dimension references <ref-create-dimension-references>` for the full rules.


.. _howto-dimref-related:

Related
=======

- :ref:`Dimension references <ref-create-dimension-references>` -- The rules in the ``CREATE SEMANTIC VIEW`` reference
- :ref:`howto-facts` -- Reusable row-level expressions on one table
- :ref:`howto-role-playing` -- A table referenced more than once
