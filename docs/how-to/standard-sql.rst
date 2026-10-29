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

Use this form when you, or a tool that writes SQL, would rather write a familiar ``GROUP BY`` query than the construct. Both return the same rows.

**Prerequisites:**

- The extension loaded (see :ref:`tutorial-getting-started`)
- A semantic view to query; the examples use the one defined below


.. _howto-standard-sql-setup:

Set Up an Example View
======================

Every example below runs against this view of orders and the customers who placed them. ``amount`` is declared as a fact, so it can be aggregated ad hoc or returned row by row.

.. code-block:: sql

   CREATE TABLE customers (id INTEGER, name VARCHAR, region VARCHAR);
   INSERT INTO customers VALUES
       (1, 'Acme',    'East'),
       (2, 'Globex',  'East'),
       (3, 'Initech', 'West');

   CREATE TABLE orders (
       id INTEGER, customer_id INTEGER, amount DECIMAL(10,2),
       channel VARCHAR, ordered_at DATE
   );
   INSERT INTO orders VALUES
       (1, 1, 100.00, 'web',    DATE '2023-12-15'),
       (2, 1, 250.00, 'web',    DATE '2024-01-10'),
       (3, 2,  80.00, 'retail', DATE '2024-02-03'),
       (4, 3, 300.00, 'retail', DATE '2024-03-21'),
       (5, 3, 150.00, 'web',    DATE '2024-04-02');

   CREATE SEMANTIC VIEW analytics AS
   TABLES (
       o AS orders    PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   FACTS (
       o.amount AS o.amount
   )
   DIMENSIONS (
       c.customer   AS c.name,
       c.region     AS c.region,
       o.channel    AS o.channel,
       o.ordered_at AS o.ordered_at
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   );

The query from the top of the page returns web revenue by region:

.. code-block:: sql

   SELECT region, AGG(revenue)
   FROM analytics
   WHERE channel = 'web'
   GROUP BY region
   ORDER BY region;

.. code-block:: text

   ┌────────┬──────────────┐
   │ region │ agg(revenue) │
   ├────────┼──────────────┤
   │ East   │       350.00 │
   │ West   │       150.00 │
   └────────┴──────────────┘


.. _howto-standard-sql-mapping:

How the Clauses Map
===================

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - In the query
     - Becomes
   * - ``GROUP BY`` expressions
     - ``DIMENSIONS`` items. ``GROUP BY 1`` (a position), ``GROUP BY r`` (a ``SELECT`` alias) and ``GROUP BY ALL`` are resolved to the ``SELECT`` item first. A grouped fact, like any expression over facts, gives one row per value.
   * - ``AGG(m)``
     - The metric ``m`` in ``METRICS``. ``MIN(m)``, ``MAX(m)`` and ``ANY_VALUE(m)`` of a metric mean the same as ``AGG(m)``.
   * - Any other aggregate, such as ``SUM(amount)`` or ``COUNT(DISTINCT customer)``
     - An ad-hoc metric in ``METRICS``, over the dimensions and facts it names. An aggregate must name at least one: ``COUNT(*)`` alone is refused, because nothing says which table's rows it counts. Declare a metric ``AS COUNT(*)`` on that table and pass it to ``AGG``.
   * - A ``SELECT`` item that is neither grouped nor aggregated
     - A ``FACTS`` item, in a query with no ``GROUP BY``: the query returns one row per row of the fact's table. A grouped query refuses it, as any SQL does.
   * - ``WHERE``
     - The construct's ``WHERE``: it filters rows before the metrics are computed, and may refer only to dimensions and facts.
   * - ``HAVING``
     - A filter on the result, after the metrics are computed. It may refer only to metrics passed to ``AGG`` or another aggregate.
   * - ``ORDER BY``, ``LIMIT``, ``DISTINCT``
     - Applied to the result, as for any query.

Each column is named as DuckDB would name the ``SELECT`` item: ``region``, ``agg(revenue)``, or the alias you give it. Give each ``AGG`` an alias when the column name matters to the code that reads it:

.. code-block:: sql

   SELECT region, AGG(revenue) AS revenue, AGG(order_count) AS orders
   FROM analytics
   GROUP BY ALL
   ORDER BY region;

.. code-block:: text

   ┌────────┬─────────┬────────┐
   │ region │ revenue │ orders │
   ├────────┼─────────┼────────┤
   │ East   │  430.00 │      3 │
   │ West   │  450.00 │      2 │
   └────────┴─────────┴────────┘


.. _howto-standard-sql-expressions:

Compute Over Metrics and Dimensions
===================================

Expressions around ``AGG`` are computed on the result, and expressions over grouped dimensions are dimension expressions. ``HAVING`` keeps the years with more than one order:

.. code-block:: sql

   SELECT year(ordered_at) AS y,
          AGG(revenue) / AGG(order_count) AS avg_order
   FROM analytics
   GROUP BY year(ordered_at)
   HAVING AGG(order_count) > 1
   ORDER BY y;

.. code-block:: text

   ┌──────┬───────────┐
   │  y   │ avg_order │
   ├──────┼───────────┤
   │ 2024 │     195.0 │
   └──────┴───────────┘

2023 had one order, so ``HAVING`` drops it after the metrics are computed.


.. _howto-standard-sql-adhoc:

Aggregate Ad Hoc and Return Fact Rows
=====================================

An aggregate other than ``AGG`` is computed over the dimensions and facts it names, like an :ref:`expression item <ref-sv-expression-items>` in ``METRICS``. Use it for a calculation the view does not declare:

.. code-block:: sql

   SELECT region,
          AGG(revenue)             AS revenue,
          MAX(amount)              AS biggest_order,
          COUNT(DISTINCT customer) AS customers
   FROM analytics
   GROUP BY region
   ORDER BY region;

.. code-block:: text

   ┌────────┬─────────┬───────────────┬───────────┐
   │ region │ revenue │ biggest_order │ customers │
   ├────────┼─────────┼───────────────┼───────────┤
   │ East   │  430.00 │        250.00 │         2 │
   │ West   │  450.00 │        300.00 │         1 │
   └────────┴─────────┴───────────────┴───────────┘

A fact in a query with no ``GROUP BY`` returns row-level values, one row per order here, as ``FACTS amount`` would in the construct. In a grouped query, a fact must be grouped on or aggregated, as any column must in SQL. Grouped on, it gives one row per distinct value, which here is one row per order:

.. code-block:: sql

   SELECT customer, amount
   FROM analytics
   GROUP BY customer, amount
   ORDER BY customer, amount;

.. code-block:: text

   ┌──────────┬────────┐
   │ customer │ amount │
   ├──────────┼────────┤
   │ Acme     │ 100.00 │
   │ Acme     │ 250.00 │
   │ Globex   │  80.00 │
   │ Initech  │ 150.00 │
   │ Initech  │ 300.00 │
   └──────────┴────────┘


.. _howto-standard-sql-names:

Refer to Members
================

Members are named as in the construct. A name used by members of two tables is qualified by the logical table's alias (``c.region``). A qualifier that names the view itself, by its alias in ``FROM`` or by its name, is dropped. The view itself can be named with its schema, and database, as a table can (``FROM main.analytics``):

.. code-block:: sql

   SELECT s.region, AGG(s.revenue) AS revenue
   FROM analytics s
   GROUP BY s.region
   ORDER BY s.region;

.. code-block:: text

   ┌────────┬─────────┐
   │ region │ revenue │
   ├────────┼─────────┤
   │ East   │  430.00 │
   │ West   │  450.00 │
   └────────┴─────────┘


.. _howto-standard-sql-nesting:

Use It Anywhere a Query Goes
============================

The query can stand wherever a ``SELECT`` can: in a subquery or CTE, joined (as a subquery) with tables, in ``UNION``, in ``CREATE TABLE ... AS``, ``INSERT ... SELECT``, a subquery of ``UPDATE``, ``DELETE`` or ``SET VARIABLE``, ``COPY (...) TO``, ``EXPLAIN``, ``DESCRIBE``, a prepared statement, or a ``CREATE VIEW`` (which stores the construct form). A subquery in any clause counts, ``ORDER BY`` and ``LIMIT`` included.

The query block that reads the view cannot join other tables, so join its result instead. Here each region's revenue is compared with a target held in an ordinary table:

.. code-block:: sql

   CREATE TABLE region_targets (region VARCHAR, target DECIMAL(10,2));
   INSERT INTO region_targets VALUES ('East', 400.00), ('West', 500.00);

   SELECT r.region, r.revenue, t.target, r.revenue >= t.target AS met
   FROM (
       SELECT region, AGG(revenue) AS revenue
       FROM analytics
       GROUP BY region
   ) r
   JOIN region_targets t USING (region)
   ORDER BY r.region;

.. code-block:: text

   ┌────────┬─────────┬────────┬───────┐
   │ region │ revenue │ target │  met  │
   ├────────┼─────────┼────────┼───────┤
   │ East   │  430.00 │ 400.00 │ true  │
   │ West   │  450.00 │ 500.00 │ false │
   └────────┴─────────┴────────┴───────┘

Window functions work the same way: compute the metrics in a CTE, then rank the result:

.. code-block:: sql

   WITH r AS (
       SELECT region, AGG(revenue) AS revenue
       FROM analytics
       GROUP BY region
   )
   SELECT region, revenue, rank() OVER (ORDER BY revenue DESC) AS rnk
   FROM r
   ORDER BY rnk;

.. code-block:: text

   ┌────────┬─────────┬─────┐
   │ region │ revenue │ rnk │
   ├────────┼─────────┼─────┤
   │ West   │  450.00 │   1 │
   │ East   │  430.00 │   2 │
   └────────┴─────────┴─────┘


.. _howto-standard-sql-app:

Pass Values from Application Code
=================================

A prepared-statement parameter (``?``, ``$1``, ``$name``) in ``WHERE`` is a parameter of your statement, as in the construct's ``WHERE``, so a value from a request is bound as a value and never pasted into the SQL:

.. code-block:: sql

   PREPARE web_revenue AS
       SELECT region, AGG(revenue) AS revenue
       FROM analytics
       WHERE channel = ?
       GROUP BY region
       ORDER BY region;

   EXECUTE web_revenue('web');

.. code-block:: text

   ┌────────┬─────────┐
   │ region │ revenue │
   ├────────┼─────────┤
   │ East   │  350.00 │
   │ West   │  150.00 │
   └────────┴─────────┘

From Python, ``duckdb.sql()``, ``con.sql()`` and ``con.execute()`` all accept the query, with parameters bound the usual way:

.. code-block:: python

   import datetime

   import duckdb

   con = duckdb.connect("analytics.duckdb")  # the database holding the view
   con.execute("LOAD semantic_views")


   def revenue_by_region(channel, since):
       return con.execute(
           """
           SELECT region, AGG(revenue) AS revenue
           FROM analytics
           WHERE channel = ? AND ordered_at >= ?
           GROUP BY region
           ORDER BY region
           """,
           [channel, since],
       ).fetchall()


   revenue_by_region("web", datetime.date(2024, 1, 1))
   # [('East', Decimal('250.00')), ('West', Decimal('150.00'))]

A relation built over the query can be read by its variable name in a later query, as DuckDB reads any relation. A Python DataFrame or relation that has the view's own name is read as itself, not as the view. :ref:`howto-filtering-app` covers the safety rules for request values in more depth.


.. _howto-standard-sql-verify:

Verify the Generated SQL
========================

``EXPLAIN`` on a standard-SQL query shows DuckDB's plan, not the SQL the extension writes. To see that SQL, pass the same request to :ref:`explain_semantic_view() <ref-explain-semantic-view>`, using the mapping above: the ``GROUP BY`` items as ``dimensions``, the metrics in ``AGG`` as ``metrics``, and the ``WHERE`` as ``where_clause``. For the query at the top of the page:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('analytics',
       dimensions := ['region'],
       metrics := ['revenue'],
       where_clause := 'channel = ''web'''
   );

The output shows the expanded SQL, then DuckDB's plan for it. The SQL has a ``SUM`` over ``orders``, a ``LEFT JOIN`` to ``customers`` for ``region``, and ``channel = 'web'`` applied before the ``GROUP BY``. ``HAVING``, ``ORDER BY`` and ``LIMIT`` run on that result, so they do not appear in it.


.. _howto-standard-sql-refused:

What Is Refused
===============

These follow Snowflake's rules for the query form, and each is refused with an error naming the view (the messages are listed under :ref:`ref-err-standard-sql`):

- A metric not passed to ``AGG`` (``SELECT region, revenue ...``).
- A dimension in ``SELECT`` or ``ORDER BY`` that is not in ``GROUP BY``.
- ``HAVING`` over anything but aggregated metrics; filter dimensions in ``WHERE``.
- In the query block that reads the view: ``SELECT *``, a join (or a ``FROM`` list) with other tables, ``PIVOT`` (also the ``PIVOT v ON ...`` statement), window functions, ``QUALIFY``, ``GROUPING SETS`` / ``ROLLUP`` / ``CUBE``, sampling, and a column alias list on the view. Query the view in a subquery or CTE and apply these to its result, as in :ref:`howto-standard-sql-nesting`.
- A reference to a table of an enclosing query (a correlated subquery), written qualified, as in ``WHERE region = c.region``. Inside the construct the name would otherwise resolve to the view's own logical table.

  A bare name in ``WHERE`` that is no member of the view is passed to DuckDB as written, as a raw column in the construct's ``WHERE`` is. When none of the view's tables has a column of that name, DuckDB binds it to the enclosing query, so ``WHERE region = nm``, with ``nm`` a column of the outer query, is a working correlated subquery. A bare name that is a member always means the member, and a name that is a column of one of the view's tables binds to that column, so the outer column is reached only when no table of the view has one by that name.

A name that is a table, a view, a CTE in scope or a Python DataFrame or relation is not a semantic view here: those keep their usual meaning, and so do DuckDB's own errors for statements that read no semantic view.

This form has no way to set the view's :ref:`variables <ref-create-variables>`, so each takes its default. A view with a variable that has no default cannot be queried this way: the query fails with ``variable '<name>' has no default``. Use :ref:`SEMANTIC_VIEW(...) <ref-sv-variables>` with a ``VARIABLES`` clause for such a view.


.. _howto-standard-sql-troubleshooting:

Troubleshooting
===============

**metric 'revenue' must be passed to AGG, e.g. AGG(revenue)**
   A metric is already aggregated, so it is requested with ``AGG(revenue)``, not by its bare name. This also applies in ``HAVING``: a ``SELECT`` alias that repeats a metric's name still names the metric there, so write ``HAVING AGG(revenue) > 400`` rather than ``HAVING revenue > 400``.

**dimension 'customer' must appear in GROUP BY**
   Every dimension you select is a grouping key. Add it to ``GROUP BY``, or write ``GROUP BY ALL``.

**'amount' must be passed to an aggregate or appear in GROUP BY**
   In a grouped query, every ``SELECT`` item is grouped on or aggregated. Either the item is a fact you meant to aggregate (write ``MAX(amount)``) or group by, or its name is misspelled: ``SELECT region, regoin, AGG(revenue) ... GROUP BY region`` gives this error too. Check the name against :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>`. To read a dimension row by row next to a fact, use the construct's ``FACTS`` (see :ref:`howto-query-facts-dims`).

**cannot combine facts and metrics in the same query**
   In a query with no ``GROUP BY``, a ``SELECT`` item that is neither grouped nor aggregated is read as a fact, and a query returns either row-level facts or aggregated metrics, not both. Aggregate the fact, or check its name as above.

**SELECT * is not supported in a query over a semantic view**
   The query form needs the dimensions and metrics named, as in Snowflake. Name them, or use ``SELECT * FROM SEMANTIC_VIEW(...)``, where ``*`` means the items you listed.

**semantic view 'analytics' cannot be joined, pivoted or read alongside other tables**
   Query the view on its own in a subquery or CTE and join that result, as in :ref:`howto-standard-sql-nesting`.

**Catalog Error: Table with name ... does not exist**
   The name in ``FROM`` is not a semantic view, table or view. Check the spelling with :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>`. If a CTE or table has the same name as the semantic view, the query reads that instead; rename one of them.


.. _howto-standard-sql-related:

Related
=======

- :ref:`ref-sv-construct` -- The ``SEMANTIC_VIEW(...)`` construct this form turns into
- :ref:`ref-err-standard-sql` -- Every error the query form can raise
- :ref:`howto-filtering` -- ``WHERE`` before aggregation and filters after it
- :ref:`howto-query-facts` -- Row-level fact queries
- :ref:`explanation-snowflake` -- How querying compares with Snowflake
