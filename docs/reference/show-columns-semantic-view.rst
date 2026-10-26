.. meta::
   :description: Syntax reference for SHOW COLUMNS IN SEMANTIC VIEW, which lists all queryable columns (dimensions, facts, metrics) with their types, expressions, and comments

.. _ref-show-columns:

=================================
SHOW COLUMNS IN SEMANTIC VIEW
=================================

Lists all queryable columns in a semantic view -- dimensions, facts, and metrics -- with their data types, expressions, kind, and comments. Private items are excluded from the output.


.. _ref-show-columns-syntax:

Syntax
======

.. code-block:: sqlgrammar

   SHOW COLUMNS IN SEMANTIC VIEW <name>


.. _ref-show-columns-params:

Parameters
==========

``<name>``
   The name of the semantic view. Returns an error if the view does not exist.


.. _ref-show-columns-output:

Output Columns
==============

Returns one row per queryable column with 8 columns:

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
     - The semantic view name.
   * - ``column_name``
     - VARCHAR
     - The dimension, fact, or metric name.
   * - ``data_type``
     - VARCHAR
     - The type the member's column has in a :ref:`semantic-view query <ref-semantic-view-function>`, in DuckDB's spelling (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``). Determined when the command runs, so it follows the current source columns. Empty when the member's source no longer binds (for example, its table was dropped), or when the member cannot be queried on its own because of a fan trap. See :ref:`Reported Data Types <explanation-sf-data-types>`.
   * - ``kind``
     - VARCHAR
     - The column kind: ``DIMENSION``, ``FACT``, ``METRIC``, or ``DERIVED_METRIC``.
   * - ``expression``
     - VARCHAR
     - The SQL expression defining the column.
   * - ``comment``
     - VARCHAR
     - The comment text. Empty string if no comment is set.


.. versionchanged:: 0.14.0

   ``data_type`` is populated again. It had been empty for every view created
   since v0.10.0. The type is now worked out when the command runs, so views
   created in that window report it too, with no need to re-create them.


.. _ref-show-columns-kind:

Kind Values
===========

.. list-table::
   :header-rows: 1
   :widths: 25 75

   * - Kind
     - Description
   * - ``DIMENSION``
     - A grouping expression from the ``DIMENSIONS`` clause.
   * - ``FACT``
     - A row-level expression from the ``FACTS`` clause.
   * - ``METRIC``
     - A base metric (scoped to a table) from the ``METRICS`` clause.
   * - ``DERIVED_METRIC``
     - A derived metric (no table alias, references other metrics) from the ``METRICS`` clause.


.. _ref-show-columns-private:

PRIVATE Exclusion
=================

Metrics and facts marked ``PRIVATE`` are excluded from the output. Only ``PUBLIC`` items (the default) appear. This matches the behavior of wildcard expansion in :ref:`semantic_view() <ref-semantic-view-function>` queries.


.. _ref-show-columns-sort:

Sort Order
==========

Rows are sorted by ``kind`` (alphabetically: ``DERIVED_METRIC``, ``DIMENSION``, ``FACT``, ``METRIC``) and then by ``column_name`` within each kind.


.. _ref-show-columns-examples:

Examples
========

.. code-block:: sql

   CREATE SEMANTIC VIEW shop AS
   TABLES (o AS orders PRIMARY KEY (id))
   FACTS (o.raw_amount AS o.quantity * o.price COMMENT = 'Line total')
   DIMENSIONS (o.region AS o.region)
   METRICS (
       o.revenue     AS SUM(o.quantity * o.price),
       o.order_count AS COUNT(o.id),
       avg_order     AS revenue / order_count
   );

   SHOW COLUMNS IN SEMANTIC VIEW shop;

.. code-block:: text

   ┌───────────────┬─────────────┬────────────────────┬─────────────┬───────────────┬────────────────┬───────────────────────────┬────────────┐
   │ database_name │ schema_name │ semantic_view_name │ column_name │ data_type     │ kind           │ expression                │ comment    │
   ├───────────────┼─────────────┼────────────────────┼─────────────┼───────────────┼────────────────┼───────────────────────────┼────────────┤
   │ memory        │ main        │ shop               │ avg_order   │ DOUBLE        │ DERIVED_METRIC │ revenue / order_count     │            │
   │ memory        │ main        │ shop               │ region      │ VARCHAR       │ DIMENSION      │ o.region                  │            │
   │ memory        │ main        │ shop               │ raw_amount  │ DECIMAL(18,2) │ FACT           │ o.quantity * o.price      │ Line total │
   │ memory        │ main        │ shop               │ order_count │ BIGINT        │ METRIC         │ COUNT(o.id)               │            │
   │ memory        │ main        │ shop               │ revenue     │ DECIMAL(38,2) │ METRIC         │ SUM(o.quantity * o.price) │            │
   └───────────────┴─────────────┴────────────────────┴─────────────┴───────────────┴────────────────┴───────────────────────────┴────────────┘

``avg_order`` is a ``DERIVED_METRIC`` because it has no table alias and its expression names two other metrics. A derived metric may not contain an aggregate function of its own -- ``avg_order AS revenue / COUNT(*)`` is rejected at ``CREATE``, which is why the order count is declared as the base metric ``o.order_count`` first. See :ref:`howto-derived-metrics`.

The example tables use ``VARCHAR`` text columns, ``DATE`` dates, ``INTEGER`` quantities and ``DECIMAL(10,2)`` amounts and prices. ``data_type`` is each member's type as a query returns it -- an aggregate's own type, not its input's: ``COUNT`` is ``BIGINT``, ``SUM`` of a ``DECIMAL(10,2)`` is ``DECIMAL(38,2)``, and a ratio of two metrics is ``DOUBLE``. See :ref:`Reported Data Types <explanation-sf-data-types>`.

**Error: view does not exist:**

.. code-block:: sql

   SHOW COLUMNS IN SEMANTIC VIEW nonexistent;

.. code-block:: text

   Error: semantic view 'nonexistent' does not exist
