.. meta::
   :description: Compose metrics from other metrics using arithmetic expressions so you can compute profit, margin, and similar calculations without repeating aggregate logic

.. _howto-derived-metrics:

=============================================
How to Compose Metrics with Derived Metrics
=============================================

This guide shows how to define derived metrics, which combine other metrics by name rather than writing full aggregate expressions. Derived metrics enable calculations like profit (revenue minus cost) and margin (profit divided by revenue) without repeating the underlying aggregation logic.

**Prerequisites:**

- A working multi-table semantic view (see :ref:`tutorial-multi-table`, or :ref:`tutorial-building-model` for a guided introduction to derived metrics)
- Understanding of the ``FACTS`` clause (see :ref:`howto-facts`)


.. _howto-derived-basic:

Define a Derived Metric
=======================

A derived metric references other metric names instead of table columns. It has no table alias prefix and no aggregate function.

.. code-block:: sql
   :emphasize-lines: 13,14

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       li AS line_items PRIMARY KEY (id),
       o  AS orders      PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       li_to_order AS li(order_id) REFERENCES o
   )
   DIMENSIONS (
       o.region AS o.region
   )
   METRICS (
       li.revenue AS SUM(li.extended_price),
       li.cost    AS SUM(li.unit_cost),
       profit     AS revenue - cost,
       margin     AS profit / revenue * 100
   );

The key distinction:

- **Base metrics** (``li.revenue``, ``li.cost``) have a table alias prefix and contain aggregate functions.
- **Derived metrics** (``profit``, ``margin``) have no table alias prefix and reference other metric names.


.. _howto-derived-query:

Query Derived Metrics
=====================

Derived metrics work like any other metric in a :ref:`query <ref-semantic-view-function>`:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS region
       METRICS revenue, cost, profit
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬─────────┬────────┬────────┐
   │ region │ revenue │  cost  │ profit │
   ├────────┼─────────┼────────┼────────┤
   │ East   │  300.00 │ 130.00 │ 170.00 │
   │ West   │  150.00 │  60.00 │  90.00 │
   └────────┴─────────┴────────┴────────┘

You can also query a derived metric on its own. The extension resolves all underlying base metrics it depends on:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS region
       METRICS margin
   ) ORDER BY region;

.. tip::

   To try a calculation before you declare it, write it as an expression in
   the query: ``METRICS revenue, cost, revenue - cost AS profit`` gives the same
   column as the declared ``profit``, for that query only. See
   :ref:`ref-sv-expression-items`.


.. _howto-derived-stacking:

Stack Derived Metrics
=====================

Derived metrics can reference other derived metrics. The extension resolves the full dependency chain:

.. code-block:: sql

   METRICS (
       li.revenue AS SUM(li.extended_price),
       li.cost    AS SUM(li.unit_cost),
       profit     AS revenue - cost,
       margin     AS profit / revenue * 100
   );

Here ``margin`` references ``profit``, which in turn references ``revenue`` and ``cost``. The extension expands the chain:

- ``profit`` becomes ``SUM(li.extended_price) - SUM(li.unit_cost)``
- ``margin`` becomes ``(SUM(li.extended_price) - SUM(li.unit_cost)) / SUM(li.extended_price) * 100``


.. _howto-derived-with-facts:

Combine Facts and Derived Metrics
=================================

Facts and derived metrics work together. Facts provide reusable row-level expressions, base metrics aggregate them, and derived metrics compose the aggregates.

.. code-block:: sql

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       li AS line_items PRIMARY KEY (id),
       o  AS orders      PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       li_to_order AS li(order_id) REFERENCES o
   )
   FACTS (
       li.net_price AS li.extended_price * (1 - li.discount)
   )
   DIMENSIONS (
       o.region AS o.region
   )
   METRICS (
       li.revenue AS SUM(li.net_price),
       li.cost    AS SUM(li.unit_cost),
       profit     AS revenue - cost,
       margin     AS profit / revenue * 100
   );

The full resolution chain:

1. ``net_price`` (fact) = ``li.extended_price * (1 - li.discount)``
2. ``revenue`` (base metric) = ``SUM(li.extended_price * (1 - li.discount))``
3. ``profit`` (derived) = ``SUM(...) - SUM(li.unit_cost)``
4. ``margin`` (derived) = ``(SUM(...) - SUM(...)) / SUM(...) * 100``


.. _howto-derived-errors:

Troubleshooting
===============

**Aggregate function in a derived metric**
   Derived metrics must not contain aggregate functions. The expression ``profit AS SUM(revenue)``
   is rejected at define time because ``SUM()`` is an aggregate. The correct form is
   ``profit AS revenue - cost`` (referencing metric names, not aggregating them).

**Circular derived metric references**
   Derived metrics that reference each other in a cycle cause a define-time error. For
   example, ``a AS b + 1`` and ``b AS a + 1`` is rejected.

**Derived metric references unknown name**
   Every bare name in a derived metric must be a metric declared in the same view. Any
   other name, such as a misspelling or an unqualified column, is rejected at define
   time. The error suggests the closest match and lists the metrics you can use:

   .. code-block:: text

      unknown metric 'costt' referenced in derived metric 'profit'; did you mean 'cost'?.
      Available metrics: [revenue, cost, profit]

   To use a column, aggregate it in a base metric first (``li.cost AS SUM(li.unit_cost)``)
   and reference that metric. A table-qualified column such as ``li.unit_cost`` passes
   the name check, and ``CREATE`` then refuses it when it binds the metric:
   ``metric 'profit' does not bind: Binder Error: column "unit_cost" must appear in the
   GROUP BY clause or must be part of an aggregate function``. The fix is the same.


.. _howto-derived-related:

Related
=======

- :ref:`ref-create-metrics` -- Full ``METRICS`` clause syntax, including derived metrics
- :ref:`howto-facts` -- Reusable row-level expressions for base metrics to aggregate
- :ref:`howto-role-playing` -- How derived metrics inherit ``USING`` context
- :ref:`tutorial-building-model` -- A guided walk through facts and derived metrics
