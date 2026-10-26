.. meta::
   :description: Query a semantic view by name with an ordinary SELECT ... GROUP BY, passing each metric to AGG, as in Snowflake

.. _howto-standard-sql:

==============================================
How to Query a Semantic View with Standard SQL
==============================================

.. versionadded:: 0.17.0

This guide shows how to query a semantic view by name with an ordinary ``SELECT``, as you would a table, the way Snowflake allows. You list the dimensions you want in ``GROUP BY`` and pass each metric to ``AGG``:

.. code-block:: sql

   SELECT region, AGG(revenue)
   FROM analytics
   WHERE channel = 'web'
   GROUP BY region;

The extension turns the query into the :ref:`SEMANTIC_VIEW(...) construct <ref-sv-construct>` before DuckDB plans it, so the results, the checks (fan traps, role-playing paths, private members) and the errors are the construct's. The query above is the same as:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       DIMENSIONS region
       METRICS revenue
       WHERE channel = 'web'
   );

**Prerequisites:**

- A semantic view (see :ref:`tutorial-getting-started`)


.. _howto-standard-sql-mapping:

How the Clauses Map
===================

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - In the query
     - Becomes
   * - ``GROUP BY`` expressions
     - ``DIMENSIONS`` items. ``GROUP BY 1`` (a position), ``GROUP BY r`` (a ``SELECT`` alias) and ``GROUP BY ALL`` are resolved to the ``SELECT`` item first.
   * - ``AGG(m)``
     - The metric ``m`` in ``METRICS``. ``MIN(m)``, ``MAX(m)`` and ``ANY_VALUE(m)`` of a metric mean the same as ``AGG(m)``.
   * - Any other aggregate, such as ``SUM(amount)`` or ``COUNT(DISTINCT customer)``
     - An ad-hoc metric in ``METRICS``, over the dimensions and facts it names.
   * - A ``SELECT`` item that is neither grouped nor aggregated
     - A ``FACTS`` item: the query returns one row per row of the fact's table.
   * - ``WHERE``
     - The construct's ``WHERE``: it filters rows before the metrics are computed, and may refer only to dimensions and facts.
   * - ``HAVING``
     - A filter on the result, after the metrics are computed. It may refer only to metrics passed to ``AGG`` or another aggregate.
   * - ``ORDER BY``, ``LIMIT``, ``DISTINCT``
     - Applied to the result, as for any query.

Each column is named as DuckDB would name the ``SELECT`` item: ``region``, ``agg(revenue)``, or the alias you give it.


.. _howto-standard-sql-expressions:

Compute Over Metrics and Dimensions
===================================

Expressions around ``AGG`` are computed on the result, and expressions over grouped dimensions are dimension expressions:

.. code-block:: sql

   SELECT year(ordered_at) AS y,
          AGG(revenue) / AGG(order_count) AS avg_order
   FROM analytics
   GROUP BY year(ordered_at)
   HAVING AGG(order_count) > 10
   ORDER BY y;


.. _howto-standard-sql-names:

Refer to Members
================

Members are named as in the construct. A name used by members of two tables is qualified by the logical table's alias (``c.region``). A qualifier that names the view itself, by its alias in ``FROM`` or by its name, is dropped:

.. code-block:: sql

   SELECT s.region, AGG(s.revenue) FROM analytics s GROUP BY s.region;


.. _howto-standard-sql-nesting:

Use It Anywhere a Query Goes
============================

The query can stand wherever a ``SELECT`` can: in a subquery or CTE, joined (as a subquery) with tables, in ``UNION``, in ``CREATE TABLE ... AS``, ``INSERT ... SELECT``, ``COPY (...) TO``, ``EXPLAIN``, ``DESCRIBE``, a prepared statement, or a ``CREATE VIEW`` (which stores the construct form). From Python, ``duckdb.sql()``, ``con.sql()`` and ``con.execute()`` all accept it, and a relation over it can be read by its variable name in a later query.

.. code-block:: sql

   SELECT c.name, r.rev
   FROM (SELECT region, AGG(revenue) AS rev FROM analytics GROUP BY region) r
   JOIN customers c USING (region);


.. _howto-standard-sql-refused:

What Is Refused
===============

These follow Snowflake's rules for the query form, and each is refused with an error naming the view:

- A metric not passed to ``AGG`` (``SELECT region, revenue ...``).
- A dimension in ``SELECT`` or ``ORDER BY`` that is not in ``GROUP BY``.
- ``HAVING`` over anything but aggregated metrics; filter dimensions in ``WHERE``.
- In the query block that reads the view: ``SELECT *``, a join (or a ``FROM`` list) with other tables, ``PIVOT``, window functions, ``QUALIFY``, ``GROUPING SETS`` / ``ROLLUP`` / ``CUBE``, sampling, and a column alias list on the view. Query the view in a subquery or CTE and apply these to its result.
- A reference to a table of an enclosing query (a correlated subquery). Inside the construct the name would otherwise resolve to the view's own logical table.

A name that is a table, a view or a CTE in scope is not a semantic view here: those keep their usual meaning, and so do DuckDB's own errors for statements that read no semantic view.


See Also
========

- :ref:`ref-sv-construct` -- The ``SEMANTIC_VIEW(...)`` construct this form turns into
- :ref:`howto-filtering` -- ``WHERE`` before aggregation and filters after it
- :ref:`howto-query-facts` -- Row-level fact queries
- :ref:`explanation-snowflake` -- How querying compares with Snowflake
