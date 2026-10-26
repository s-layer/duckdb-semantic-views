.. meta::
   :description: Feature-by-feature comparison with Databricks metric views, covering concept mapping, syntax differences, and feature coverage

.. _explanation-databricks:

=======================
Databricks Comparison
=======================

Databricks offers `Metric Views <https://docs.databricks.com/aws/en/uc-semantics/metric-views/>`_ as part of its Unity Catalog semantic layer. If you have used Databricks metric views, this page maps the key concepts to DuckDB Semantic Views, highlights the differences, and identifies features unique to each system.

.. note::

   This comparison covers Databricks' documented metric view surface. It was last
   checked against the Databricks documentation in August 2026. Creating a metric
   view requires Databricks Runtime 16.4 or above, and individual YAML features
   require later runtimes.


.. _explanation-db-concepts:

Concept Mapping
===============

.. list-table::
   :header-rows: 1
   :widths: 22 39 39

   * - Concept
     - Databricks Metric Views
     - DuckDB Semantic Views
   * - Define a semantic layer
     - ``CREATE VIEW ... WITH METRICS LANGUAGE YAML AS $$ ... $$``
     - :ref:`CREATE SEMANTIC VIEW <ref-create-semantic-view>`
   * - Table declarations
     - ``source:`` key naming one table, view, or SQL query
     - ``TABLES`` clause with aliases, optional ``PRIMARY KEY``
   * - Multi-table relationships
     - ``joins:`` list. Each entry has a ``name``, a ``source``, an ``on`` or ``using`` condition, and a ``cardinality`` (``many_to_one`` by default). Entries nest for snowflake schemas.
     - ``RELATIONSHIPS`` clause declaring FK ``REFERENCES`` edges between tables. Join paths are derived from that graph per query.
   * - Dimensions
     - ``fields:`` key. ``dimensions:`` is accepted as a synonym, and is what the Catalog Explorer low-code editor emits.
     - ``DIMENSIONS`` clause
   * - Metrics (measures)
     - ``measures:`` key
     - ``METRICS`` clause
   * - Reusable row-level expressions
     - ``fields:`` entries fill both roles: later fields and measures can reference a field by name, and a numeric field can be aggregated at query time.
     - ``FACTS`` clause, declared separately from ``DIMENSIONS`` (queryable with ``FACTS`` in ``SEMANTIC_VIEW(...)``)
   * - Metric composition
     - Measures reference earlier measures through the ``MEASURE()`` function
     - Derived metrics (a metric referencing other metrics)
   * - Semi-additive metrics
     - ``semiadditive: first`` or ``last`` on a window measure, collapsing along that window's ``order`` field
     - ``NON ADDITIVE BY`` on the metric itself, with no window required (see :ref:`howto-semi-additive`)
   * - Window function metrics
     - ``window:`` block on a measure, with ``order``, ``range``, ``semiadditive``, and an optional ``offset``
     - ``OVER`` clause with ``PARTITION BY EXCLUDING`` (see :ref:`howto-window-metrics`)
   * - Metadata annotations
     - ``comment``, ``synonyms``, ``display_name``, and ``format`` on fields and measures
     - ``COMMENT``, ``WITH SYNONYMS``, and ``LABELS = (FILTER)`` (see :ref:`howto-metadata-annotations`)
   * - Access modifiers
     - Unity Catalog ``GRANT`` on the view, plus row filters and column masks
     - ``PRIVATE`` / ``PUBLIC`` on metrics and facts (see :ref:`howto-annotations-access`)
   * - Materializations
     - ``materialization:`` block. Databricks builds and refreshes the materialized views and rewrites queries onto them automatically.
     - ``MATERIALIZATIONS`` clause routing to a table you build and refresh yourself (see :ref:`howto-materializations`)
   * - YAML definitions
     - The only definition language. The SQL statement wraps a YAML document.
     - Optional alternative to the SQL DDL: ``FROM YAML`` import and :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>` export (see :ref:`howto-yaml-definitions`)
   * - View-level filter
     - ``filter:`` key, applied to every query against the view
     - No definition-level filter. Filter per query with a ``WHERE`` inside ``SEMANTIC_VIEW(...)`` or in the outer ``SELECT`` (see :ref:`howto-filtering`).
   * - Query-time parameters
     - ``parameters:`` key, bound by calling the view as a table-valued function
     - No declared parameters. A prepared statement's parameter in the query's ``WHERE`` supplies a run-time value (see :ref:`ref-sv-construct`)
   * - Query interface
     - Standard SQL against the metric view name, with every measure wrapped in ``MEASURE()``
     - Standard SQL against the view name, with every metric wrapped in ``AGG()`` as in Snowflake (see :ref:`howto-standard-sql`); also the :ref:`SEMANTIC_VIEW(...) <ref-sv-construct>` construct and the :ref:`semantic_view() <ref-semantic-view-function>` table function
   * - View inspection
     - ``DESCRIBE TABLE EXTENDED``, or Catalog Explorer
     - :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`, :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>`
   * - Definition retrieval
     - ``DESCRIBE TABLE EXTENDED <name> AS JSON``, which returns the full YAML in the ``View Text`` field
     - :ref:`GET_DDL('SEMANTIC_VIEW', ...) <ref-get-ddl>`


.. _explanation-db-syntax:

Syntax Comparison
=================

The two systems reach the same model through different surfaces. A Databricks metric view is a YAML document -- ``source``, ``joins``, ``fields``, ``measures`` -- embedded in a ``CREATE VIEW`` statement between ``$$`` delimiters. DuckDB Semantic Views declares tables, relationships, and column definitions as SQL clauses.

.. tab-set::
   :sync-group: platform

   .. tab-item:: Databricks
      :sync: databricks

      .. code-block:: duckdb-sql

         CREATE OR REPLACE VIEW main.analytics.revenue_by_region
         WITH METRICS LANGUAGE YAML AS $$
         version: 1.1
         source: main.sales.orders
         fields:
           - name: region
             expr: region
         measures:
           - name: revenue
             expr: SUM(amount)
         $$;

   .. tab-item:: DuckDB Semantic Views
      :sync: duckdb

      .. code-block:: sql

         CREATE SEMANTIC VIEW revenue_by_region AS
         TABLES (
             o AS orders PRIMARY KEY (id)
         )
         DIMENSIONS (
             o.region AS o.region
         )
         METRICS (
             o.revenue AS SUM(o.amount)
         );


.. _explanation-db-differences:

Key Differences
===============

Multi-Table Handling
--------------------

Databricks declares joins in the definition rather than writing them out as SQL. Each entry under ``joins:`` names the joined source, gives the condition, and may assert that the join does not fan out:

.. code-block:: duckdb-sql

   -- Databricks: joins are a declarative list in the YAML definition
   CREATE OR REPLACE VIEW main.analytics.analytics_mv
   WITH METRICS LANGUAGE YAML AS $$
   version: 1.1
   source: main.sales.orders
   joins:
     - name: customer
       source: main.sales.customers
       'on': source.customer_id = customer.id
       rely:
         at_most_one_match: true
   fields:
     - name: customer_name
       expr: customer.name
     - name: region
       expr: region
   measures:
     - name: revenue
       expr: SUM(amount)
   $$;

DuckDB Semantic Views declares the tables separately and lets the extension synthesize JOINs from the declared relationships:

.. code-block:: sql

   -- DuckDB: joins are synthesized from relationships
   CREATE SEMANTIC VIEW analytics AS
   TABLES (
       o AS orders    PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (
       c.customer_name AS c.name,
       o.region AS o.region
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

Both systems join only what a query needs. Databricks joins the source and the dimension tables required by the selected fields and measures; this extension joins only the tables reached by the requested dimensions, facts, and metrics. If a query asks for ``region`` and ``revenue`` alone, neither system touches the customer table.

The difference is in how the join graph is expressed. In Databricks the joins form a tree rooted at ``source``, and each edge carries its own ``on`` or ``using`` condition, so reaching another table means adding an entry -- nested under a dimension table for a snowflake schema. In this extension, ``RELATIONSHIPS`` declares FK/PK edges once, and the extension chooses a path through that graph per query, including multi-hop paths and role-played paths disambiguated with ``USING``.

That difference carries through to fan-out. Databricks' ``rely.at_most_one_match: true`` is an assertion the engine trusts without checking: if the join does fan out, measures return inflated numbers and no error is raised. This extension infers cardinality from the declared ``PRIMARY KEY`` and ``UNIQUE`` constraints and, on a traversal that would fan out, either computes each metric at its own grain or raises a fan trap error -- it does not return an inflated aggregate (see :ref:`howto-fan-traps`).


Query Interface
---------------

Databricks metric views are queried with standard SQL, as if querying a regular table or view. Every measure must be wrapped in the ``MEASURE()`` aggregate function, and ``SELECT *`` is not available, so fields are listed explicitly:

.. code-block:: sql

   -- Databricks: standard SQL, with MEASURE() around each measure
   SELECT region, MEASURE(revenue) AS revenue
   FROM main.analytics.revenue_by_region
   GROUP BY region;

DuckDB Semantic Views takes the same shape, with Snowflake's ``AGG()`` in place of ``MEASURE()`` (see :ref:`howto-standard-sql`); ``SELECT *`` is refused here too:

.. code-block:: sql

   -- DuckDB: standard SQL, with AGG() around each metric
   SELECT region, AGG(revenue) AS revenue
   FROM revenue_by_region
   GROUP BY region;

It can also name the dimensions and metrics explicitly, with Snowflake's :ref:`SEMANTIC_VIEW(...) construct <ref-sv-construct>` or the :ref:`semantic_view() <ref-semantic-view-function>` table function it turns into. There ``SELECT *`` works, and no wrapper is needed:

.. code-block:: sql

   -- DuckDB: the SEMANTIC_VIEW construct
   SELECT * FROM SEMANTIC_VIEW(revenue_by_region
       DIMENSIONS region
       METRICS revenue
   );

   -- DuckDB: the same query as a table function with named lists
   SELECT * FROM semantic_view('revenue_by_region',
       dimensions := ['region'],
       metrics := ['revenue']
   );


Naming: Measures vs Metrics
---------------------------

Databricks names its aggregate columns under a ``measures:`` key. DuckDB Semantic Views uses a ``METRICS`` clause, following Snowflake's naming convention. The concept is the same: named aggregate expressions that the engine evaluates at whatever grain the query asks for.


Dimension Expressions
---------------------

In Databricks, every field carries both a ``name`` and an ``expr``, so a passthrough column is written as ``name: region`` with ``expr: region``. In DuckDB Semantic Views, every dimension is written as ``<logical_name> AS <expression>``, where the logical name (left of ``AS``) must carry a table-alias prefix: ``o.region AS o.region``. The expression on the right of ``AS`` is any SQL expression -- its column references may be qualified (``o.region``) or unqualified (``region``), though qualifying them avoids ambiguity in multi-table views. Computed dimensions use any SQL expression: ``o.month AS date_trunc('month', o.order_date)``.


.. _explanation-db-unique-duckdb:

Features in DuckDB Semantic Views Not in Databricks
====================================================

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - Feature
     - Description
   * - ``NON ADDITIVE BY``
     - Declares a metric non-additive across named dimensions, on the metric itself. Databricks expresses semi-additivity only as ``semiadditive: first`` or ``last`` inside a window measure, which collapses along that window's single ``order`` field.
   * - ``PRIVATE`` / ``PUBLIC``
     - Access modifiers on individual metrics and facts. Databricks controls access at the view level, through Unity Catalog privileges, row filters, and column masks.
   * - ``RELATIONSHIPS``
     - FK/PK edges declared once between tables, with cardinality inferred from ``PRIMARY KEY`` and ``UNIQUE`` declarations and the join path chosen per query. Databricks joins are a tree rooted at ``source``, with each edge's condition written out and its cardinality asserted by hand.
   * - Fan-trap detection
     - Automatic detection of one-to-many traversals that would inflate an aggregate. The extension computes each metric at its own grain where it can, and raises a fan trap error where it cannot, rather than returning an inflated number. Databricks does not validate ``rely.at_most_one_match``, and picks the first matching row when a many-to-one join turns out to be many-to-many.
   * - Role-playing dimensions
     - ``USING`` clause on a metric to choose between multiple join paths to the same table at query time. In Databricks each role is a separate named join entry, fixed at definition time.
   * - :ref:`explain_semantic_view() <ref-explain-semantic-view>`
     - Returns the SQL the extension generates for a request, before running it. Databricks exposes the compiled plan through the query profile rather than the generated SQL.


.. _explanation-db-unique-databricks:

Features in Databricks Not in DuckDB Semantic Views
====================================================

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - Feature
     - Description
   * - Unity Catalog integration
     - Metric views are first-class catalog objects with lineage tracking, access control, and governance.
   * - Row-level security / column masking
     - Databricks provides fine-grained access control at the workspace level. DuckDB defers access control to DuckDB's own mechanisms.
   * - AI/BI integration
     - Metric views power Databricks AI/BI dashboards and natural-language queries through Genie Agents, which apply ``MEASURE()`` and the declared agent metadata automatically.
   * - Managed materialization
     - Databricks builds and refreshes materialized views from the ``materialization:`` block through a managed pipeline, then rewrites queries onto them automatically -- on an exact match, a rollup to coarser dimensions, or an unaggregated match. This extension's ``MATERIALIZATIONS`` clause routes only on an exact dimension and metric name match, to a table you build and refresh.
   * - View-level ``filter``
     - A predicate in the definition that applies to every query against the view. This extension filters per query instead, with a ``WHERE`` inside ``SEMANTIC_VIEW(...)`` or an outer ``WHERE``.
   * - Query-time parameters
     - Named values declared with ``parameters:`` and passed by calling the metric view as a table-valued function, so one definition serves several query variants.
   * - Display names and number formats
     - ``display_name`` and ``format`` on fields and measures drive labels and value formatting in downstream tools. This extension carries ``COMMENT`` and ``WITH SYNONYMS``, but no display name or format.


.. _explanation-db-choosing:

Choosing Between Them
=====================

Databricks metric views are purpose-built for the Databricks ecosystem. They integrate with Unity Catalog, AI/BI dashboards, and the broader Databricks workspace. If your data already lives in Databricks and your team uses the Databricks platform, metric views fit naturally into the workflow.

DuckDB Semantic Views targets a different use case: lightweight, local-first analytics with an open-source, embeddable engine. It is designed for data engineers who want a semantic layer that runs anywhere DuckDB runs -- inside an application server, in a notebook, or on a developer laptop -- without depending on a cloud platform. The tables it models can be anything DuckDB can read, including Parquet files, Postgres, and Iceberg (see :ref:`howto-data-sources`).

The two systems are not interchangeable. They solve the same conceptual problem (define metrics once, query flexibly) but for different deployment models and ecosystems.
