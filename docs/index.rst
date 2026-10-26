.. meta::
   :description: A DuckDB extension that brings Snowflake-style semantic views to DuckDB -- define dimensions and metrics as DDL, query any combination with SEMANTIC_VIEW(...) or a plain SELECT ... GROUP BY

.. _overview:

========================
DuckDB Semantic Views
========================

.. raw:: html

   <p class="cycling-subtitle">
     The semantic layer for your <span id="typed-target"></span>
   </p>
   <script src="https://unpkg.com/typed.js@3.0.0/dist/typed.umd.js"></script>
   <script>
     new Typed('#typed-target', {
       strings: ['Iceberg tables', 'CSV files', 'DuckLake', 'dataframes'],
       typeSpeed: 50,
       backSpeed: 30,
       backDelay: 2000,
       loop: true,
       showCursor: true,
       cursorChar: '|',
     });
   </script>

A `Semantic Layer <https://www.databricks.com/blog/what-is-a-semantic-layer>`_ sits between your raw tables and the people querying them. Instead of everyone writing their own ``SUM(amount)`` and hoping they ``GROUP BY`` the same columns, you define each metric and dimension once, in one place. Analysts pick the ones they want; the system assembles the SQL.

Snowflake and Databricks, along with dbt Cloud, Cube.dev and others, all ship semantic layers in different forms. Snowflake has `Semantic Views <https://docs.snowflake.com/en/user-guide/views-semantic/overview>`_ and Databricks has `Metric Views <https://docs.databricks.com/aws/en/uc-semantics/metric-views/>`_. Both expose the semantic layer as a SQL object: a view-like interface over metrics and dimensions that you can query in any combination. This extension brings the same idea to DuckDB, using DDL syntax modeled closely on Snowflake's ``CREATE SEMANTIC VIEW``. You query a view the ways Snowflake does: with the same ``SEMANTIC_VIEW(...)`` construct, or by name with an ordinary ``SELECT region, AGG(revenue) FROM ... GROUP BY region`` (see :ref:`howto-standard-sql`).

.. grid:: 1 2 3 3
   :gutter: 3

   .. grid-item-card:: Getting started
      :link: tutorial-getting-started
      :link-type: ref

      Install the extension, create your first semantic view, and run a query in 5 minutes.

   .. grid-item-card:: Multi-table semantic views
      :link: tutorial-multi-table
      :link-type: ref

      Model relationships between tables and query across them.

   .. grid-item-card:: Building a complete model
      :link: tutorial-building-model
      :link-type: ref

      Facts, derived metrics, and iterative model refinement in 15 minutes.

.. grid:: 1 2 3 3
   :gutter: 3

   .. grid-item-card:: DDL reference
      :link: ref-create-semantic-view
      :link-type: ref

      Full syntax for ``CREATE SEMANTIC VIEW`` and related DDL.

   .. grid-item-card:: How-to guides
      :link: how-to-guides
      :link-type: ref

      Modeling, advanced metrics, data sources, materializations, YAML definitions, and more.

   .. grid-item-card:: Snowflake comparison
      :link: explanation-snowflake
      :link-type: ref

      Feature-by-feature comparison with Snowflake's ``CREATE SEMANTIC VIEW``.

If you are building a tool or an agent on top of the extension, every function it registers describes itself in DuckDB's ``duckdb_functions()`` listing, with its parameter names and an example call. :ref:`ref-functions` lists them and shows the query to run.

.. toctree::
   :hidden:

   tutorials/index
   how-to/index
   explanation/index
   reference/index
   changelog
