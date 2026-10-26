.. meta::
   :description: Explains the difference between storing a fixed SQL query in a view versus storing a model that generates queries on demand

.. _explanation-sv-vs-views:

======================================
Semantic Views vs. Regular SQL Views
======================================

If you are coming from standard SQL, you already know ``CREATE VIEW``. A semantic view is a different kind of abstraction with different capabilities and trade-offs. This page explains what each does, where they overlap, and why semantic views exist.


.. _explanation-sv-what-is-view:

What a Regular View Does
=========================

A regular SQL view stores a fixed query. When you ``SELECT FROM`` the view, DuckDB substitutes the view's query and executes it.

.. code-block:: sql

   CREATE VIEW revenue_by_region AS
   SELECT region, SUM(amount) AS revenue
   FROM orders
   GROUP BY region;

   -- Querying the view always runs the same query
   SELECT * FROM revenue_by_region;

The view is a single, predetermined query. Every column, join, and GROUP BY is baked in. To get revenue by category instead of by region, you create a second view.


.. _explanation-sv-what-is-sv:

What a Semantic View Does
=========================

A semantic view stores a *model*, a set of dimensions, metrics, relationships, and facts. It does not store a query. Instead, it generates a query on demand based on what you request.

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region   AS o.region,
       o.category AS o.category
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   );

   -- Revenue by region
   SELECT * FROM SEMANTIC_VIEW(order_metrics
       DIMENSIONS region
       METRICS revenue
   );

   -- Revenue by category (same view, different request)
   SELECT * FROM SEMANTIC_VIEW(order_metrics
       DIMENSIONS category
       METRICS revenue
   );

One semantic view definition serves many different queries. The extension generates the appropriate SQL (SELECT, GROUP BY, and JOIN) based on which dimensions and metrics are requested.

The same request can be written as an ordinary query over the view by name, which reads much like a query over a table:

.. code-block:: sql

   SELECT category, AGG(revenue)
   FROM order_metrics
   GROUP BY category;

It only looks like a table query. ``order_metrics`` has no rows of its own and ``revenue`` is not a column: ``GROUP BY`` chooses the grain, ``AGG()`` asks for a metric already defined in the view, and the extension writes the aggregation. ``SUM(revenue)`` is refused, because ``revenue`` is already an aggregate. See :ref:`howto-standard-sql`.


.. _explanation-sv-key-differences:

Key Differences
===============

.. list-table::
   :header-rows: 1
   :widths: 25 35 35

   * -
     - Regular View
     - Semantic View
   * - Stores
     - A fixed SQL query
     - A model (dimensions, metrics, relationships)
   * - Query flexibility
     - One fixed output shape
     - Any combination of dimensions and metrics
   * - JOIN logic
     - Baked into the query
     - Generated per request; unused tables excluded
   * - GROUP BY logic
     - Baked into the query
     - Generated based on requested dimensions
   * - Metric consistency
     - Each view defines its own aggregation
     - Defined once, reused across all queries
   * - Checked at ``CREATE``
     - The stored query is bound
     - Each dimension, fact and metric is bound
   * - Query interface
     - ``SELECT * FROM view``
     - :ref:`SELECT * FROM SEMANTIC_VIEW(name DIMENSIONS ... METRICS ...) <ref-semantic-view-function>`, or :ref:`SELECT dim, AGG(metric) FROM name GROUP BY dim <howto-standard-sql>`


.. _explanation-sv-why:

Why Semantic Views Exist
========================

The core problem semantic views solve is **metric inconsistency**. In a traditional analytics setup, every dashboard, report, and ad-hoc query defines its own GROUP BY and JOIN logic. Revenue might be calculated differently across three dashboards: one includes discounts, another does not, a third uses a different join path.

A semantic view defines each metric once:

.. code-block:: sql

   METRICS (
       o.revenue AS SUM(o.amount)
   )

Every query that requests ``revenue`` gets the same calculation. There is no possibility of drift between different consumers of the same metric.

The second problem is **join management**. In a star schema with five dimension tables, a regular view must join all five tables even if a query only needs one. A semantic view joins only the tables that the requested dimensions require. This reduces query complexity and can improve performance.


.. _explanation-sv-tradeoffs:

Trade-Offs
==========

Semantic views are not a replacement for regular views in all cases.

**When regular views are better:**

- Fixed reports where the query shape never changes
- Complex queries that cannot be expressed as dimension/metric combinations: CTEs, correlated subqueries, or window functions over raw rows rather than over an aggregated metric. A window function over a metric -- a rolling average, a lag comparison, a running total -- is available as a :ref:`window metric <howto-window-metrics>`.
- Queries that need fine-grained control over join types, ordering, or LIMIT within the view

**When semantic views are better:**

- Multiple consumers (dashboards, reports, APIs) querying the same data model with different dimension/metric combinations
- Star or snowflake schemas where join management across many tables is error-prone
- Analytics layers where metric consistency matters (everyone should compute revenue the same way)
- Exploration workflows where analysts want to slice data by different dimensions without writing new queries


.. _explanation-sv-materialization:

Materialization Support
========================

.. versionadded:: 0.7.0

By default, semantic views do not store data. The extension is a *preprocessor*: it generates SQL and hands it to DuckDB for execution. Each query runs fresh against the underlying tables.

The ``MATERIALIZATIONS`` clause lets you optionally route queries to pre-aggregated tables. When a query's requested dimensions and metrics exactly match a declared materialization, the extension reads from the pre-aggregated table instead of expanding raw sources with JOINs and GROUP BY.

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region AS o.region
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   )
   MATERIALIZATIONS (
       region_agg AS (
           TABLE revenue_by_region,
           DIMENSIONS (region),
           METRICS (revenue)
       )
   );

This is not automatic caching or background refresh. You create and maintain the pre-aggregated table yourself (or use external tools like dbt). The extension routes matching queries to it. For queries that do not match any materialization, standard on-demand SQL generation continues as before.

Materialization routing is transparent to the caller -- the :ref:`query <ref-semantic-view-function>` does not change. See :ref:`howto-materializations` for a detailed guide.
