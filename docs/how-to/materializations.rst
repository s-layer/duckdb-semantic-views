.. meta::
   :description: Declare materializations that route matching queries to pre-aggregated tables instead of expanding raw sources

.. _howto-materializations:

===========================
How to Use Materializations
===========================

.. versionadded:: 0.7.0

This guide shows how to declare materializations in a semantic view so that queries whose dimensions and metrics exactly match a materialization are routed to a pre-aggregated table instead of expanding raw sources with JOINs and GROUP BY. Materializations can improve query performance for common access patterns without changing the query interface.

**Prerequisites:**

- A working semantic view with ``TABLES``, ``DIMENSIONS``, and ``METRICS`` (see :ref:`tutorial-multi-table`)
- A pre-aggregated table containing the pre-computed results you want to route to


.. _howto-materializations-concept:

How Materializations Work
=========================

A materialization maps a set of dimensions and metrics to a physical table that already contains the pre-aggregated results for that exact combination. When a query requests that exact set of dimensions and metrics, the extension reads directly from the pre-aggregated table instead of scanning raw tables, building JOINs, and computing GROUP BY.

The query (:ref:`SEMANTIC_VIEW(...) or semantic_view() <ref-semantic-view-function>`) does not change. Routing is transparent -- the caller does not specify which table to use. The extension decides based on the materialization declarations.


.. _howto-materializations-declare:

Declare a Materialization
=========================

Add a ``MATERIALIZATIONS`` clause after ``METRICS`` in the ``CREATE SEMANTIC VIEW`` DDL. Each materialization entry names a pre-aggregated table and lists the dimensions and metrics it covers.

First, create the pre-aggregated table:

.. code-block:: sql

   CREATE TABLE revenue_by_region AS
   SELECT region, SUM(amount) AS revenue, COUNT(*) AS order_count
   FROM orders
   GROUP BY region;

Then declare a semantic view with a materialization pointing to that table:

.. code-block:: sql
   :emphasize-lines: 12-18

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region AS o.region
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   )
   MATERIALIZATIONS (
       region_agg AS (
           TABLE revenue_by_region,
           DIMENSIONS (region),
           METRICS (revenue, order_count)
       )
   );

The ``MATERIALIZATIONS`` clause must appear after all other clauses. The clause order is: ``TABLES``, ``RELATIONSHIPS``, ``FACTS``, ``DIMENSIONS``, ``METRICS``, ``MATERIALIZATIONS``.

.. tip::

   The pre-aggregated table must have columns named after the dimensions and metrics it covers. The extension selects columns by name from the materialization table.


.. _howto-materializations-routing:

How Routing Works
=================

Routing uses **exact match** logic. A materialization matches a query when both of these conditions are true:

1. The materialization's dimension set **exactly equals** the requested dimension set.
2. The materialization's metric set **exactly equals** the requested metric set.

Matching is case-insensitive. If a query requests ``region`` and ``revenue``, a materialization declaring ``Region`` and ``Revenue`` still matches.

When a match is found, the extension generates a simple ``SELECT ... FROM <materialization_table>`` instead of the usual JOINs and GROUP BY. When no match is found, the extension falls back to standard expansion against the raw tables.

.. code-block:: sql

   -- This query matches the materialization (exact dims + metrics)
   -- Routes to revenue_by_region
   SELECT * FROM SEMANTIC_VIEW(order_metrics
       DIMENSIONS region
       METRICS revenue, order_count
   );

   -- This query does NOT match (different metric set)
   -- Falls back to standard expansion
   SELECT * FROM SEMANTIC_VIEW(order_metrics
       DIMENSIONS region
       METRICS revenue
   );

.. warning::

   Superset matching is not supported. If a materialization covers ``[region]`` + ``[revenue, order_count]`` and a query requests only ``[region]`` + ``[revenue]``, the materialization does **not** match. The query falls back to standard expansion.


.. _howto-materializations-multiple:

Multiple Materializations
=========================

A semantic view can declare multiple materializations for different access patterns. The extension scans materializations in **definition order** and uses the **first match**.

.. code-block:: sql
   :emphasize-lines: 13-24

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region AS o.region,
       o.status AS o.status
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   )
   MATERIALIZATIONS (
       region_agg AS (
           TABLE revenue_by_region,
           DIMENSIONS (region),
           METRICS (revenue, order_count)
       ),
       region_status_agg AS (
           TABLE revenue_by_region_status,
           DIMENSIONS (region, status),
           METRICS (revenue)
       )
   );

Queries match whichever materialization covers their exact dimension and metric set. If two materializations cover the same set, the first one declared wins.


.. _howto-materializations-exclusions:

Routing Exclusions
==================

Two kinds of metrics are **always excluded** from materialization routing, even when a materialization declaration covers them:

- **Semi-additive metrics** (declared with ``NON ADDITIVE BY``): These pick the latest snapshot row at query time, which a pre-aggregated table cannot reproduce.
- **Window metrics** (declared with ``OVER``): Their values depend on the dimensions in each query, so one pre-computed table cannot serve them.

When a query includes any semi-additive or window metric, the extension skips all materializations and falls back to standard expansion, regardless of whether a matching materialization exists.

.. code-block:: sql

   CREATE SEMANTIC VIEW account_metrics AS
   TABLES (
       a AS accounts PRIMARY KEY (id)
   )
   DIMENSIONS (
       a.customer_id  AS a.customer_id,
       a.report_date  AS a.report_date
   )
   METRICS (
       a.total_balance NON ADDITIVE BY (report_date) AS SUM(a.balance)
   )
   MATERIALIZATIONS (
       balance_agg AS (
           TABLE balance_by_customer,
           DIMENSIONS (customer_id),
           METRICS (total_balance)
       )
   );

   -- This query falls back to standard expansion (semi-additive metric)
   -- even though a matching materialization exists
   SELECT * FROM SEMANTIC_VIEW(account_metrics
       DIMENSIONS customer_id
       METRICS total_balance
   );


.. _howto-materializations-verify:

Verify Routing with explain_semantic_view()
===========================================

Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to check whether a query routes to a materialization. The output includes a ``-- Materialization:`` header line that shows either the materialization name or ``none``.

.. code-block:: sql

   SELECT * FROM explain_semantic_view('order_metrics',
       dimensions := ['region'],
       metrics := ['revenue', 'order_count']
   );

Sample output when routing matches:

.. code-block:: text

   -- Semantic View: order_metrics
   -- Dimensions: region
   -- Metrics: revenue, order_count
   -- Materialization: region_agg

   -- Expanded SQL:
   SELECT
       "region",
       "revenue",
       "order_count"
   FROM "memory"."main"."revenue_by_region"

The output continues with a ``-- DuckDB Plan:`` section, which shows a single
scan of the materialization table.

When no materialization matches, the output shows ``-- Materialization: none`` and the expanded SQL contains the standard JOINs and GROUP BY.


.. _howto-materializations-inspect:

Inspect with SHOW and DESCRIBE
==============================

Use :ref:`SHOW SEMANTIC MATERIALIZATIONS <ref-show-semantic-materializations>` to list all materializations declared in a view or across all views:

.. code-block:: sql

   -- List materializations for a specific view
   SHOW SEMANTIC MATERIALIZATIONS IN order_metrics;

   -- List materializations across all views
   SHOW SEMANTIC MATERIALIZATIONS;

Use :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` to see materialization details alongside all other view objects:

.. code-block:: sql

   DESCRIBE SEMANTIC VIEW order_metrics;

The materialization rows come last, after the tables, dimensions, and metrics. To see only those rows, filter the output by ``object_kind``:

.. code-block:: sql

   SELECT object_name, property, property_value
   FROM (DESCRIBE SEMANTIC VIEW order_metrics)
   WHERE object_kind = 'MATERIALIZATION';


.. _howto-materializations-staleness:

Keep Materialization Tables Fresh
=================================

Materialization tables are user-owned pre-aggregations. The routing engine
matches a request against a materialization by **name sets only** -- it does
not validate that the table's contents agree with the view's current metric
expressions, nor that the table is fresh:

- If you redefine a metric's expression with ``CREATE OR REPLACE SEMANTIC
  VIEW`` but keep the metric name, routed queries keep returning the old
  pre-aggregated numbers until you rebuild the table.
- If the base tables change after the pre-aggregation was built, routed
  results are stale until you rebuild it.
- A missing or misnamed column in the materialization table surfaces as a
  binder error at query time (the routed SQL selects columns by dimension
  and metric name).

Rebuild materialization tables whenever the underlying data or the view
definition changes. Use :ref:`ref-explain-semantic-view` to confirm whether
a given request routes to a materialization or falls back to raw expansion.

Dimensions-only routed queries apply ``SELECT DISTINCT``, matching the raw
expansion path's semantics, so duplicate rows in a pre-aggregation table do
not change dimension listings.


.. _howto-materializations-troubleshooting:

Troubleshooting
===============

**Query not routing to materialization when expected**
   - Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to check the ``-- Materialization:`` line.
   - Verify the dimension and metric sets in the query **exactly match** the materialization declaration (supersets and subsets do not match).
   - Check whether any metric in the query is semi-additive (``NON ADDITIVE BY``) or a window metric (``OVER``). These are always excluded from routing.

**Materialization table has wrong column names**
   The materialization table must have columns named to match the dimension and metric names declared in the semantic view. If the pre-aggregated table uses different column names, the extension cannot select from it correctly.

**DDL error: dimension or metric not found**
   Materialization declarations reference dimensions and metrics by name. The names must match dimensions and metrics declared earlier in the same ``CREATE SEMANTIC VIEW`` statement. The error message suggests close matches.

**DDL error: MATERIALIZATIONS clause out of order**
   The ``MATERIALIZATIONS`` clause must appear after ``METRICS``. Move it to the end of the DDL body.

**DDL error: must specify at least one of DIMENSIONS or METRICS**
   Each materialization entry must declare at least one dimension or one metric. A materialization with only a ``TABLE`` and neither ``DIMENSIONS`` nor ``METRICS`` is rejected.

See :ref:`ref-error-messages` for the full list of materialization validation errors.


.. _howto-materializations-related:

Related
=======

- :ref:`ref-create-semantic-view` -- Full ``MATERIALIZATIONS`` clause syntax
- :ref:`ref-show-semantic-materializations` -- List materializations across views
- :ref:`ref-explain-semantic-view` -- Verify routing decisions
- :ref:`howto-semi-additive` -- Semi-additive metrics (excluded from routing)
- :ref:`howto-window-metrics` -- Window metrics (excluded from routing)
