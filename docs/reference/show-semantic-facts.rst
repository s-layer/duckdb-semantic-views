.. meta::
   :description: Syntax reference for SHOW SEMANTIC FACTS, which lists named row-level expressions across one or all semantic views with optional filtering

.. _ref-show-semantic-facts:

=====================
SHOW SEMANTIC FACTS
=====================

Lists facts (named row-level expressions) registered in one or all semantic views. Each row describes a single fact with its name, source table, data type, synonyms, comment, and labels. Views that have no facts defined return no rows.


.. _ref-show-facts-syntax:

Syntax
======

.. code-block:: sqlgrammar

   SHOW SEMANTIC FACTS
       [ LIKE '<pattern>' ]
       [ IN { <name> | ACCOUNT
            | DATABASE [ <database_name> ]
            | SCHEMA [ [<database_name>.]<schema_name> ] } ]
       [ STARTS WITH '<prefix>' ]
       [ LIMIT <rows> ]

All clauses are optional. When multiple clauses appear, they must follow the order shown above.


.. _ref-show-facts-variants:

Statement Variants
==================

``SHOW SEMANTIC FACTS``
   Returns facts across all registered semantic views, sorted by semantic view name and then fact name. Views with no ``FACTS`` clause are omitted from the result.

``SHOW SEMANTIC FACTS IN <name>``
   Returns facts for the specified semantic view only, sorted by fact name. Returns an error if the view does not exist. Returns an empty result if the view has no facts.


.. _ref-show-facts-params:

Parameters
==========

``<name>``
   The name of the semantic view. Required only for the single-view form (``IN`` clause). Returns an error if the view does not exist.


.. _ref-show-facts-filtering:

Optional Filtering Clauses
==========================

``LIKE '<pattern>'``
   Filters facts to those whose name matches the pattern. Uses SQL ``LIKE`` pattern syntax: ``%`` matches any sequence of characters, ``_`` matches a single character. Matching is **case-insensitive** (the extension maps ``LIKE`` to DuckDB's ``ILIKE``). The pattern must be enclosed in single quotes.

``IN ...``
   Scopes the listing. The alternatives are mutually exclusive -- ``IN`` appears at most once, so a view name and a schema cannot both be given.

   ``IN <name>``
      Returns facts for that semantic view only.

   ``IN ACCOUNT``
      Returns everything, the same as omitting ``IN`` entirely. Accepted for Snowflake compatibility; DuckDB has no account.

   ``IN DATABASE [ <database_name> ]``
      Filters facts to semantic views in that database, or the current database when the name is omitted.

   ``IN SCHEMA [ [<database_name>.]<schema_name> ]``
      Filters facts to semantic views in that schema, or the current schema when the name is omitted. Qualifying the schema with a database matches on both, so a same-named schema in another database is excluded.

   Schema and database names follow DuckDB's identifier rule: quotes are stripped and case is ignored. A view named ``schema`` or ``database`` must be quoted (``IN "schema"``) to be read as a view name rather than as the scope keyword.

``STARTS WITH '<prefix>'``
   Filters facts to those whose name begins with the prefix. Matching is **case-sensitive**. The prefix must be enclosed in single quotes.

``LIMIT <rows>``
   Restricts the output to the first *rows* results. Must be a non-negative integer; ``LIMIT 0`` is accepted and returns no rows.

When ``LIKE`` and ``STARTS WITH`` are both present, a fact must satisfy both conditions (they are combined with ``AND``).

.. warning::

   Clause order is enforced. ``LIKE`` must come before ``IN``, and ``STARTS WITH`` must come after ``IN``. Placing clauses out of order produces a syntax error.


.. _ref-show-facts-output:

Output Columns
==============

Returns one row per fact with 9 columns:

.. list-table::
   :header-rows: 1
   :widths: 22 12 66

   * - Column
     - Type
     - Description
   * - ``database_name``
     - VARCHAR
     - The DuckDB database containing the semantic view.
   * - ``schema_name``
     - VARCHAR
     - The DuckDB schema containing the semantic view.
   * - ``semantic_view_name``
     - VARCHAR
     - The semantic view this fact belongs to.
   * - ``table_name``
     - VARCHAR
     - The physical table name the fact is scoped to.
   * - ``name``
     - VARCHAR
     - The fact name as declared in the ``FACTS`` clause.
   * - ``data_type``
     - VARCHAR
     - The type the member's column has in a :ref:`semantic-view query <ref-semantic-view-function>`, in DuckDB's spelling (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``). Determined when the command runs, so it follows the current source columns. Empty when the member's source no longer binds (for example, its table was dropped), or when the member cannot be queried on its own because of a fan trap. See :ref:`Reported Data Types <explanation-sf-data-types>`.
   * - ``synonyms``
     - VARCHAR
     - JSON array of synonym strings (e.g., ``["discounted_price"]``). ``[]`` when no synonyms are set.
   * - ``comment``
     - VARCHAR
     - The fact comment text. Empty string if no comment is set.
   * - ``labels``
     - VARCHAR
     - JSON array of the fact's labels: ``["FILTER"]`` for a :ref:`named filter <howto-annotations-filters>`, ``[]`` otherwise.


.. versionchanged:: 0.14.0

   ``data_type`` is populated again. It had been empty for every view created
   since v0.10.0. The type is now worked out when the command runs, so views
   created in that window report it too, with no need to re-create them.

.. versionchanged:: 0.18.0

   Added the ``labels`` column, after ``comment``, as in Snowflake's output.


.. _ref-show-facts-examples:

Examples
========

**List facts for a single view:**

Given a semantic view ``orders_sv`` with one fact:

.. code-block:: sql

   SHOW SEMANTIC FACTS IN orders_sv;

.. code-block:: text

   ┌───────────────┬─────────────┬────────────────────┬────────────┬────────────┬───────────────┬──────────┬─────────┬────────┐
   │ database_name │ schema_name │ semantic_view_name │ table_name │ name       │ data_type     │ synonyms │ comment │ labels │
   ├───────────────┼─────────────┼────────────────────┼────────────┼────────────┼───────────────┼──────────┼─────────┼────────┤
   │ memory        │ main        │ orders_sv          │ orders     │ raw_amount │ DECIMAL(10,2) │ []       │         │ []     │
   └───────────────┴─────────────┴────────────────────┴────────────┴────────────┴───────────────┴──────────┴─────────┴────────┘

The example tables use ``VARCHAR`` text columns, ``DATE`` dates, ``INTEGER`` quantities and ``DECIMAL(10,2)`` amounts and prices. ``data_type`` is each member's type as a query returns it -- an aggregate's own type, not its input's: ``COUNT`` is ``BIGINT``, ``SUM`` of a ``DECIMAL(10,2)`` is ``DECIMAL(38,2)``, and a ratio of two metrics is ``DOUBLE``. See :ref:`Reported Data Types <explanation-sf-data-types>`.

**List facts across all views:**

.. code-block:: sql

   SHOW SEMANTIC FACTS;

Views with no facts are omitted.

**Filter by pattern with LIKE (case-insensitive):**

Find all facts whose name contains "amount":

.. code-block:: sql

   SHOW SEMANTIC FACTS LIKE '%amount%';

Because ``LIKE`` is case-insensitive, ``LIKE '%AMOUNT%'`` produces the same results.

**Filter by schema:**

.. code-block:: sql

   SHOW SEMANTIC FACTS IN SCHEMA main;

**Chained facts:**

Facts can reference other facts. Consider a view with two chained facts:

.. code-block:: sql

   CREATE SEMANTIC VIEW tpch_analysis AS
   TABLES (
       li AS line_items PRIMARY KEY (id)
   )
   FACTS (
       li.net_price  AS li.extended_price * (1 - li.discount),
       li.tax_amount AS li.net_price * li.tax_rate
   )
   DIMENSIONS (
       li.status AS li.status
   )
   METRICS (
       li.revenue AS SUM(li.net_price)
   );

   SHOW SEMANTIC FACTS IN tpch_analysis;

.. code-block:: text

   ┌───────────────┬─────────────┬────────────────────┬────────────┬────────────┬───────────────┬──────────┬─────────┬────────┐
   │ database_name │ schema_name │ semantic_view_name │ table_name │ name       │ data_type     │ synonyms │ comment │ labels │
   ├───────────────┼─────────────┼────────────────────┼────────────┼────────────┼───────────────┼──────────┼─────────┼────────┤
   │ memory        │ main        │ tpch_analysis      │ line_items │ net_price  │ DECIMAL(18,4) │ []       │         │ []     │
   │ memory        │ main        │ tpch_analysis      │ line_items │ tax_amount │ DECIMAL(18,6) │ []       │         │ []     │
   └───────────────┴─────────────┴────────────────────┴────────────┴────────────┴───────────────┴──────────┴─────────┴────────┘

Assuming ``extended_price DECIMAL(12,2)`` and ``discount`` / ``tax_rate DECIMAL(4,2)``, each fact reports the type of its expression with the referenced fact substituted in: ``tax_amount`` is typed through ``li.net_price``. Chained references are resolved the same way at query expansion time.

**A named filter:**

A fact declared with ``LABELS = (FILTER)`` reports ``["FILTER"]`` in the ``labels`` column:

.. code-block:: sql

   CREATE SEMANTIC VIEW orders_sv
     TABLES (o AS orders PRIMARY KEY (id))
     FACTS (
       o.raw_amount AS o.amount,
       o.is_large LABELS = (FILTER) AS o.amount > 100
     )
     METRICS (o.revenue AS SUM(o.raw_amount));

   SHOW SEMANTIC FACTS IN orders_sv;

.. code-block:: text

   ┌───────────────┬─────────────┬────────────────────┬────────────┬────────────┬───────────────┬──────────┬─────────┬────────────┐
   │ database_name │ schema_name │ semantic_view_name │ table_name │ name       │ data_type     │ synonyms │ comment │ labels     │
   ├───────────────┼─────────────┼────────────────────┼────────────┼────────────┼───────────────┼──────────┼─────────┼────────────┤
   │ memory        │ main        │ orders_sv          │ orders     │ is_large   │ BOOLEAN       │ []       │         │ ["FILTER"] │
   │ memory        │ main        │ orders_sv          │ orders     │ raw_amount │ DECIMAL(10,2) │ []       │         │ []         │
   └───────────────┴─────────────┴────────────────────┴────────────┴────────────┴───────────────┴──────────┴─────────┴────────────┘

**Error: view does not exist:**

.. code-block:: sql

   SHOW SEMANTIC FACTS IN nonexistent_view;

.. code-block:: text

   Error: semantic view 'nonexistent_view' does not exist
