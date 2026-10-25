.. meta::
   :description: Syntax reference for SHOW SEMANTIC METRICS, which lists both base and derived metrics across one or all semantic views with optional filtering

.. _ref-show-semantic-metrics:

========================
SHOW SEMANTIC METRICS
========================

Lists metrics registered in one or all semantic views. Each row describes a single metric with its name, source table, data type, synonyms, and comment. Both base metrics and derived metrics are included.


.. _ref-show-metrics-syntax:

Syntax
======

.. code-block:: sqlgrammar

   SHOW SEMANTIC METRICS
       [ LIKE '<pattern>' ]
       [ IN { <name> | ACCOUNT
            | DATABASE [ <database_name> ]
            | SCHEMA [ [<database_name>.]<schema_name> ] } ]
       [ STARTS WITH '<prefix>' ]
       [ LIMIT <rows> ]

All clauses are optional. When multiple clauses appear, they must follow the order shown above.


.. _ref-show-metrics-variants:

Statement Variants
==================

``SHOW SEMANTIC METRICS``
   Returns metrics across all registered semantic views, sorted by semantic view name and then metric name.

``SHOW SEMANTIC METRICS IN <name>``
   Returns metrics for the specified semantic view only, sorted by metric name. Returns an error if the view does not exist.


.. _ref-show-metrics-params:

Parameters
==========

``<name>``
   The name of the semantic view. Required only for the single-view form (``IN`` clause). Returns an error if the view does not exist.


.. _ref-show-metrics-filtering:

Optional Filtering Clauses
==========================

``LIKE '<pattern>'``
   Filters metrics to those whose name matches the pattern. Uses SQL ``LIKE`` pattern syntax: ``%`` matches any sequence of characters, ``_`` matches a single character. Matching is **case-insensitive** (the extension maps ``LIKE`` to DuckDB's ``ILIKE``). The pattern must be enclosed in single quotes.

``IN ...``
   Scopes the listing. The alternatives are mutually exclusive -- ``IN`` appears at most once, so a view name and a schema cannot both be given.

   ``IN <name>``
      Returns metrics for that semantic view only.

   ``IN ACCOUNT``
      Returns everything, the same as omitting ``IN`` entirely. Accepted for Snowflake compatibility; DuckDB has no account.

   ``IN DATABASE [ <database_name> ]``
      Filters metrics to semantic views in that database, or the current database when the name is omitted.

   ``IN SCHEMA [ [<database_name>.]<schema_name> ]``
      Filters metrics to semantic views in that schema, or the current schema when the name is omitted. Qualifying the schema with a database matches on both, so a same-named schema in another database is excluded.

   Schema and database names follow DuckDB's identifier rule: quotes are stripped and case is ignored. A view named ``schema`` or ``database`` must be quoted (``IN "schema"``) to be read as a view name rather than as the scope keyword.

``STARTS WITH '<prefix>'``
   Filters metrics to those whose name begins with the prefix. Matching is **case-sensitive**. The prefix must be enclosed in single quotes.

``LIMIT <rows>``
   Restricts the output to the first *rows* results. Must be a non-negative integer; ``LIMIT 0`` is accepted and returns no rows.

When ``LIKE`` and ``STARTS WITH`` are both present, a metric must satisfy both conditions (they are combined with ``AND``).

.. warning::

   Clause order is enforced. ``LIKE`` must come before ``IN``, and ``STARTS WITH`` must come after ``IN``. Placing clauses out of order produces a syntax error.


.. _ref-show-metrics-output:

Output Columns
==============

Returns one row per metric with 8 columns:

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
     - The semantic view this metric belongs to.
   * - ``table_name``
     - VARCHAR
     - The physical table name the metric is scoped to. Empty string for derived metrics (which reference other metrics rather than a specific table).
   * - ``name``
     - VARCHAR
     - The metric name as declared in the ``METRICS`` clause.
   * - ``data_type``
     - VARCHAR
     - The type the member's column has in a :ref:`semantic-view query <ref-semantic-view-function>`, in DuckDB's spelling (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``). Determined when the command runs, so it follows the current source columns. Empty when the member's source no longer binds (for example, its table was dropped), or when the member cannot be queried on its own because of a fan trap. See :ref:`Reported Data Types <explanation-sf-data-types>`.
   * - ``synonyms``
     - VARCHAR
     - JSON array of synonym strings (e.g., ``["total_sales","gmv"]``). ``[]`` when no synonyms are set.
   * - ``comment``
     - VARCHAR
     - The metric comment text. Empty string if no comment is set.


.. versionchanged:: 0.14.0

   ``data_type`` is populated again. It had been empty for every view created
   since v0.10.0. The type is now worked out when the command runs, so views
   created in that window report it too, with no need to re-create them.


.. _ref-show-metrics-examples:

Examples
========

**List metrics for a single view:**

Given a semantic view ``orders_sv`` with two base metrics:

.. code-block:: sql

   SHOW SEMANTIC METRICS IN orders_sv;

.. code-block:: text

   ┌───────────────┬─────────────┬────────────────────┬────────────┬──────────────┬───────────────┬──────────┬─────────┐
   │ database_name │ schema_name │ semantic_view_name │ table_name │ name         │ data_type     │ synonyms │ comment │
   ├───────────────┼─────────────┼────────────────────┼────────────┼──────────────┼───────────────┼──────────┼─────────┤
   │ memory        │ main        │ orders_sv          │ orders     │ order_count  │ BIGINT        │ []       │         │
   │ memory        │ main        │ orders_sv          │ orders     │ total_amount │ DECIMAL(38,2) │ []       │         │
   └───────────────┴─────────────┴────────────────────┴────────────┴──────────────┴───────────────┴──────────┴─────────┘

The example tables use ``VARCHAR`` text columns, ``DATE`` dates, ``INTEGER`` quantities and ``DECIMAL(10,2)`` amounts and prices. ``data_type`` is each member's type as a query returns it -- an aggregate's own type, not its input's: ``COUNT`` is ``BIGINT``, ``SUM`` of a ``DECIMAL(10,2)`` is ``DECIMAL(38,2)``, and a ratio of two metrics is ``DOUBLE``. See :ref:`Reported Data Types <explanation-sf-data-types>`.

**List metrics across all views:**

.. code-block:: sql

   SHOW SEMANTIC METRICS;

Results are sorted by ``semantic_view_name`` then ``name``.

**Filter by pattern with LIKE (case-insensitive):**

Find all metrics whose name contains "amount":

.. code-block:: sql

   SHOW SEMANTIC METRICS LIKE '%amount%';

Because ``LIKE`` is case-insensitive, ``LIKE '%AMOUNT%'`` produces the same results.

**Filter by schema:**

.. code-block:: sql

   SHOW SEMANTIC METRICS IN SCHEMA main;

**Derived metrics appear with an empty table_name:**

Derived metrics reference other metrics rather than a specific physical table. They are distinguished from base metrics by their empty ``table_name``:

.. code-block:: sql

   SHOW SEMANTIC METRICS IN profit_analysis;

.. code-block:: text

   ┌───────────────┬─────────────┬────────────────────┬────────────┬─────────┬───────────────┬──────────┬─────────┐
   │ database_name │ schema_name │ semantic_view_name │ table_name │ name    │ data_type     │ synonyms │ comment │
   ├───────────────┼─────────────┼────────────────────┼────────────┼─────────┼───────────────┼──────────┼─────────┤
   │ memory        │ main        │ profit_analysis    │ line_items │ cost    │ DECIMAL(38,2) │ []       │         │
   │ memory        │ main        │ profit_analysis    │            │ margin  │ DOUBLE        │ []       │         │
   │ memory        │ main        │ profit_analysis    │            │ profit  │ DECIMAL(38,2) │ []       │         │
   │ memory        │ main        │ profit_analysis    │ line_items │ revenue │ DECIMAL(38,2) │ []       │         │
   └───────────────┴─────────────┴────────────────────┴────────────┴─────────┴───────────────┴──────────┴─────────┘

Base metrics (``revenue``, ``cost``) show their physical table name. Derived metrics (``profit``, ``margin``) show an empty ``table_name`` because they reference other metrics rather than a specific table. ``profit`` keeps the ``DECIMAL`` type of the two sums it subtracts; ``margin`` divides them and is ``DOUBLE``.

**Error: view does not exist:**

.. code-block:: sql

   SHOW SEMANTIC METRICS IN nonexistent_view;

.. code-block:: text

   Error: semantic view 'nonexistent_view' does not exist
