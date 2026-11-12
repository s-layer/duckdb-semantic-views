.. meta::
   :description: Goal-oriented guides for standard-SQL queries, view variables, tables defined by a query, CREATE OR ALTER, FACTS, derived metrics, role-playing dimensions, fan trap and multi-grain resolution, ASOF and range relationships, many-to-many bridges, data source connectivity, query filtering, metadata annotations, semi-additive metrics, window metrics, wildcard selection, fact queries, materializations, and YAML definitions

.. _how-to-guides:

=============
How-To Guides
=============

Goal-oriented guides for specific tasks with DuckDB Semantic Views.

**Modeling**

- :ref:`howto-facts` -- Define reusable row-level expressions that can be referenced inside metric aggregations.
- :ref:`howto-derived-metrics` -- Compose metrics from other metrics using arithmetic without repeating aggregate logic.
- :ref:`howto-role-playing` -- Join the same table through several named relationships, pick the route with ``USING``, and query a diamond.
- :ref:`howto-fan-traps` -- See how a metric grouped below its grain is counted, which queries still raise the fan trap error, and how to fix them.
- :ref:`howto-asof-range` -- Join each row to the record in effect at its date, or to the period that holds it, with ASOF and range relationships.
- :ref:`howto-many-to-many` -- Group a metric by a dimension linked many-to-many through a bridge table, counting each row once per group.
- :ref:`howto-query-tables` -- Make a ``TABLES`` entry a query that joins, filters or pre-aggregates source tables inside the view.

**Advanced Metrics**

- :ref:`howto-semi-additive` -- Define metrics with NON ADDITIVE BY for snapshot data like account balances and inventory levels.
- :ref:`howto-window-metrics` -- Define window function metrics for rolling averages, lag comparisons, and rankings using OVER clauses.

**Data & Queries**

- :ref:`howto-data-sources` -- Connect semantic views to CSV, Parquet, Iceberg, and database tables.
- :ref:`howto-filtering` -- Filter a query before aggregation with a ``WHERE`` inside ``SEMANTIC_VIEW(...)`` or after it with an outer ``WHERE``, tell which one a filter needs, and pass request values from application code safely.
- :ref:`howto-variables` -- Declare thresholds, rates and reporting dates as view variables and set them per query or from application code.
- :ref:`howto-standard-sql` -- Query a semantic view by name with an ordinary ``SELECT ... GROUP BY``, passing each metric to ``AGG``.
- :ref:`howto-query-facts` -- Query facts directly as row-level columns without aggregation.
- :ref:`howto-wildcard-selection` -- Select all dimensions, metrics, or facts for a table alias using wildcard patterns in queries.

**Operations**

- :ref:`howto-metadata-annotations` -- Add comments, synonyms, access modifiers, and named filters to dimensions, metrics, facts, and tables.
- :ref:`howto-materializations` -- Declare materializations that route matching queries to pre-aggregated tables.
- :ref:`howto-yaml-definitions` -- Import and export semantic view definitions as YAML for version control and migration.
- :ref:`howto-create-or-alter` -- Deploy a view definition repeatedly with ``CREATE OR ALTER``, keeping its creation time, tags and still-valid materializations.

.. toctree::
   :hidden:

   facts
   derived-metrics
   role-playing-dimensions
   fan-traps
   asof-and-range-relationships
   many-to-many-relationships
   query-defined-tables
   data-sources
   filtering
   view-variables
   standard-sql
   metadata-annotations
   semi-additive-metrics
   window-metrics
   wildcard-selection
   query-facts
   materializations
   yaml-definitions
   create-or-alter
