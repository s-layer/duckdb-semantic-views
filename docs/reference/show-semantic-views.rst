.. meta::
   :description: Syntax reference for SHOW SEMANTIC VIEWS, including TERSE variant, IN SCHEMA/IN DATABASE qualifiers, and optional LIKE, STARTS WITH, and LIMIT ... FROM filtering

.. _ref-show-semantic-views:

====================
SHOW SEMANTIC VIEWS
====================

Lists all registered semantic views, with optional filtering by name pattern, prefix, schema, database, or row count.


.. _ref-show-syntax:

Syntax
======

.. code-block:: sqlgrammar

   SHOW [ TERSE ] SEMANTIC VIEWS
       [ LIKE '<pattern>' ]
       [ IN SCHEMA [<database_name>.]<schema_name> | IN DATABASE <database_name> ]
       [ STARTS WITH '<prefix>' ]
       [ LIMIT <rows> [ FROM '<name_string>' ] ]

All clauses are optional. When multiple clauses appear, they must follow the order shown above.


.. _ref-show-variants:

Statement Variants
==================

``SHOW SEMANTIC VIEWS``
   Returns all registered semantic views with 6 columns.

``SHOW TERSE SEMANTIC VIEWS``
   Returns a compact listing with 5 columns (no ``comment`` column).

.. note::

   ``SHOW SEMANTIC VIEWS`` runs in your transaction, so a view created, dropped or renamed earlier in it is listed that way before you commit. See :ref:`explanation-txn-ddl-write-visibility`.


.. _ref-show-filtering:

Optional Filtering Clauses
==========================

``LIKE '<pattern>'``
   Filters views to those whose name matches the pattern. Uses SQL ``LIKE`` pattern syntax: ``%`` matches any sequence of characters, ``_`` matches a single character. Matching is **case-insensitive** (the extension maps ``LIKE`` to DuckDB's ``ILIKE``). The pattern must be enclosed in single quotes.

``IN SCHEMA [<database_name>.]<schema_name>``
   Filters views to those in the specified schema. The schema may be qualified with a database -- ``IN SCHEMA mydb.analytics`` -- in which case both halves must match, so a same-named schema in another database is not included. Unqualified, the schema is matched in any database.

``IN DATABASE <database_name>``
   Filters views to those in the specified database. Unlike the schema, a database name takes no qualifier; ``IN DATABASE a.b`` is an error rather than a name.

   Both names may be written double-quoted, which is what lets a name containing whitespace be given at all: ``IN SCHEMA "my schema"``. Matching follows DuckDB's identifier rule, the same rule view, dimension and metric names use: quotes are stripped and case is ignored, so ``"main"``, ``main`` and ``MAIN`` all select the same schema.

   Case-insensitivity here is not a convenience -- it is required for the filter to be answerable at all. The schema and database recorded against a semantic view come from ``current_schema()`` / ``current_database()`` at ``CREATE`` time, and DuckDB returns those as the spelling the caller last wrote in ``USE``, not the catalog's own. Two views created in one schema under ``USE MySchema`` and ``USE myschema`` are therefore recorded differently, and an exact-match filter could return at most one of them.

``STARTS WITH '<prefix>'``
   Filters views to those whose name begins with the prefix. Matching is **case-sensitive**. The prefix must be enclosed in single quotes.

``LIMIT <rows>``
   Restricts the output to the first *rows* results. Must be a non-negative integer; ``LIMIT 0`` is accepted and returns no rows.

``LIMIT <rows> FROM '<name_string>'``
   Pages through the list, as in Snowflake. Only views whose name sorts after ``'<name_string>'`` are kept, and ``LIMIT`` then applies to them. Pass the last name of one page to get the next. A view named exactly ``'<name_string>'`` is not returned. The string need not be a full view name: ``FROM 'sales_b'`` returns the views after ``sales_b`` whether or not one has that name. Like ``STARTS WITH``, the comparison is **case-sensitive**, and the string must be enclosed in single quotes. ``FROM`` needs ``LIMIT``, and only ``SHOW [TERSE] SEMANTIC VIEWS`` takes it; on the other ``SHOW SEMANTIC`` commands it is an error.

   The comparison uses the view name alone. When views in several schemas share a list, add ``IN SCHEMA`` so that the cursor follows the order of the rows.

   .. versionadded:: 0.18.0

When ``LIKE`` and ``STARTS WITH`` are both present, a view must satisfy both conditions (they are combined with ``AND``).

.. warning::

   Clause order is enforced. ``LIKE`` must come before ``IN SCHEMA``/``IN DATABASE``, and ``STARTS WITH`` must come before ``LIMIT``. Placing clauses out of order produces a syntax error.


.. _ref-show-output:

Output Columns
==============

**SHOW SEMANTIC VIEWS** returns one row per registered semantic view with 6 columns:

.. list-table::
   :header-rows: 1
   :widths: 20 15 65

   * - Column
     - Type
     - Description
   * - ``created_on``
     - VARCHAR
     - Time the semantic view was created, as an ISO 8601 UTC string (e.g., ``2026-09-30T22:39:55Z``).
   * - ``name``
     - VARCHAR
     - The semantic view name.
   * - ``kind``
     - VARCHAR
     - Always ``SEMANTIC_VIEW``.
   * - ``database_name``
     - VARCHAR
     - The DuckDB database containing the view (e.g., ``memory``).
   * - ``schema_name``
     - VARCHAR
     - The DuckDB schema containing the view (e.g., ``main``).
   * - ``comment``
     - VARCHAR
     - The view-level comment. Empty string if no comment is set.

**SHOW TERSE SEMANTIC VIEWS** returns 5 columns (same as above, without ``comment``):

.. list-table::
   :header-rows: 1
   :widths: 20 15 65

   * - Column
     - Type
     - Description
   * - ``created_on``
     - VARCHAR
     - Time the semantic view was created, as an ISO 8601 UTC string.
   * - ``name``
     - VARCHAR
     - The semantic view name.
   * - ``kind``
     - VARCHAR
     - Always ``SEMANTIC_VIEW``.
   * - ``database_name``
     - VARCHAR
     - The DuckDB database containing the view.
   * - ``schema_name``
     - VARCHAR
     - The DuckDB schema containing the view.


.. _ref-show-examples:

Examples
========

**List all semantic views:**

.. code-block:: sql

   SHOW SEMANTIC VIEWS;

.. code-block:: text

   ┌──────────────────────┬─────────────────┬───────────────┬───────────────┬─────────────┬───────────────────┐
   │ created_on           │ name            │ kind          │ database_name │ schema_name │ comment           │
   ├──────────────────────┼─────────────────┼───────────────┼───────────────┼─────────────┼───────────────────┤
   │ 2026-09-30T23:02:27Z │ order_metrics   │ SEMANTIC_VIEW │ memory        │ main        │ Revenue analytics │
   │ 2026-09-30T23:02:27Z │ sales_analytics │ SEMANTIC_VIEW │ memory        │ main        │                   │
   └──────────────────────┴─────────────────┴───────────────┴───────────────┴─────────────┴───────────────────┘

If no semantic views are registered, the result set is empty.

.. _ref-show-subquery:

**Selecting and filtering columns:**

.. versionadded:: 0.16.0

``SHOW SEMANTIC VIEWS`` can be used as a subquery or in a ``WITH`` clause, like DuckDB's own ``SHOW`` (see :ref:`ref-functions-subqueries`), so you can pick the columns you want and filter the rows:

.. code-block:: sql

   SELECT name, schema_name
   FROM (SHOW SEMANTIC VIEWS)
   WHERE comment <> '';

.. code-block:: text

   ┌───────────────┬─────────────┐
   │ name          │ schema_name │
   ├───────────────┼─────────────┤
   │ order_metrics │ main        │
   └───────────────┴─────────────┘

To dump the DDL of every view, see the :ref:`GET_DDL <ref-get-ddl-examples>` recipe.

**TERSE variant (no comment column):**

.. code-block:: sql

   SHOW TERSE SEMANTIC VIEWS;

.. code-block:: text

   ┌──────────────────────┬─────────────────┬───────────────┬───────────────┬─────────────┐
   │ created_on           │ name            │ kind          │ database_name │ schema_name │
   ├──────────────────────┼─────────────────┼───────────────┼───────────────┼─────────────┤
   │ 2026-09-30T23:02:27Z │ order_metrics   │ SEMANTIC_VIEW │ memory        │ main        │
   │ 2026-09-30T23:02:27Z │ sales_analytics │ SEMANTIC_VIEW │ memory        │ main        │
   └──────────────────────┴─────────────────┴───────────────┴───────────────┴─────────────┘

**Filter by schema:**

.. code-block:: sql

   SHOW SEMANTIC VIEWS IN SCHEMA main;

**Filter by database:**

.. code-block:: sql

   SHOW SEMANTIC VIEWS IN DATABASE memory;

**Filter by pattern with LIKE (case-insensitive):**

Find all views whose name contains "order":

.. code-block:: sql

   SHOW SEMANTIC VIEWS LIKE '%order%';

Because ``LIKE`` is case-insensitive, ``LIKE '%ORDER%'`` produces the same results.

**Filter by prefix with STARTS WITH (case-sensitive):**

Find views whose name starts with "sales":

.. code-block:: sql

   SHOW SEMANTIC VIEWS STARTS WITH 'sales';

``STARTS WITH`` is case-sensitive. ``STARTS WITH 'Sales'`` would return no results because the view is named ``sales_analytics`` (lowercase).

**Limit the number of results:**

.. code-block:: sql

   SHOW SEMANTIC VIEWS LIMIT 1;

**Page through the list with LIMIT ... FROM:**

With views ``inventory``, ``sales_b``, ``sales_c`` and ``sales_d``, ask for two views at a time. The first page has no ``FROM``:

.. code-block:: sql

   SHOW SEMANTIC VIEWS LIMIT 2;

It returns ``inventory`` and ``sales_b``. Pass the last name to get the next page:

.. code-block:: sql

   SHOW SEMANTIC VIEWS LIMIT 2 FROM 'sales_b';

.. code-block:: text

   ┌──────────────────────┬─────────┬───────────────┬───────────────┬─────────────┬─────────┐
   │ created_on           │ name    │ kind          │ database_name │ schema_name │ comment │
   ├──────────────────────┼─────────┼───────────────┼───────────────┼─────────────┼─────────┤
   │ 2026-10-28T13:05:35Z │ sales_c │ SEMANTIC_VIEW │ memory        │ main        │         │
   │ 2026-10-28T13:05:35Z │ sales_d │ SEMANTIC_VIEW │ memory        │ main        │         │
   └──────────────────────┴─────────┴───────────────┴───────────────┴─────────────┴─────────┘

**Combine multiple clauses:**

All optional clauses can be combined, following the required order:

.. code-block:: sql

   SHOW SEMANTIC VIEWS LIKE '%a%' IN SCHEMA main STARTS WITH 'sales' LIMIT 10;

**TERSE with filtering:**

.. code-block:: sql

   SHOW TERSE SEMANTIC VIEWS LIKE '%order%';

   SHOW TERSE SEMANTIC VIEWS IN SCHEMA main;

**The statement is case-insensitive:**

.. code-block:: sql

   show semantic views like '%order%';
