.. meta::
   :description: Compute a value at a grain other than the query's, such as orders per customer averaged by region or a share of a region's revenue, with LOD expressions and FIXED, INCLUDE or EXCLUDE DIMENSIONS

.. _howto-lod:

===========================================================
How to Compute Values at Another Grain (LOD Expressions)
===========================================================

.. versionadded:: 0.20.0

This guide shows how to compute a value at a grain other than the query's, such as each customer's order count averaged by region, or each region's revenue as a share of the grand total. You write the value as a **level-of-detail (LOD) expression**: an aggregate followed by the dimensions it is computed at. ``FIXED DIMENSIONS (...)`` names them outright, ``INCLUDE DIMENSIONS (...)`` adds them to the query's dimensions, and ``EXCLUDE DIMENSIONS (...)`` takes them away. The syntax and the answers are Snowflake's.

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
   :emphasize-lines: 11-14,21,25-31

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
       grand_total_revenue AS SUM(orders.order_amount) FIXED DIMENSIONS (),
       orders_incl_customer AS COUNT(orders.order_id) INCLUDE DIMENSIONS (customers.customer_id),
       region_revenue      AS SUM(orders.order_amount) EXCLUDE DIMENSIONS (customers.customer_id)
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
       avg_orders_inline       AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id)),
       avg_orders_incl_customer AS AVG(orders_incl_customer),
       avg_customer_revenue    AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id)),
       pct_of_region_revenue   AS SUM(orders.order_amount) / AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (orders.category)),
       avg_region_revenue      AS AVG(region_revenue)
   );

Each expression is one aggregate over the view's facts and dimensions, followed by its dimensions, each named as ``<table>.<name>``. ``FIXED DIMENSIONS ()`` is the grand total. The table it aggregates (``orders`` here) must reference every table its dimensions read, directly or through other tables. ``orders`` needs no ``PRIMARY KEY``.


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


.. _howto-lod-include:

Add a Dimension to the Query's Grain (INCLUDE)
==============================================

An ``INCLUDE DIMENSIONS`` expression is computed at the query's dimensions plus its own. ``orders_incl_customer`` counts each customer's orders within each group of the query, and ``avg_orders_incl_customer`` averages those counts. Grouped by region alone, it gives the same answer as the ``FIXED`` metric. Grouped by region and category, the two differ:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customers.region, orders.category
       METRICS avg_orders_per_customer, avg_orders_incl_customer
   ) ORDER BY region, category;

.. code-block:: text

   ┌────────┬─────────────┬─────────────────────────┬──────────────────────────┐
   │ region │  category   │ avg_orders_per_customer │ avg_orders_incl_customer │
   ├────────┼─────────────┼─────────────────────────┼──────────────────────────┤
   │ North  │ Clothing    │                     3.0 │                      1.5 │
   │ North  │ Electronics │                     3.0 │                      1.5 │
   │ South  │ Clothing    │                     3.0 │                      1.0 │
   │ South  │ Electronics │                     2.0 │                      1.5 │
   └────────┴─────────────┴─────────────────────────┴──────────────────────────┘

The ``FIXED`` metric averages each customer's count of all orders. The ``INCLUDE`` metric counts only the orders in the row's category: North's clothing orders are Alice's 2 and Bob's 1, which average 1.5.

``avg_customer_revenue`` writes an ``INCLUDE`` expression inline. By region it is the average customer's revenue: 650 for North (Alice's 1000 and Bob's 300) and 400 for South.


.. _howto-lod-exclude:

Take a Dimension Out of the Query's Grain (EXCLUDE)
===================================================

An ``EXCLUDE DIMENSIONS`` expression is computed at the query's dimensions minus its own. ``pct_of_region_revenue`` divides each group's revenue by its revenue without the category, which is its region's revenue when the query groups by region and category:

.. code-block:: sql

   SELECT region, category, round(pct_of_region_revenue, 3) AS pct FROM SEMANTIC_VIEW(sales
       DIMENSIONS customers.region, orders.category
       METRICS pct_of_region_revenue
   ) ORDER BY region, category;

.. code-block:: text

   ┌────────┬─────────────┬───────┐
   │ region │  category   │  pct  │
   ├────────┼─────────────┼───────┤
   │ North  │ Clothing    │ 0.385 │
   │ North  │ Electronics │ 0.615 │
   │ South  │ Clothing    │ 0.125 │
   │ South  │ Electronics │ 0.875 │
   └────────┴─────────────┴───────┘

North's clothing revenue is 500.00 of its 1300.00. A query that doesn't group by the category has nothing to take out, so the same metric by region alone is 1.0 for each region.

An ``EXCLUDE`` value is listed for every group the query lists, also one with no rows of the aggregated table. Grouped by region and customer, ``avg_region_revenue`` gives each customer their region's revenue, and a customer with no orders gets it too.


.. _howto-lod-filters:

Filters and LOD Expressions
===========================

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

An ``INCLUDE`` or ``EXCLUDE`` expression is computed at the query's grain, so it is computed over the rows the ``WHERE`` keeps:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customers.region
       METRICS avg_orders_per_customer, avg_orders_incl_customer
       WHERE orders.category = 'Electronics'
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬─────────────────────────┬──────────────────────────┐
   │ region │ avg_orders_per_customer │ avg_orders_incl_customer │
   ├────────┼─────────────────────────┼──────────────────────────┤
   │ North  │                     3.0 │                      1.5 │
   │ South  │                     2.0 │                      1.5 │
   └────────┴─────────────────────────┴──────────────────────────┘

The ``FIXED`` metric still counts every order of each customer with an electronics order. The ``INCLUDE`` metric counts only their electronics orders: Alice's 2 and Bob's 1 in North, Carol's 2 and Dan's 1 in South.


.. _howto-lod-child:

Group by a Table Below the Aggregated One
=========================================

A query can group or filter an ``INCLUDE`` or ``EXCLUDE`` expression by a table below the one it aggregates, such as order returns below orders. Add a returns table, where order 102 is returned twice for the same reason:

.. code-block:: sql

   CREATE TABLE order_returns (return_id INTEGER, order_id INTEGER, reason VARCHAR);
   INSERT INTO order_returns VALUES
       (1, 101, 'damaged'), (2, 101, 'late'), (3, 102, 'late'),
       (4, 102, 'late'),    (5, 103, 'late'), (6, 107, 'damaged');

   CREATE SEMANTIC VIEW sales_returns
   TABLES (
       customers     AS customers PRIMARY KEY (customer_id),
       orders        AS orders PRIMARY KEY (order_id),
       order_returns AS order_returns PRIMARY KEY (return_id)
   )
   RELATIONSHIPS (
       orders(customer_id) REFERENCES customers,
       order_returns(order_id) REFERENCES orders
   )
   FACTS (orders.order_amount AS amount)
   DIMENSIONS (
       customers.customer_id AS customer_id,
       customers.region      AS region,
       order_returns.reason  AS reason
   )
   METRICS (
       orders.total_revenue AS SUM(amount),
       avg_customer_revenue AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id))
   );

As in Snowflake, the expression is then computed over the returns, each joined to its order. An order counts once for each of its returns in a group, and the groups are the returns' reasons:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales_returns
       DIMENSIONS order_returns.reason
       METRICS avg_customer_revenue
   ) ORDER BY reason;

.. code-block:: text

   ┌─────────┬──────────────────────┐
   │ reason  │ avg_customer_revenue │
   ├─────────┼──────────────────────┤
   │ damaged │                350.0 │
   │ late    │               1100.0 │
   └─────────┴──────────────────────┘

Alice's late returns are 101 (400), 103 (300) and 102 twice (200 each), so her late revenue is 1100. A return with a ``NULL`` reason would make a ``NULL`` group of its own. Filtering by the returns works the same way: ``WHERE order_returns.reason = 'late'`` by region gives North 1100.

Beside a plain metric on the aggregated table, such as ``total_revenue`` on orders, the expression is computed over that metric's rows instead, as Snowflake computes it: each order once per group of its returns, and the orders with no return in the ``NULL`` group:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales_returns
       DIMENSIONS order_returns.reason
       METRICS orders.total_revenue, avg_customer_revenue
   ) ORDER BY reason;

.. code-block:: text

   ┌─────────┬───────────────┬──────────────────────┐
   │ reason  │ total_revenue │ avg_customer_revenue │
   ├─────────┼───────────────┼──────────────────────┤
   │ damaged │        700.00 │                350.0 │
   │ late    │        900.00 │                900.0 │
   │ NULL    │        900.00 │                225.0 │
   └─────────┴───────────────┴──────────────────────┘

Alice's late revenue is now 900, order 102 counted once. A plain metric on another table, such as one on the returns or on the customers, leaves the first answer; so does an LOD metric. When a value has no return to list its group, for example a customer whose only orders have no return in a query grouped by region and reason, it is read in a row whose dimensions are all ``NULL``. A ``WHERE`` applies to that row too, so ``WHERE customers.region = 'North'`` drops it, and a value whose returns the ``WHERE`` all drops is left out. To compute the expression over the returns, query it without the plain metric.

A ``FIXED`` expression is computed over every row of the aggregated table whatever the query groups by, but when the query groups or filters by a table below it, the groups still come from that table, as Snowflake reads them: each customer's value goes to the groups of its returns, and a customer with no return to a row whose dimensions are all ``NULL``, even when the query also groups by region.


.. _howto-lod-diamond:

Across Two Paths
================

The aggregated table can reach a grouped table along two paths, as orders reach regions through customers and through stores. A plain metric is refused there. An ``INCLUDE`` or ``EXCLUDE`` expression is computed, as in Snowflake, along one path, which the relationship names choose. Read from the aggregated table, the paths are compared by the names of their relationships, upper-cased, hop by hop, and the first name that differs decides: the smaller name wins, whatever the paths' lengths. Any other table the expression needs, such as an ``INCLUDE`` dimension on the other path, is joined too, and a table that two relationships reach from the joined tables is joined on both.

So name the relationships to choose the path. Snowflake gives an unnamed relationship a random name each time the view is created, so there the path can change with every ``CREATE``; the extension gives it a name made from its tables and columns (``SYS_RELATIONSHIP_...``, shown by ``DESCRIBE SEMANTIC VIEW``), so the path stays the same, but it is not one you chose. A query that filters by a table on the path not taken is refused, as in Snowflake.


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

- An ``INCLUDE`` or ``EXCLUDE`` expression is computed at the query's grain, so a query can read it only in ``METRICS``, inside an aggregate: not in ``DIMENSIONS``, ``FACTS`` or ``WHERE``. A query that reads one can't group by an expression item, or group or filter by an LOD dimension, which has a grain of its own. As in Snowflake, these are refused.
- A query that reads an ``INCLUDE`` or ``EXCLUDE`` expression can't filter by a table on the path not taken across two paths (see :ref:`howto-lod-diamond`), or group or filter by a table neither the aggregated table nor the grouped tables reach along foreign keys (see :ref:`howto-lod-child`).
- An LOD expression cannot be written in a query; declare it in the view.
- A query that reads an LOD expression and reaches across a :ref:`direct many-to-many relationship <ref-create-direct-many-to-many>` is refused, as in Snowflake (010959).
- A materialization cannot cover an LOD expression, or a metric that reads one.


.. _howto-lod-troubleshooting:

Troubleshooting
===============

``CREATE`` refuses an LOD expression that breaks one of Snowflake's rules, and the message says which (:ref:`ref-err-lod-rule`):

- a fact, dimension or metric with a table prefix that holds or reads an LOD expression;
- an LOD expression that reads another, or that is not one aggregate over facts and dimensions;
- ``FIXED``, ``INCLUDE`` or ``EXCLUDE DIMENSIONS`` listing an expression, an LOD expression, or a name the view doesn't define;
- a metric that reads an LOD expression outside an aggregate call, two in one call, or another member in the same call;
- an LOD expression whose tables are not related along foreign keys, or reached by more than one path.


See also
========

- :ref:`LOD expressions <ref-create-lod>` -- the reference for LOD expressions
- :ref:`howto-fan-traps` -- how a metric is counted once per group
- :ref:`howto-derived-metrics` -- metrics built from other metrics
