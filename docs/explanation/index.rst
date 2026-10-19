.. meta::
   :description: Background concepts and design context explaining how semantic views work, metric grain, and where they differ from standard SQL views, Snowflake, and Databricks

.. _explanation:

===========
Explanation
===========

Background concepts and design context for DuckDB Semantic Views.

- :ref:`explanation-sv-vs-views` -- How semantic views differ from standard SQL views and what the declarative layer adds.
- :ref:`explanation-metric-grain` -- Why each metric aggregates at the grain of its own table, how multi-grain queries are assembled, and which shapes are still refused.
- :ref:`explanation-snowflake` -- Feature-by-feature comparison with Snowflake's SQL DDL semantic views and where this extension diverges.
- :ref:`explanation-databricks` -- Feature-by-feature comparison with Databricks metric views and where this extension diverges.
- :ref:`explanation-transactional-ddl` -- How transactional DDL works since 0.8.0 and the small set of caveats around read visibility, the parser override setting, and concurrent writers.

.. toctree::
   :hidden:

   semantic-views-vs-regular-views
   metric-grain
   snowflake-comparison
   databricks-comparison
   transactional-ddl-and-limitations
