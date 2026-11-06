.. meta::
   :description: Declare view variables for thresholds, rates and reporting dates, set them per query in SEMANTIC_VIEW(...) or semantic_view(), and bind them from application code

.. _howto-variables:

=========================================
How to Parameterize a View with Variables
=========================================

.. versionadded:: 0.18.0

This guide shows how to give a semantic view named values -- a threshold, a currency rate, a reporting date -- that each query can set, so one view answers questions that would otherwise need a view per value. You declare the variables in the view's ``VARIABLES`` clause, read them by bare name in facts, dimensions and metrics, and set them per query. The clause and its rules follow Snowflake's.

**Prerequisites:**

- The extension loaded (see :ref:`tutorial-getting-started`)
- Familiarity with ``CREATE SEMANTIC VIEW`` clauses (see :ref:`ref-create-semantic-view`)


.. _howto-variables-declare:

Declare Variables in the View
=============================

Put a ``VARIABLES`` clause between ``RELATIONSHIPS`` and ``FACTS``. Each entry is ``name [type] [DEFAULT <expression>] [COMMENT '<text>']``; ``:=`` can replace ``DEFAULT``. Members read a variable by its bare name, with no table alias.

This view of orders has a threshold that decides which orders count as large, and a rate that converts amounts from US dollars to the reporting currency:

.. code-block:: sql
   :emphasize-lines: 13-18,20,24

   CREATE TABLE orders (id INTEGER, region VARCHAR, amount DECIMAL(10,2), ordered_at DATE);
   INSERT INTO orders VALUES
       (1, 'East', 120.00, DATE '2024-03-02'),
       (2, 'East', 640.00, DATE '2024-03-15'),
       (3, 'West',  80.00, DATE '2024-03-20'),
       (4, 'West', 900.00, DATE '2024-04-05'),
       (5, 'East', 300.00, DATE '2024-04-18');

   CREATE SEMANTIC VIEW order_analysis
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   VARIABLES (
       large_order_min DECIMAL(10,2) DEFAULT 500
           COMMENT 'smallest amount that counts as a large order',
       fx_rate DOUBLE DEFAULT 1.0
           COMMENT 'multiplier from USD to the reporting currency'
   )
   FACTS (
       o.amount_reported AS o.amount * fx_rate
   )
   DIMENSIONS (
       o.region     AS o.region,
       o.order_size AS CASE WHEN o.amount >= large_order_min THEN 'large' ELSE 'small' END
   )
   METRICS (
       o.revenue     AS SUM(o.amount_reported),
       o.order_count AS COUNT(*)
   );

``order_size`` reads ``large_order_min``, and ``revenue`` reads ``fx_rate`` through the ``amount_reported`` fact. A query that sets neither gets the defaults: orders of 500 or more are large, and revenue is in US dollars.

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(order_analysis
       DIMENSIONS region, order_size
       METRICS revenue, order_count
   ) ORDER BY ALL;

.. code-block:: text

   ┌────────┬────────────┬─────────┬─────────────┐
   │ region │ order_size │ revenue │ order_count │
   ├────────┼────────────┼─────────┼─────────────┤
   │ East   │ large      │   640.0 │           1 │
   │ East   │ small      │   420.0 │           2 │
   │ West   │ large      │   900.0 │           1 │
   │ West   │ small      │    80.0 │           1 │
   └────────┴────────────┴─────────┴─────────────┘

Give each variable a type. Every value it takes, from a query or from its default, is cast to that type, so ``large_order_min = 250`` and ``large_order_min = '250'`` mean the same thing. Without a type, a value keeps whatever type the query gives it.

.. warning::

   **An unqualified name is the variable, a qualified one the column.** A variable named like a column changes what an unqualified reference to that column means in every member of the view. With a variable called ``amount``, ``SUM(amount)`` sums the variable. Qualify the column (``o.amount``) wherever you mean the column, as the view above does, or give variables names no column has.


.. _howto-variables-set:

Set Variables in a Query
========================

Add a ``VARIABLES`` clause at the end of the :ref:`SEMANTIC_VIEW(...) <ref-sv-construct>` construct, after any ``WHERE``. ``name = value`` and ``name => value`` mean the same. A variable you leave out keeps its default.

Report the same breakdown with large orders starting at 250, converted at 0.8 to the reporting currency:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(order_analysis
       DIMENSIONS region, order_size
       METRICS revenue, order_count
       VARIABLES large_order_min = 250, fx_rate = 0.8
   ) ORDER BY ALL;

.. code-block:: text

   ┌────────┬────────────┬─────────┬─────────────┐
   │ region │ order_size │ revenue │ order_count │
   ├────────┼────────────┼─────────┼─────────────┤
   │ East   │ large      │   752.0 │           2 │
   │ East   │ small      │    96.0 │           1 │
   │ West   │ large      │   720.0 │           1 │
   │ West   │ small      │    64.0 │           1 │
   └────────┴────────────┴─────────┴─────────────┘

East's 300 order is now large, and every amount is multiplied by 0.8.

With the :ref:`semantic_view() <ref-semantic-view-function>` table function, pass the values as a struct in ``variables``:

.. code-block:: duckdb-sql

   SELECT * FROM semantic_view('order_analysis',
       dimensions := ['region', 'order_size'],
       metrics := ['revenue', 'order_count'],
       variables := {'large_order_min': 250, 'fx_rate': 0.8}
   ) ORDER BY ALL;

The query's ``WHERE`` and its :ref:`expression items <ref-sv-expression-items>` read variables by bare name too, so ``METRICS revenue / fx_rate AS revenue_usd`` turns a converted total back into dollars.

.. note::

   View variables are not DuckDB's session variables (``SET VARIABLE``). A view variable has no value outside a query: each query sets it, or takes its default. To pass a session variable's value, read it in the query: ``SET VARIABLE fx = 0.8;`` then ``VARIABLES fx_rate = getvariable('fx')``.


.. _howto-variables-defaults:

Use a Default That Changes per Query
====================================

A default is an expression that each query evaluates, not a value fixed at ``CREATE``. Use that for a reporting date that should mean "today" unless the caller says otherwise. Add an ``as_of`` variable and a trailing 30-day revenue metric:

.. code-block:: sql
   :emphasize-lines: 10-11,23-24

   CREATE OR REPLACE SEMANTIC VIEW order_analysis
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   VARIABLES (
       large_order_min DECIMAL(10,2) DEFAULT 500
           COMMENT 'smallest amount that counts as a large order',
       fx_rate DOUBLE DEFAULT 1.0
           COMMENT 'multiplier from USD to the reporting currency',
       as_of DATE DEFAULT current_date
           COMMENT 'last day of the reporting window'
   )
   FACTS (
       o.amount_reported AS o.amount * fx_rate
   )
   DIMENSIONS (
       o.region     AS o.region,
       o.order_size AS CASE WHEN o.amount >= large_order_min THEN 'large' ELSE 'small' END
   )
   METRICS (
       o.revenue     AS SUM(o.amount_reported),
       o.order_count AS COUNT(*),
       o.revenue_30d AS SUM(o.amount_reported)
           FILTER (WHERE o.ordered_at > as_of - INTERVAL 30 DAY AND o.ordered_at <= as_of)
   );

A dashboard leaves ``as_of`` unset and gets the 30 days up to today. A back-dated report sets it:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(order_analysis
       DIMENSIONS region
       METRICS revenue_30d
       VARIABLES as_of = DATE '2024-04-20'
   ) ORDER BY ALL;

.. code-block:: text

   ┌────────┬─────────────┐
   │ region │ revenue_30d │
   ├────────┼─────────────┤
   │ East   │       300.0 │
   │ West   │       900.0 │
   └────────┴─────────────┘

Only the orders of 2024-04-05 and 2024-04-18 fall in the 30 days up to 2024-04-20.


.. _howto-variables-required:

Make a Variable Required
========================

Leave out the default when no value is a safe guess, such as an exchange rate that must come from the caller. Every query of the view must then set the variable, as in Snowflake, even a query that reads no member that uses it:

.. code-block:: sql

   CREATE SEMANTIC VIEW order_fx
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   VARIABLES (
       fx_rate DOUBLE COMMENT 'multiplier from USD to the reporting currency'
   )
   DIMENSIONS (
       o.region AS o.region
   )
   METRICS (
       o.revenue     AS SUM(o.amount * fx_rate),
       o.order_count AS COUNT(*)
   );

   SELECT * FROM SEMANTIC_VIEW(order_fx
       DIMENSIONS region
       METRICS order_count
   );

.. code-block:: text

   Binder Error: semantic_view: semantic view 'order_fx': variable 'fx_rate' has no default, so the query must set it: VARIABLES fx_rate = <value>

Set it and the query runs:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(order_fx
       DIMENSIONS region
       METRICS revenue, order_count
       VARIABLES fx_rate = 0.8
   ) ORDER BY ALL;

.. code-block:: text

   ┌────────┬─────────┬─────────────┐
   │ region │ revenue │ order_count │
   ├────────┼─────────┼─────────────┤
   │ East   │   848.0 │           3 │
   │ West   │   784.0 │           2 │
   └────────┴─────────┴─────────────┘

.. warning::

   A :ref:`standard-SQL query <howto-standard-sql>` over the view (``SELECT region, AGG(revenue) FROM order_fx GROUP BY region``) has no way to set variables, so every variable takes its default there. A view with a required variable cannot be queried that way at all. Give the variable a default if tools that write standard SQL need to read the view.


.. _howto-variables-app:

Pass Values from Application Code
=================================

A variable's value can be a prepared-statement parameter. Parameters are numbered across the whole statement, and ``VARIABLES`` comes after ``WHERE``, so its parameters follow the ones in ``WHERE``:

.. code-block:: sql

   PREPARE revenue_by_size AS
   SELECT * FROM SEMANTIC_VIEW(order_analysis
       DIMENSIONS order_size
       METRICS revenue
       WHERE region = ?
       VARIABLES large_order_min = ?, fx_rate = ?
   ) ORDER BY ALL;

   EXECUTE revenue_by_size('East', 250, 0.8);

.. code-block:: text

   ┌────────────┬─────────┐
   │ order_size │ revenue │
   ├────────────┼─────────┤
   │ large      │   752.0 │
   │ small      │    96.0 │
   └────────────┴─────────┘

From Python, bind the values the usual way. Each one reaches the query as a value, never as SQL text, so a value taken from a request cannot change the query:

.. code-block:: python

   import duckdb

   con = duckdb.connect("analytics.duckdb")  # the database holding the view
   con.execute("LOAD semantic_views")


   def revenue_by_size(region, large_order_min, fx_rate):
       return con.execute(
           """
           SELECT * FROM SEMANTIC_VIEW(order_analysis
               DIMENSIONS order_size
               METRICS revenue
               WHERE region = ?
               VARIABLES large_order_min = ?, fx_rate = ?
           ) ORDER BY ALL
           """,
           [region, large_order_min, fx_rate],
       ).fetchall()


   revenue_by_size("East", 250, 0.8)
   # [('large', 752.0), ('small', 96.0)]

The table function takes parameters the same way: ``variables := {'large_order_min': ?, 'fx_rate': ?}``.


.. _howto-variables-verify:

Verify the Generated SQL
========================

Pass the same ``variables`` to :ref:`explain_semantic_view() <ref-explain-semantic-view>` to see where each value lands:

.. code-block:: duckdb-sql

   SELECT * FROM explain_semantic_view('order_analysis',
       dimensions := ['order_size'],
       metrics := ['revenue'],
       variables := {'fx_rate': 0.8}
   );

In the expanded SQL, a value the query sets appears as a parameter cast to the variable's type (``CAST(($1) AS DOUBLE)`` for ``fx_rate``), and a variable left unset has its default written in (``CAST((500) AS DECIMAL(10,2))`` for ``large_order_min``):

.. code-block:: text

   -- Expanded SQL:
   SELECT
       CASE WHEN o.amount >= (CAST((500) AS DECIMAL(10,2))) THEN 'large' ELSE 'small' END AS "order_size",
       SUM((o.amount * (CAST(($1) AS DOUBLE)))) AS "revenue"
   FROM "memory"."main"."orders" AS "o"
   GROUP BY
       1

To list a view's variables with their types, defaults and comments, read the ``VARIABLE`` rows of :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`:

.. code-block:: sql

   SELECT object_name, property, property_value
   FROM (DESCRIBE SEMANTIC VIEW order_analysis)
   WHERE object_kind = 'VARIABLE';


.. _howto-variables-materializations:

Variables and Materializations
==============================

A :ref:`materialization <howto-materializations>` holds numbers computed with one set of values, and a query may ask for another. So a materialization that covers a member reading a variable, directly or through a fact or another metric, is never used to answer a query, whatever values the query sets. The query computes from the base tables instead. Materializations of the view that cover only members without variables still route.

Keep variables out of the members your materializations cover when routing matters. For ``order_analysis``, a materialization of ``order_count`` by ``region`` routes, and one of ``revenue`` by ``region`` does not, because ``revenue`` reads ``fx_rate``.


.. _howto-variables-troubleshooting:

Troubleshooting
===============

**variable 'fx_rate' has no default, so the query must set it**
   The view declares the variable without a default. Add it to the query's ``VARIABLES`` clause (or ``variables := {...}``), or recreate the view with a ``DEFAULT``. See :ref:`howto-variables-required`.

**semantic view 'order_analysis' has no variable 'fx_rat'. Did you mean 'fx_rate'?**
   The query sets a variable the view does not declare. Check the names in the ``VARIABLE`` rows of ``DESCRIBE SEMANTIC VIEW``.

**VARIABLES comes last in SEMANTIC_VIEW(...)**
   Move the ``VARIABLES`` clause after ``WHERE`` and the other clauses.

**variable 'region' has the name of a dimension**
   A variable cannot share its name with a fact, dimension or metric. Rename the variable.

**A member returns the variable's value instead of the column**
   A variable has the same name as a column, and the member refers to the column without its table alias. Qualify the column (``o.amount``). See the warning in :ref:`howto-variables-declare`.

**variable 'k' does not bind: Binder Error: Referenced column "amount" was not found**
   A default is evaluated on its own, before any table is read, so it cannot refer to a column. Give it a constant or an expression such as ``current_date``.

**metric 'w' uses variable 'n' in its window frame**
   A window frame bound such as ``ROWS BETWEEN n PRECEDING AND CURRENT ROW`` cannot read a variable. Write the bound as a number.

See :ref:`ref-err-variables` and :ref:`ref-err-query-variables` for every message.


.. _howto-variables-related:

Related
=======

- :ref:`ref-create-variables` -- ``VARIABLES`` clause syntax and validation rules
- :ref:`ref-sv-variables` -- Setting variables in ``SEMANTIC_VIEW(...)`` and ``semantic_view()``
- :ref:`howto-filtering` -- Filtering before and after aggregation, and binding request values safely
- :ref:`explanation-snowflake` -- How variables compare with Snowflake's
