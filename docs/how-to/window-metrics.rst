.. meta::
   :description: Define window function metrics with OVER clause, PARTITION BY, PARTITION BY EXCLUDING, ORDER BY, and frame clauses for rolling averages, lag comparisons, and rankings

.. _howto-window-metrics:

=======================================
How to Use Window Function Metrics
=======================================

This guide shows how to define metrics that use SQL window functions -- rolling averages, lag comparisons, running totals, and rankings -- using the ``OVER`` clause in the ``METRICS`` section, and how to give each row a window value with a window fact or dimension (see :ref:`howto-window-members`).

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

With neither, the window runs over every row of the result, as in Snowflake. ``SUM(total_qty) OVER (ORDER BY date)`` is a running total across the whole result, whatever other dimensions the query requests. Rows with the same ``date`` are peers, so they share one value:

.. code-block:: text

   Query requests dimensions [store, date]:
   OVER (ORDER BY date)  ->  OVER (ORDER BY date)   (no partition: one running total over every store)
   OVER ()               ->  OVER ()                (the grand total on every row)

A window with a frame (``ROWS BETWEEN ...``), or with an order-sensitive function (``LAG``, ``LEAD``, ``FIRST_VALUE``, ``LAST_VALUE``, ``NTH_VALUE``, ``ROW_NUMBER``, ``RANK``, ``DENSE_RANK``, ``PERCENT_RANK``, ``CUME_DIST`` or ``NTILE``), needs an ``ORDER BY``. Without one the rows have no defined order, so ``CREATE SEMANTIC VIEW`` refuses it, as Snowflake does. ``SUM``, ``AVG`` and the other aggregates take an empty ``OVER ()``, which is the total over the whole result.

.. versionchanged:: 0.19.0

   A window with no ``PARTITION BY`` used to be partitioned by every queried dimension, so each row was its own partition and ``SUM(total_qty) OVER (ORDER BY date)`` returned each row's own ``total_qty``.


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


.. _howto-window-beside:

Query a Window Beside Other Metrics
===================================

A query can hold window metrics and aggregate metrics together. Each row shows the aggregate and the window computed over it:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS store, date
       METRICS total_qty, rolling_avg
   );

.. code-block:: text

   store | date       | total_qty | rolling_avg
   ------+------------+-----------+------------
   A     | 2024-01-01 |         5 |         5.0
   A     | 2024-01-02 |         3 |         4.0
   B     | 2024-01-01 |         7 |         7.0

The extension answers the query without the window metrics first, with each window's inner metric added to it, and then computes the windows over that result. So a window sees exactly the rows and values the same query without it returns, and a ``WHERE`` filter applies to the rows before the window runs.

.. versionchanged:: 0.19.0

   A query used to fail with *cannot mix window function metrics with aggregate metrics* when it held both kinds.


.. _howto-window-relationships:

Windows Across Relationships
============================

Because a window runs over its inner metric's result, it follows the same rules as that metric:

- **Dimensions of other tables.** A window can be queried with dimensions of any table its inner metric can be queried with, including tables reached through a many-to-many bridge. Each group's value is counted once, as the inner metric counts it.
- **USING paths and roles.** A window cannot be computed over a metric with ``USING``: as in Snowflake, only a derived metric may refer to one, so ``CREATE SEMANTIC VIEW`` refuses it. A ``USING`` written on the window metric itself applies to its inner metric, which must then be on the window metric's table.
- **Fan-outs.** A window queried with a dimension of a table that references the metric's table (orders by an item's quantity) counts each order once per group, as the inner metric does.

The dimensions a window clause names (in ``PARTITION BY``, ``PARTITION BY EXCLUDING`` and ``ORDER BY``) follow Snowflake's rules:

- A bare name names a dimension of the metric's own table. Write ``alias.name`` for any other table's: ``OVER (PARTITION BY r.region ORDER BY o.day)`` partitions an order metric by its region and orders it by its own day.
- The other table must be one the metric's table references, along one chain of relationships. ``CREATE SEMANTIC VIEW`` refuses a dimension of a finer table (a line item's quantity in a window over an order metric), of a table reached more than one way (airports, which flights reference as departure and as arrival), and of an unrelated table, such as one on the other side of a many-to-many bridge or a sibling under the same parent:

.. code-block:: text

   Window metric 'running_pages': its window clause names dimension 'a.country' of table 'a',
   which is unrelated to the metric's table 'b': ...

.. versionchanged:: 0.19.0

   ``PARTITION BY`` and ``PARTITION BY EXCLUDING`` accept the ``alias.name`` form, which ``ORDER BY`` already accepted. ``CREATE`` refuses a bare name of another table's dimension, a dimension of a finer table or of one reached more than one way, a window over a metric with ``USING``, and a frame or an order-sensitive function with no ``ORDER BY``, as Snowflake does. A view created by 0.18.0 with one of these windows keeps loading and answering as before, and its ``GET_DDL`` shows the window as written; rewrite the window before running that DDL again.


.. _howto-window-members:

Rank or Total Each Row with a Window Fact or Dimension
======================================================

.. versionadded:: 0.20.0

A window metric is computed over the query's result. To give each **row** a value computed over its table instead, such as a sale's rank or its category's revenue, put the window function in a fact or a dimension, as in Snowflake. Its value is computed over every row of the table, before the query's ``WHERE``.

.. code-block:: sql

   CREATE TABLE win_stores (store_id INTEGER, region VARCHAR);
   INSERT INTO win_stores VALUES (1, 'North'), (2, 'South');

   CREATE TABLE win_sales (id INTEGER, store_id INTEGER, category VARCHAR, units INTEGER, revenue DECIMAL(10,2));
   INSERT INTO win_sales VALUES
       (1, 1, 'A', 5, 50), (2, 1, 'B', 2, 30), (3, 2, 'A', 8, 70),
       (4, 2, 'B', 1, 10), (5, 1, 'A', 3, 20);

   CREATE SEMANTIC VIEW store_sales
   TABLES (
       st    AS win_stores PRIMARY KEY (store_id),
       sales AS win_sales  PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       sale_store AS sales(store_id) REFERENCES st
   )
   FACTS (
       sales.revenue_by_category AS SUM(revenue) OVER (PARTITION BY category),
       sales.region_revenue      AS SUM(revenue) OVER (PARTITION BY st.region)
   )
   DIMENSIONS (
       sales.sale_id    AS id,
       sales.category   AS category,
       st.region        AS region,
       sales.units_rank AS ROW_NUMBER() OVER (ORDER BY units DESC)
   )
   METRICS (
       sales.revenue     AS SUM(revenue),
       sales.max_cat_rev AS MAX(revenue_by_category)
   );

A query filtered to category ``A`` still gets each sale's rank among all five sales:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(store_sales
       DIMENSIONS sales.sale_id, sales.units_rank
       WHERE sales.category = 'A'
   ) ORDER BY sale_id;

.. code-block:: text

   ┌─────────┬────────────┐
   │ sale_id │ units_rank │
   ├─────────┼────────────┤
   │       1 │          2 │
   │       3 │          1 │
   │       5 │          3 │
   └─────────┴────────────┘

A window fact can read a table its own table references, here the store's region. ``FACTS`` lists each sale's values:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(store_sales
       FACTS sales.sale_id, sales.revenue_by_category, sales.region_revenue
   ) ORDER BY sale_id;

.. code-block:: text

   ┌─────────┬─────────────────────┬────────────────┐
   │ sale_id │ revenue_by_category │ region_revenue │
   ├─────────┼─────────────────────┼────────────────┤
   │       1 │              140.00 │         100.00 │
   │       2 │               40.00 │         100.00 │
   │       3 │              140.00 │          80.00 │
   │       4 │               40.00 │          80.00 │
   │       5 │              140.00 │         100.00 │
   └─────────┴─────────────────────┴────────────────┘

A query can group by a window dimension and filter on one, and a metric can aggregate a window fact, such as ``max_cat_rev`` above:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(store_sales
       METRICS sales.revenue
       WHERE sales.units_rank <= 2
   );

.. code-block:: text

   ┌───────────────┐
   │    revenue    │
   ├───────────────┤
   │        120.00 │
   └───────────────┘

The two sales with the most units, 3 and 1, made 120.00. ``CREATE`` refuses a window fact or dimension that reads a child table or a table reached by more than one path. See :ref:`window functions in facts and dimensions <ref-create-window-members>` for the full rules.


.. _howto-window-verify:

Verify the Generated SQL
=========================

Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to inspect the CTE expansion:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('sales',
       dimensions := ['store', 'date'],
       metrics := ['total_qty', 'rolling_avg']
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
       "total_qty" AS "total_qty",
       AVG("total_qty") OVER (PARTITION BY "store" ORDER BY "date" ASC NULLS LAST) AS "rolling_avg"
   FROM __sv_agg

1. The ``WITH __sv_agg`` step is the query without its window metrics, with each window's inner metric (here ``total_qty``) added. It aggregates by all queried dimensions.
2. The outer ``SELECT`` reads the dimensions and aggregate metrics from those rows and applies the window function to them. ``PARTITION BY EXCLUDING date`` has become ``PARTITION BY "store"``, because ``store`` is the only other queried dimension.


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
   A window metric cannot be the inner metric of another window metric; reference a base or derived metric instead. The error message is: ``Window metric 'X': inner metric 'Y' is itself a window function metric. A window metric must be computed over an aggregate metric.``

**Window clause names an unrelated dimension**
   A dimension in ``PARTITION BY``, ``PARTITION BY EXCLUDING`` or ``ORDER BY`` must be on a table related to the metric's table (see :ref:`howto-window-relationships`). The error message starts: ``Window metric 'X': its window clause names dimension 'Y' of table 'T', which is unrelated to the metric's table 'M'``. A dimension of a table that references the metric's table is refused with ``... so it is at a finer grain``, and one of a table the metric's table reaches more than one way with ``... reaches by more than one path``.

**Bare name of another table's dimension**
   A bare name in a window clause names a dimension of the metric's own table. Qualify another table's: ``Window metric 'X': its window clause names dimension 'Y' bare, but it is on table 'T', not on the metric's table 'M'. ... write 'T.Y' to name another table's.``

**Window over a metric with USING**
   Write the window over a metric without ``USING``: ``Window metric 'X' is computed over metric 'Y', which has a USING clause. As in Snowflake, only a derived metric may refer to a metric with USING; write the window over a metric without one.``

**Window frame or order-sensitive function without ORDER BY**
   Add an ``ORDER BY`` to the window: ``Window metric 'X': a window frame requires an ORDER BY clause, as in Snowflake.`` or ``Window metric 'X': window function LAG requires ORDER BY in its window specification, as in Snowflake.``


.. _howto-window-related:

Related
=======

- :ref:`ref-create-metrics` -- Full metric grammar, including the ``OVER`` clause
- :ref:`ref-show-dims-for-metric` -- See which dimensions a window metric requires
- :ref:`explanation-metric-grain` -- How window metrics behave when their inner metric is on another table
- :ref:`howto-semi-additive` -- Snapshot metrics, which cannot be combined with ``OVER``
