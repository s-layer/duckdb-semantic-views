.. meta::
   :description: Compute a value at a fixed grain, such as orders per customer averaged by region or a share of the grand total, with LOD expressions and FIXED DIMENSIONS

.. _howto-lod:

========================================================
How to Compute Values at a Fixed Grain (LOD Expressions)
========================================================

.. versionadded:: 0.20.0

This guide shows how to compute a value at a grain other than the query's, such as each customer's order count averaged by region, or each region's revenue as a share of the grand total. You write the value as a **level-of-detail (LOD) expression**: an aggregate followed by ``FIXED DIMENSIONS (...)``, the dimensions it is computed at. The syntax and the answers are Snowflake's.

**Prerequisites:**

- A working multi-table semantic view (see :ref:`tutorial-multi-table`)
- Familiarity with how a metric is counted below its grain (see :ref:`howto-fan-traps`)


.. _howto-lod-setup:

Set Up Example Tables
=====================

Four customers in two regions and their ten orders. Alice has four orders, Bob two, Carol three and Dan one:

.. code-block:: sql

   CREATE TABLE customers (customer_id INTEGER, name VARCHAR, region VARCHAR);
   INSERT INTO customers VALUES
       (1, 'Alice', 'North'), (2, 'Bob', 'North'),
       (3, 'Carol', 'South'), (4, 'Dan',  'South');

   CREATE TABLE orders (order_id INTEGER, customer_id INTEGER, category VARCHAR, amount DECIMAL(10,2));
   INSERT INTO orders VALUES
       (101, 1, 'Electronics', 400), (102, 1, 'Electronics', 200),
       (103, 1, 'Clothing',    300), (104, 1, 'Clothing',    100),
       (105, 2, 'Electronics', 200), (106, 2, 'Clothing',    100),
       (107, 3, 'Electronics', 300), (108, 3, 'Electronics', 200),
       (109, 3, 'Clothing',    100), (110, 4, 'Electronics', 200);


.. _howto-lod-define:

Declare LOD Expressions
=======================

An LOD expression is declared as a fact or a dimension **without a table prefix**, and a metric that reads one has no table prefix either:

.. code-block:: sql
   :emphasize-lines: 11,12,20,24,25,26

   CREATE SEMANTIC VIEW sales
   TABLES (
       customers AS customers PRIMARY KEY (customer_id),
       orders    AS orders
   )
   RELATIONSHIPS (
       orders(customer_id) REFERENCES customers
   )
   FACTS (
       orders.order_amount AS amount,
       orders_per_customer AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id),
       grand_total_revenue AS SUM(orders.order_amount) FIXED DIMENSIONS ()
   )
   DIMENSIONS (
       customers.customer_id AS customer_id,
       customers.region      AS region,
       orders.order_id       AS order_id,
       orders.category       AS category,
       orders_by_region AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.region)
   )
   METRICS (
       orders.total_revenue    AS SUM(amount),
       avg_orders_per_customer AS AVG(orders_per_customer),
       pct_of_total            AS SUM(orders.order_amount) / AVG(grand_total_revenue),
       avg_orders_inline       AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id))
   );

Each expression is one aggregate over the view's facts and dimensions, followed by the dimensions it is fixed at, each named as ``<table>.<name>``. ``FIXED DIMENSIONS ()`` is the grand total. The table it aggregates (``orders`` here) must reference every table its ``FIXED DIMENSIONS`` read, directly or through other tables. ``orders`` needs no ``PRIMARY KEY``.


.. _howto-lod-metric:

Aggregate an LOD Fact
=====================

``orders_per_customer`` is each customer's order count. Averaged by region, it is the average customer's order count in each region:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customers.region
       METRICS avg_orders_per_customer
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬─────────────────────────┐
   │ region │ avg_orders_per_customer │
   ├────────┼─────────────────────────┤
   │ North  │                     3.0 │
   │ South  │                     2.0 │
   └────────┴─────────────────────────┘

North averages Alice's 4 and Bob's 2, South Carol's 3 and Dan's 1. Each customer counts once, however many orders it has: a plain ``AVG`` over the joined orders would weight Alice four times. ``avg_orders_inline``, which writes the same expression inside the metric, gives the same numbers.

A metric can read an LOD fact beside a plain aggregate. ``pct_of_total`` divides each region's revenue by the grand total, whatever the query groups by:

.. code-block:: sql

   SELECT region, total_revenue, round(pct_of_total, 3) AS pct FROM SEMANTIC_VIEW(sales
       DIMENSIONS customers.region
       METRICS orders.total_revenue, pct_of_total
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬───────────────┬───────┐
   │ region │ total_revenue │  pct  │
   ├────────┼───────────────┼───────┤
   │ North  │       1300.00 │ 0.619 │
   │ South  │        800.00 │ 0.381 │
   └────────┴───────────────┴───────┘

Grouped by an order dimension, each customer counts once in each group its orders are in. By category, Alice, Bob and Carol all have clothing orders, so ``Clothing`` averages 4, 2 and 3:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS orders.category
       METRICS avg_orders_per_customer
   ) ORDER BY category;

.. code-block:: text

   ┌─────────────┬─────────────────────────┐
   │  category   │ avg_orders_per_customer │
   ├─────────────┼─────────────────────────┤
   │ Clothing    │                     3.0 │
   │ Electronics │                     2.5 │
   └─────────────┴─────────────────────────┘

A query can also aggregate an LOD fact itself, as in ``METRICS AVG(orders_per_customer)``.


.. _howto-lod-dimension:

Group by an LOD Dimension
=========================

An LOD dimension gives each row its value at the fixed grain. ``orders_by_region`` is the order count of the row's region, 6 for North and 4 for South, and a query can group or filter by it:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customers.customer_id, orders_by_region
       METRICS orders.total_revenue
   ) ORDER BY customer_id;

.. code-block:: text

   ┌─────────────┬──────────────────┬───────────────┐
   │ customer_id │ orders_by_region │ total_revenue │
   ├─────────────┼──────────────────┼───────────────┤
   │           1 │                6 │       1000.00 │
   │           2 │                6 │        300.00 │
   │           3 │                4 │        600.00 │
   │           4 │                4 │        200.00 │
   └─────────────┴──────────────────┴───────────────┘


.. _howto-lod-filters:

Filters Apply After the Calculation
===================================

A ``FIXED`` expression is computed over every row, before the query's ``WHERE``. The filter decides which rows are left, not what the expression counts:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customers.region
       METRICS avg_orders_per_customer
       WHERE orders.category = 'Clothing'
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬─────────────────────────┐
   │ region │ avg_orders_per_customer │
   ├────────┼─────────────────────────┤
   │ North  │                     3.0 │
   │ South  │                     3.0 │
   └────────┴─────────────────────────┘

Each customer keeps its full order count: Alice still counts 4, not her 2 clothing orders. Dan has no clothing order, so South averages Carol's 3 alone.


.. _howto-lod-facts:

Read an LOD Fact in FACTS
=========================

Alone in ``FACTS``, an LOD fact returns one row per value of its fixed dimensions: here one row per customer, ``4``, ``2``, ``3`` and ``1``. Beside an order fact or dimension, it returns one row per order, each with its customer's count:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       FACTS orders.order_id, orders_per_customer
   ) ORDER BY order_id LIMIT 3;

.. code-block:: text

   ┌──────────┬─────────────────────┐
   │ order_id │ orders_per_customer │
   ├──────────┼─────────────────────┤
   │      101 │                   4 │
   │      102 │                   4 │
   │      103 │                   4 │
   └──────────┴─────────────────────┘


.. _howto-lod-nulls:

NULL Fixed-Grain Values
=======================

An order with no customer, or whose customer has no ``customers`` row, has a ``NULL`` customer. The extension computes an LOD expression with ``GROUP BY``, so every such order shares one ``NULL`` customer: with two such orders, ``orders_per_customer`` is 2 for each of them, and ``avg_orders_per_customer`` counts that ``NULL`` customer once, in the ``NULL`` region.

Snowflake differs here. It counts the same ``NULL`` customer in an average such as ``avg_orders_per_customer``, but gives an LOD value that reads a ``NULL`` fixed dimension as ``NULL`` when the value is read on its own: in ``FACTS``, and in a metric beside a plain aggregate, such as ``pct_of_total``, where Snowflake's ``NULL`` region gets ``NULL`` and the extension its share of the total. To match Snowflake, give every order a customer, or filter the ``NULL`` customers out.


.. _howto-lod-limits:

What Is Not Supported
=====================

- ``INCLUDE DIMENSIONS`` and ``EXCLUDE DIMENSIONS`` are refused at ``CREATE`` for now; only ``FIXED DIMENSIONS`` is supported. They are planned for the next release.
- An LOD expression cannot be written in a query; declare it in the view.
- A query that reads an LOD expression and reaches across a :ref:`direct many-to-many relationship <ref-create-direct-many-to-many>` is refused, as in Snowflake (010959).
- A materialization cannot cover an LOD expression, or a metric that reads one.


.. _howto-lod-troubleshooting:

Troubleshooting
===============

``CREATE`` refuses an LOD expression that breaks one of Snowflake's rules, and the message says which (:ref:`ref-err-lod-rule`):

- a fact, dimension or metric with a table prefix that holds or reads an LOD expression;
- an LOD expression that reads another, or that is not one aggregate over facts and dimensions;
- ``FIXED DIMENSIONS`` listing an expression, an LOD expression, or a name the view doesn't define;
- a metric that reads an LOD expression outside an aggregate call, two in one call, or another member in the same call;
- an LOD expression whose tables are not related along foreign keys, or reached by more than one path.


See also
========

- :ref:`LOD expressions <ref-create-lod>` -- the reference for LOD expressions
- :ref:`howto-fan-traps` -- how a metric is counted once per group
- :ref:`howto-derived-metrics` -- metrics built from other metrics
