.. meta::
   :description: Define window function metrics with OVER clause, PARTITION BY, PARTITION BY EXCLUDING, ORDER BY, and frame clauses for rolling averages, lag comparisons, and rankings

.. _howto-window-metrics:

=======================================
How to Use Window Function Metrics
=======================================

This guide shows how to define metrics that use SQL window functions -- rolling averages, lag comparisons, running totals, and rankings -- using the ``OVER`` clause in the ``METRICS`` section.

**Prerequisites:**

- A working semantic view with ``TABLES``, ``DIMENSIONS``, and ``METRICS`` (see :ref:`tutorial-multi-table`)
- Familiarity with SQL window functions (``AVG() OVER``, ``LAG()``, ``ROW_NUMBER()``, etc.)


.. _howto-window-define:

Define a Window Metric
======================

A window metric wraps another metric in a window function. The ``OVER`` clause specifies partitioning, ordering, and an optional frame:

.. code-block:: sql
   :emphasize-lines: 12,13

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       s AS sales PRIMARY KEY (id)
   )
   DIMENSIONS (
       s.store AS s.store,
       s.date  AS s.sale_date
   )
   METRICS (
       s.total_qty AS SUM(s.quantity),
       s.rolling_avg AS
           AVG(total_qty) OVER (PARTITION BY EXCLUDING date ORDER BY date NULLS LAST)
   );

This defines ``rolling_avg`` as the rolling average of ``total_qty``, partitioned by all queried dimensions except ``date``, ordered by ``date``.


.. _howto-window-partition-excluding:

PARTITION BY
============

Plain ``PARTITION BY`` specifies an explicit, fixed set of partition dimensions. Unlike ``EXCLUDING``, the partition set does not change based on which dimensions are queried -- it is always exactly the listed dimensions:

.. code-block:: sql
   :emphasize-lines: 12,13

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       s AS sales PRIMARY KEY (id)
   )
   DIMENSIONS (
       s.store   AS s.store,
       s.date    AS s.sale_date,
       s.region  AS s.region
   )
   METRICS (
       s.total_qty AS SUM(s.quantity),
       s.store_avg AS
           AVG(total_qty) OVER (PARTITION BY store ORDER BY date NULLS LAST)
   );

Here ``store_avg`` always partitions by ``store``, regardless of whether the query also includes ``region`` or other dimensions. This is useful when the business logic requires a fixed partition boundary -- for example, always computing a per-store average even when additional dimensions appear in the query.

.. code-block:: text

   Query requests dimensions [store, date, region]:
   PARTITION BY store  ->  PARTITION BY store   (unchanged; region does not enter the partition set)

``PARTITION BY EXCLUDING`` computes the partition set dynamically at query time. The extension takes all queried dimensions and removes the excluded ones to form the partition:

.. code-block:: text

   Query requests dimensions [store, date, year]:
   PARTITION BY EXCLUDING date        ->  PARTITION BY store, year
   PARTITION BY EXCLUDING date, year  ->  PARTITION BY store

This approach avoids hard-coding partition columns. The same metric adapts to different dimension combinations -- add more dimensions to a query, and they automatically become part of the partition set.

.. tip::

   Use ``PARTITION BY EXCLUDING`` when you want the partition to adapt to whatever dimensions the query requests. Use plain ``PARTITION BY`` when you need a fixed, predictable partition that stays the same regardless of the query context.

``PARTITION BY`` and ``PARTITION BY EXCLUDING`` are mutually exclusive -- a single window metric uses one or the other, never both.


.. _howto-window-order:

ORDER BY with Sort and NULLS
=============================

The ``ORDER BY`` clause inside ``OVER`` specifies which dimension(s) control the window ordering:

.. code-block:: sql

   -- Ascending by date (default), NULLs last (default)
   AVG(total_qty) OVER (PARTITION BY EXCLUDING date ORDER BY date)

   -- Descending by date, NULLs first
   AVG(total_qty) OVER (PARTITION BY EXCLUDING date ORDER BY date DESC NULLS FIRST)

Each ``ORDER BY`` entry accepts ``ASC`` or ``DESC`` (default: ``ASC``) and ``NULLS FIRST`` or ``NULLS LAST`` (default: ``NULLS LAST``).


.. _howto-window-frame:

Frame Clauses
=============

Add a frame clause after ``ORDER BY`` for sliding window calculations:

.. code-block:: sql
   :emphasize-lines: 3

   s.rolling_avg_7d AS
       AVG(total_qty) OVER (PARTITION BY EXCLUDING date
           ORDER BY date NULLS LAST
           RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW)

The frame clause is passed through to the generated SQL unchanged. Both ``RANGE`` and ``ROWS`` frame types are supported.


.. _howto-window-extra-args:

Extra Function Arguments
=========================

Window functions that take additional arguments beyond the metric (like ``LAG`` and ``LEAD``) specify them after the inner metric name:

.. code-block:: sql

   -- LAG with an offset of 30 rows
   s.qty_30_rows_ago AS
       LAG(total_qty, 30) OVER (PARTITION BY EXCLUDING date ORDER BY date NULLS LAST)

The extra arguments (``30``) appear after the inner metric name inside the function call. The offset counts rows of the query result, not calendar days: with one row per store per sale date, ``qty_30_rows_ago`` is the quantity 30 sale dates earlier for the same store, and any date with no sales is skipped rather than counted.


.. _howto-window-required:

Required Dimensions
===================

Dimensions referenced in ``PARTITION BY EXCLUDING``, ``PARTITION BY``, and ``ORDER BY`` must be included in the query. If a required dimension is missing, the extension returns an error:

.. code-block:: text

   semantic view 'sales': window function metric 'rolling_avg' requires dimension 'date'
   to be included in the query (used in PARTITION BY EXCLUDING)

.. code-block:: text

   semantic view 'sales': window function metric 'store_avg' requires dimension 'store'
   to be included in the query (used in PARTITION BY)

.. tip::

   Use :ref:`SHOW SEMANTIC DIMENSIONS FOR METRIC <ref-show-dims-for-metric>` to see which dimensions are required for a window metric. Required dimensions have ``required = true`` in the output.


.. _howto-window-mixing:

Mixing Restriction
==================

.. warning::

   Window function metrics and regular aggregate metrics cannot be mixed in the same query. A query must contain either all window metrics or all aggregate metrics.

.. code-block:: sql

   -- This fails: mixing window metric 'rolling_avg' with aggregate metric 'total_qty'
   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS store, date
       METRICS total_qty, rolling_avg
   );

.. code-block:: text

   semantic view 'sales': cannot mix window function metrics [rolling_avg]
   with aggregate metrics [total_qty] in the same query

To get both, run two separate queries and join the results.


.. _howto-window-verify:

Verify the Generated SQL
=========================

Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to inspect the CTE expansion:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('sales',
       dimensions := ['store', 'date'],
       metrics := ['rolling_avg']
   );

The ``-- Expanded SQL:`` section of the ``explain_output`` column shows two parts:

.. code-block:: sql

   WITH __sv_agg AS (
       SELECT
           s.store AS "store",
           s.sale_date AS "date",
           SUM(s.quantity) AS "total_qty"
       FROM "memory"."main"."sales" AS "s"
       GROUP BY
           1,
           2
   )
   SELECT
       "store" AS "store",
       "date" AS "date",
       AVG("total_qty") OVER (PARTITION BY "store" ORDER BY "date" ASC NULLS LAST) AS "rolling_avg"
   FROM __sv_agg

1. The ``WITH __sv_agg`` step aggregates the inner metric (``total_qty``) by all queried dimensions.
2. The outer ``SELECT`` applies the window function to those aggregated rows. ``PARTITION BY EXCLUDING date`` has become ``PARTITION BY "store"``, because ``store`` is the only other queried dimension.


.. _howto-window-troubleshoot:

Troubleshooting
===============

**Inner metric not found**
   The metric name inside the window function (e.g., ``total_qty`` in ``AVG(total_qty) OVER (...)``) must match a base or derived metric in the same view. The error identifies the missing metric and suggests a close match: ``Window metric 'X': inner metric 'Y' not found in semantic view metrics. Did you mean '...'?``

**EXCLUDING dimension not found**
   Dimensions listed in ``PARTITION BY EXCLUDING`` must match declared dimensions. The error identifies the unrecognized dimension: ``Window metric 'X': EXCLUDING dimension 'Y' not found in semantic view dimensions. Did you mean '...'?``

**PARTITION BY dimension not found**
   Dimensions listed in ``PARTITION BY`` must match declared dimensions. The error identifies the unrecognized dimension: ``Window metric 'X': PARTITION BY dimension 'Y' not found in semantic view dimensions. Did you mean '...'?``

**ORDER BY dimension not found**
   Dimensions referenced in ``ORDER BY`` must match declared dimensions: ``Window metric 'X': ORDER BY dimension 'Y' not found in semantic view dimensions. Did you mean '...'?``

**Cannot combine OVER with NON ADDITIVE BY**
   A metric cannot be both a window metric and a semi-additive metric. The error message is: ``Cannot combine OVER clause with NON ADDITIVE BY on metric 'alias.X'. Use one or the other.``

**OVER clause not allowed on derived metric**
   Window metrics must use a qualified name (``alias.metric_name``). Derived metrics (those without a table alias) cannot have an ``OVER`` clause. The error message is: ``OVER clause not allowed on derived metric 'X'. Only qualified metrics (alias.name) can use OVER.``

**Window metrics cannot be used as inner metrics**
   A window metric cannot be the inner metric of another window metric; reference a base or derived metric instead. ``CREATE`` refuses the outer metric when it binds it, with ``metric '<name>' does not bind:`` followed by DuckDB's error, which may be a syntax error rather than a message about windows.


.. _howto-window-related:

Related
=======

- :ref:`ref-create-metrics` -- Full metric grammar, including the ``OVER`` clause
- :ref:`ref-show-dims-for-metric` -- See which dimensions a window metric requires
- :ref:`explanation-metric-grain` -- How window metrics behave when their inner metric is on another table
- :ref:`howto-semi-additive` -- Snapshot metrics, which cannot be combined with ``OVER``
