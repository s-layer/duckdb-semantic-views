.. meta::
   :description: Build a realistic analytics model with facts and derived metrics, learning how to refactor duplicated logic and compose calculations

.. _tutorial-building-model:

=========================
Building a Complete Model
=========================

In this tutorial, you will extend a multi-table semantic view with facts and derived metrics. Starting from a basic view with simple metrics, you will identify duplicated calculations, extract them into reusable facts, and compose higher-level metrics from lower-level ones. By the end, you will understand the full modeling workflow: tables, relationships, facts, base metrics, and derived metrics working together.

**Time:** 15 minutes

**Prerequisites:**

- Completed the :ref:`tutorial-multi-table` tutorial
- Familiarity with row-level vs. aggregate expressions in SQL


.. _tutorial-bm-create-tables:

Create the Schema
=================

Create an e-commerce schema with three tables: ``orders`` (header table), ``customers`` (dimension table), and ``line_items`` (fact table with pricing detail).

.. code-block:: sql

   CREATE TABLE customers (id INTEGER, name VARCHAR, city VARCHAR);
   INSERT INTO customers VALUES
       (1, 'Alice', 'Portland'),
       (2, 'Bob',   'Seattle'),
       (3, 'Carol', 'Portland');

   CREATE TABLE orders (
       id INTEGER, customer_id INTEGER, ordered_at DATE
   );
   INSERT INTO orders VALUES
       (1, 1, '2024-01-15'),
       (2, 1, '2024-01-20'),
       (3, 2, '2024-02-10'),
       (4, 3, '2024-03-01');

   CREATE TABLE line_items (
       id INTEGER, order_id INTEGER,
       price DECIMAL(10,2), quantity INTEGER,
       discount DECIMAL(4,2), unit_cost DECIMAL(10,2)
   );
   INSERT INTO line_items VALUES
       (1, 1, 25.00, 2, 0.00,  10.00),
       (2, 1, 40.00, 1, 0.10,  15.00),
       (3, 2, 50.00, 3, 0.00,  20.00),
       (4, 3, 30.00, 1, 0.20,  12.00),
       (5, 3, 80.00, 2, 0.05,  35.00),
       (6, 4, 60.00, 1, 0.00,  25.00);

The ``line_items`` table has the pricing detail: ``price`` is the unit price, ``quantity`` is the number of units, ``discount`` is a fractional discount (0.10 = 10%), and ``unit_cost`` is the per-unit cost to the business.


.. _tutorial-bm-basic-view:

Start with a Basic View
=======================

Define a semantic view with simple metrics. This is a starting point that you will refactor as patterns emerge.

.. code-block:: sql

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       o  AS orders     PRIMARY KEY (id),
       c  AS customers  PRIMARY KEY (id),
       li AS line_items PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       order_items    AS li(order_id)   REFERENCES o
   )
   DIMENSIONS (
       c.customer AS c.name,
       c.city     AS c.city,
       o.month    AS date_trunc('month', o.ordered_at)
   )
   METRICS (
       li.revenue AS SUM(li.price * li.quantity * (1 - li.discount)),
       li.cost    AS SUM(li.unit_cost * li.quantity)
   );

Query the view to verify it works:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customer
       METRICS revenue, cost
   ) ORDER BY revenue DESC;

.. code-block:: text

   ┌──────────┬──────────┬───────┐
   │ customer │ revenue  │ cost  │
   ├──────────┼──────────┼───────┤
   │ Alice    │ 236.0000 │ 95.00 │
   │ Bob      │ 176.0000 │ 82.00 │
   │ Carol    │  60.0000 │ 25.00 │
   └──────────┴──────────┴───────┘

Alice's 236 is the sum of her three line items -- 50.00, 36.00 and 150.00 -- where each is ``price * quantity * (1 - discount)``. Bob's two line items give 24.00 + 152.00 = 176.00, and Carol's single item is 60.00.

``revenue`` comes back with four decimal places while ``cost`` has two. Neither is rounded: DuckDB widens the scale of a ``DECIMAL`` product, and ``revenue`` multiplies by ``discount``, itself a ``DECIMAL(4,2)``, so the result carries the combined scale. ``cost`` multiplies only by an integer quantity and keeps its original two.

The metrics work, but look at the expressions. ``li.price * li.quantity * (1 - li.discount)`` computes the net line total, and ``li.unit_cost * li.quantity`` computes the total cost. If you added more metrics (average net price, gross total before discount), you would repeat these row-level calculations in each metric expression.


.. _tutorial-bm-add-facts:

Extract Repeated Logic into Facts
==================================

The ``FACTS`` clause lets you name row-level expressions that metrics can reference. A fact computes a value per row, without aggregation. Metrics then aggregate facts instead of repeating the calculation.

Refactor the view to extract two facts:

.. code-block:: sql
   :emphasize-lines: 11-14

   CREATE OR REPLACE SEMANTIC VIEW sales AS
   TABLES (
       o  AS orders     PRIMARY KEY (id),
       c  AS customers  PRIMARY KEY (id),
       li AS line_items PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       order_items    AS li(order_id)   REFERENCES o
   )
   FACTS (
       li.net_total AS li.price * li.quantity * (1 - li.discount),
       li.line_cost AS li.unit_cost * li.quantity
   )
   DIMENSIONS (
       c.customer AS c.name,
       c.city     AS c.city,
       o.month    AS date_trunc('month', o.ordered_at)
   )
   METRICS (
       li.revenue AS SUM(li.net_total),
       li.cost    AS SUM(li.line_cost)
   );

The ``FACTS`` clause appears between ``RELATIONSHIPS`` and ``DIMENSIONS``. Each fact follows the same ``alias.name AS expression`` pattern as dimensions and metrics.

Now the metrics read as ``SUM(li.net_total)`` and ``SUM(li.line_cost)`` instead of embedding the full row-level calculation. When the extension expands a query, it inlines the fact expressions automatically. The query results are identical:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customer
       METRICS revenue, cost
   ) ORDER BY revenue DESC;

.. code-block:: text

   ┌──────────┬──────────┬───────┐
   │ customer │ revenue  │ cost  │
   ├──────────┼──────────┼───────┤
   │ Alice    │ 236.0000 │ 95.00 │
   │ Bob      │ 176.0000 │ 82.00 │
   │ Carol    │  60.0000 │ 25.00 │
   └──────────┴──────────┴───────┘


.. _tutorial-bm-derived-metrics:

Add Derived Metrics
===================

A derived metric references other metrics by name instead of writing an aggregate expression. Derived metrics have no table alias prefix and no aggregate function. They let you compose calculations like profit and margin without repeating the aggregation logic.

Before you change the view, you can try a calculation in a query. An item in ``METRICS`` can be an expression over the view's metrics, named with ``AS``:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customer
       METRICS revenue, cost, revenue - cost AS profit
   ) ORDER BY revenue DESC;

.. code-block:: text

   ┌──────────┬──────────┬───────┬──────────┐
   │ customer │ revenue  │ cost  │  profit  │
   ├──────────┼──────────┼───────┼──────────┤
   │ Alice    │ 236.0000 │ 95.00 │ 141.0000 │
   │ Bob      │ 176.0000 │ 82.00 │  94.0000 │
   │ Carol    │  60.0000 │ 25.00 │  35.0000 │
   └──────────┴──────────┴───────┴──────────┘

The expression lasts for this one query (see :ref:`ref-sv-expression-items`). Once the calculation is worth keeping, declare it in the view, so that every query can name it and it means the same thing everywhere.

Add ``profit`` and ``margin`` to the view:

.. code-block:: sql
   :emphasize-lines: 24,25

   CREATE OR REPLACE SEMANTIC VIEW sales AS
   TABLES (
       o  AS orders     PRIMARY KEY (id),
       c  AS customers  PRIMARY KEY (id),
       li AS line_items PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       order_items    AS li(order_id)   REFERENCES o
   )
   FACTS (
       li.net_total AS li.price * li.quantity * (1 - li.discount),
       li.line_cost AS li.unit_cost * li.quantity
   )
   DIMENSIONS (
       c.customer AS c.name,
       c.city     AS c.city,
       o.month    AS date_trunc('month', o.ordered_at)
   )
   METRICS (
       li.revenue AS SUM(li.net_total),
       li.cost    AS SUM(li.line_cost),
       profit     AS revenue - cost,
       margin     AS profit / revenue * 100
   );

Notice the pattern:

- ``profit`` references ``revenue`` and ``cost`` by name. It has no alias prefix and no aggregate function.
- ``margin`` references ``profit``, which itself references ``revenue`` and ``cost``. The extension resolves the full chain.

Query the complete model:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS customer
       METRICS revenue, cost, profit, margin
   ) ORDER BY margin DESC;

.. code-block:: text

   ┌──────────┬──────────┬───────┬──────────┬────────────────────┐
   │ customer │ revenue  │ cost  │  profit  │       margin       │
   ├──────────┼──────────┼───────┼──────────┼────────────────────┤
   │ Alice    │ 236.0000 │ 95.00 │ 141.0000 │   59.7457627118644 │
   │ Carol    │  60.0000 │ 25.00 │  35.0000 │ 58.333333333333336 │
   │ Bob      │ 176.0000 │ 82.00 │  94.0000 │  53.40909090909091 │
   └──────────┴──────────┴───────┴──────────┴────────────────────┘

Carol now sorts above Bob: she has the smallest revenue but the second-best ratio of profit to revenue. ``margin`` is a floating-point division, so the trailing digits are the usual DOUBLE representation rather than a rounded percentage.

You can query derived metrics with any dimension, just like base metrics. The extension includes all the tables needed to compute the underlying aggregations:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS month
       METRICS revenue, profit, margin
   ) ORDER BY month;

.. code-block:: text

   ┌─────────────────────┬──────────┬──────────┬────────────────────┐
   │        month        │ revenue  │  profit  │       margin       │
   ├─────────────────────┼──────────┼──────────┼────────────────────┤
   │ 2024-01-01 00:00:00 │ 236.0000 │ 141.0000 │   59.7457627118644 │
   │ 2024-02-01 00:00:00 │ 176.0000 │  94.0000 │  53.40909090909091 │
   │ 2024-03-01 00:00:00 │  60.0000 │  35.0000 │ 58.333333333333336 │
   └─────────────────────┴──────────┴──────────┴────────────────────┘

The monthly numbers match the per-customer ones here because each customer happens to order in a single month.


.. _tutorial-bm-inspect:

Inspect the Generated SQL
=========================

Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to see how facts and derived metrics are expanded:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('sales',
       dimensions := ['customer'],
       metrics := ['revenue', 'profit']
   );

The expanded SQL shows the full chain:

1. **Fact inlining** -- ``li.net_total`` is replaced with ``li.price * li.quantity * (1 - li.discount)`` inside the ``SUM()``.
2. **Derived metric expansion** -- ``profit`` is replaced with ``revenue - cost``, where ``revenue`` and ``cost`` are the aggregate expressions from step 1.
3. **Selective joining** -- only the tables needed for the requested dimensions and metrics are joined. If you remove ``customer`` from the query, the ``customers`` table is dropped from the generated SQL.

The expanded SQL also starts ``FROM line_items``, not ``FROM orders``. ``revenue`` and ``cost`` are declared on ``li``, so they are aggregated over the line items themselves, and the query joins outward to ``orders`` and ``customers`` only to look up the customer name. :ref:`explanation-metric-grain` explains why each metric is computed from its own table.

The workflow is: define facts once, compose metrics from them, and let the extension generate the SQL.


.. _tutorial-bm-cleanup:

Clean Up
========

.. code-block:: sql

   DROP SEMANTIC VIEW sales;
   DROP TABLE line_items;
   DROP TABLE orders;
   DROP TABLE customers;


.. _tutorial-bm-summary:

What You Learned
================

You now know how to:

- Define reusable row-level logic in the :ref:`FACTS <ref-create-facts>` clause
- Refactor duplicated calculations from metrics into facts
- Compose metrics from other metrics using :ref:`derived metrics <ref-create-metrics>`
- Try a calculation as an expression item in the query before declaring it
- Stack derived metrics (``margin`` references ``profit`` which references ``revenue`` and ``cost``)
- Inspect fact inlining and metric expansion with :ref:`explain_semantic_view() <ref-explain-semantic-view>`

Next, explore the how-to guides for deeper coverage of these topics and more:

- :ref:`howto-facts` -- fact chaining, querying facts directly, fact metadata
- :ref:`howto-derived-metrics` -- stacking patterns, combining facts and derived metrics
- :ref:`howto-semi-additive` -- metrics for snapshot data like account balances
- :ref:`howto-window-metrics` -- rolling averages, rankings, and lag comparisons
- :ref:`howto-materializations` -- route queries to pre-aggregated tables
