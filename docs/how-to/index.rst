.. meta::
   :description: Goal-oriented guides for standard-SQL queries, FACTS, derived metrics, role-playing dimensions, fan trap and multi-grain resolution, data source connectivity, query filtering, metadata annotations, semi-additive metrics, window metrics, wildcard selection, fact queries, materializations, and YAML definitions

.. _how-to-guides:

=============
How-To Guides
=============

Goal-oriented guides for specific tasks with DuckDB Semantic Views.

**Modeling**

- :ref:`howto-facts` -- Define reusable row-level expressions that can be referenced inside metric aggregations.
- :ref:`howto-derived-metrics` -- Compose metrics from other metrics using arithmetic without repeating aggregate logic.
- :ref:`howto-role-playing` -- Join the same table through several named relationships, pick the route with ``USING``, and fix an ambiguous diamond.
- :ref:`howto-fan-traps` -- Diagnose and fix the fan trap error in multi-table views, and see which queries across several grains are answered.

**Advanced Metrics**

- :ref:`howto-semi-additive` -- Define metrics with NON ADDITIVE BY for snapshot data like account balances and inventory levels.
- :ref:`howto-window-metrics` -- Define window function metrics for rolling averages, lag comparisons, and rankings using OVER clauses.

**Data & Queries**

- :ref:`howto-data-sources` -- Connect semantic views to CSV, Parquet, Iceberg, and database tables.
- :ref:`howto-filtering` -- Filter a query before aggregation with a ``WHERE`` inside ``SEMANTIC_VIEW(...)`` or after it with an outer ``WHERE``, tell which one a filter needs, and pass request values from application code safely.
- :ref:`howto-standard-sql` -- Query a semantic view by name with an ordinary ``SELECT ... GROUP BY``, passing each metric to ``AGG``.
- :ref:`howto-query-facts` -- Query facts directly as row-level columns without aggregation.
- :ref:`howto-wildcard-selection` -- Select all dimensions, metrics, or facts for a table alias using wildcard patterns in queries.

**Operations**

- :ref:`howto-metadata-annotations` -- Add comments, synonyms, access modifiers, and named filters to dimensions, metrics, facts, and tables.
- :ref:`howto-materializations` -- Declare materializations that route matching queries to pre-aggregated tables.
- :ref:`howto-yaml-definitions` -- Import and export semantic view definitions as YAML for version control and migration.

.. toctree::
   :hidden:

   facts
   derived-metrics
   role-playing-dimensions
   fan-traps
   data-sources
   filtering
   standard-sql
   metadata-annotations
   semi-additive-metrics
   window-metrics
   wildcard-selection
   query-facts
   materializations
   yaml-definitions
