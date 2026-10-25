.. meta::
   :description: Every SQL function the extension registers, which ones to call directly, and how a tool discovers their parameters and examples through duckdb_functions()

.. _ref-functions:

=========
Functions
=========

Loading the extension registers 19 SQL functions (18 names; ``get_ddl`` has two overloads). Most day-to-day work goes through the statements -- ``CREATE``, ``SHOW``, ``DESCRIBE`` -- and two query functions. This page lists every registered function, says which ones to call directly and which ones back a statement, and shows how a tool or agent can read their descriptions, parameters, and examples over SQL.


.. _ref-functions-inventory:

Function Inventory
==================

**Call these directly:**

.. list-table::
   :header-rows: 1
   :widths: 30 10 60

   * - Function
     - Type
     - Purpose
   * - :ref:`semantic_view() <ref-semantic-view-function>`
     - table
     - Query a semantic view: returns the requested dimensions, metrics, or facts.
   * - :ref:`explain_semantic_view() <ref-explain-semantic-view>`
     - table
     - Show the SQL and query plan ``semantic_view()`` would use for the same arguments, without running the data query.
   * - :ref:`get_ddl(object_type, object_name) <ref-get-ddl>`
     - scalar
     - Return the ``CREATE OR REPLACE SEMANTIC VIEW`` statement that recreates a stored view.
   * - :ref:`get_ddl(object_type, object_name, use_fully_qualified_names) <ref-get-ddl>`
     - scalar
     - As above, with the view name schema-qualified in the output when the third argument is ``true``.
   * - :ref:`read_yaml_from_semantic_view() <ref-read-yaml>`
     - scalar
     - Return a stored view's definition as YAML, ready to re-import with ``CREATE SEMANTIC VIEW ... FROM YAML``.

**These back a statement -- use the statement instead:**

.. list-table::
   :header-rows: 1
   :widths: 38 10 52

   * - Function
     - Type
     - Statement to use
   * - ``describe_semantic_view``
     - table
     - :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`
   * - ``list_semantic_views``
     - table
     - :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>`
   * - ``list_terse_semantic_views``
     - table
     - :ref:`SHOW TERSE SEMANTIC VIEWS <ref-show-semantic-views>`
   * - ``show_columns_in_semantic_view``
     - table
     - :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>`
   * - ``show_semantic_dimensions``
     - table
     - :ref:`SHOW SEMANTIC DIMENSIONS IN ... <ref-show-semantic-dimensions>`
   * - ``show_semantic_dimensions_all``
     - table
     - :ref:`SHOW SEMANTIC DIMENSIONS <ref-show-semantic-dimensions>` (without ``IN``)
   * - ``show_semantic_dimensions_for_metric``
     - table
     - :ref:`SHOW SEMANTIC DIMENSIONS IN ... FOR METRIC ... <ref-show-dims-for-metric>`
   * - ``show_semantic_facts``
     - table
     - :ref:`SHOW SEMANTIC FACTS IN ... <ref-show-semantic-facts>`
   * - ``show_semantic_facts_all``
     - table
     - :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>` (without ``IN``)
   * - ``show_semantic_materializations``
     - table
     - :ref:`SHOW SEMANTIC MATERIALIZATIONS IN ... <ref-show-semantic-materializations>`
   * - ``show_semantic_materializations_all``
     - table
     - :ref:`SHOW SEMANTIC MATERIALIZATIONS <ref-show-semantic-materializations>` (without ``IN``)
   * - ``show_semantic_metrics``
     - table
     - :ref:`SHOW SEMANTIC METRICS IN ... <ref-show-semantic-metrics>`
   * - ``show_semantic_metrics_all``
     - table
     - :ref:`SHOW SEMANTIC METRICS <ref-show-semantic-metrics>` (without ``IN``)

**Internal -- do not call:**

.. list-table::
   :header-rows: 1
   :widths: 38 10 52

   * - Function
     - Type
     - Notes
   * - ``__sv_compute_create_from_yaml``
     - table
     - Used by :ref:`CREATE SEMANTIC VIEW ... FROM YAML FILE <ref-create-semantic-view>`. Not part of the supported interface; its parameters and behavior can change in any release.

The statements are the stable interface. The functions behind them are listed here so that a tool reading ``duckdb_functions()`` can tell what each one is; their descriptions say which statement to use.


.. _ref-functions-discovery:

Discovering Functions over SQL
==============================

.. versionadded:: 0.13.0

Every function the extension registers has an entry in DuckDB's ``duckdb_functions()`` table function, with a description and its real parameter names and types. Every function except the internal helper also carries at least one runnable example. A tool or agent connected over SQL can learn the whole surface from there.

DuckDB 2.0's ``duckdb_functions()`` has an ``extension_name`` column, but it is ``NULL`` for this extension's functions. Filter on the ``categories`` column instead: every function this extension registers carries the ``semantic_views`` category, and no built-in DuckDB function does.

.. code-block:: sql

   SELECT DISTINCT function_name, function_type, categories[2] AS kind
   FROM duckdb_functions()
   WHERE list_contains(categories, 'semantic_views')
   ORDER BY kind DESC, function_name;

.. code-block:: text

   ┌─────────────────────────────────────┬───────────────┬──────────┐
   │ function_name                       │ function_type │ kind     │
   ├─────────────────────────────────────┼───────────────┼──────────┤
   │ explain_semantic_view               │ table         │ query    │
   │ semantic_view                       │ table         │ query    │
   │ describe_semantic_view              │ table         │ metadata │
   │ get_ddl                             │ scalar        │ metadata │
   │ list_semantic_views                 │ table         │ metadata │
   │ list_terse_semantic_views           │ table         │ metadata │
   │ read_yaml_from_semantic_view        │ scalar        │ metadata │
   │ show_columns_in_semantic_view       │ table         │ metadata │
   │ show_semantic_dimensions            │ table         │ metadata │
   │ show_semantic_dimensions_all        │ table         │ metadata │
   │ show_semantic_dimensions_for_metric │ table         │ metadata │
   │ show_semantic_facts                 │ table         │ metadata │
   │ show_semantic_facts_all             │ table         │ metadata │
   │ show_semantic_materializations      │ table         │ metadata │
   │ show_semantic_materializations_all  │ table         │ metadata │
   │ show_semantic_metrics               │ table         │ metadata │
   │ show_semantic_metrics_all           │ table         │ metadata │
   │ __sv_compute_create_from_yaml       │ table         │ internal │
   └─────────────────────────────────────┴───────────────┴──────────┘

``DISTINCT`` folds the two ``get_ddl`` overloads into one row; without it the query returns 19 rows, one per overload.

**The columns a tool needs:**

.. list-table::
   :header-rows: 1
   :widths: 22 18 60

   * - Column
     - Type
     - What it holds for this extension's functions
   * - ``function_name``
     - VARCHAR
     - The name to call.
   * - ``function_type``
     - VARCHAR
     - ``table`` (use in ``FROM``) or ``scalar`` (use in a ``SELECT`` list).
   * - ``categories``
     - VARCHAR[]
     - ``semantic_views`` plus one of ``query`` (the query functions), ``metadata`` (inspection and export), or ``internal`` (do not call).
   * - ``description``
     - VARCHAR
     - What the function does. For a function that backs a statement, the description names the statement to use instead.
   * - ``parameters``
     - VARCHAR[]
     - Parameter names. For a table function, the positional arguments come first, in call order, and everything after them is a named parameter, passed as ``name := value``, in no guaranteed order. The number of positional arguments varies: none for ``list_semantic_views``, ``list_terse_semantic_views`` and the ``*_all`` functions; one (``view_name``) for ``semantic_view``, ``explain_semantic_view`` and the other functions that take a view; two (``view_name``, ``metric_name``) for ``show_semantic_dimensions_for_metric``; three for the internal helper. The metadata doesn't mark where the positional group ends, so take the count from this list. ``search_path`` is always named, and is reserved (see :ref:`ref-functions-search-path`).
   * - ``parameter_types``
     - VARCHAR[]
     - The DuckDB type of each parameter, in the same order as ``parameters``.
   * - ``return_type``
     - VARCHAR
     - The result type of a scalar function (``VARCHAR`` for both scalars here). ``NULL`` for table functions.
   * - ``examples``
     - VARCHAR[]
     - Runnable SQL. For a function that backs a statement, the example is the statement itself.

The ``internal`` column is ``true`` for every function the extension registers, and for DuckDB's own built-in functions too, so it does not pick out the internal helper. Use the ``internal`` category for that.

**Read one function's signature, description, and example:**

.. code-block:: sql

   SELECT unnest(parameters) AS parameter, unnest(parameter_types) AS type
   FROM duckdb_functions()
   WHERE function_name = 'semantic_view';

.. code-block:: text

   ┌──────────────┬───────────┐
   │ parameter    │ type      │
   ├──────────────┼───────────┤
   │ view_name    │ VARCHAR   │
   │ dimensions   │ VARCHAR[] │
   │ metrics      │ VARCHAR[] │
   │ facts        │ VARCHAR[] │
   │ where_clause │ VARCHAR   │
   │ column_order │ VARCHAR[] │
   │ search_path  │ VARCHAR[] │
   └──────────────┴───────────┘

.. code-block:: sql

   SELECT description, examples
   FROM duckdb_functions()
   WHERE function_name = 'semantic_view';

.. code-block:: text

   description = Queries a semantic view: returns the requested dimensions,
                 metrics and/or facts, generating the joins and GROUP BY from
                 the view definition. Pass at least one of dimensions, metrics
                 or facts; an item may name its column ('region AS r').
                 where_clause filters rows before aggregation, and
                 column_order (e.g. ['metrics', 'dimensions']) sets the order
                 of the columns. The SEMANTIC_VIEW(...) construct is the same
                 query written as SQL. The search_path parameter is reserved
                 (kept so views stored by earlier releases still bind); do not
                 pass it.
      examples = [SELECT * FROM semantic_view('sales', dimensions := ['region'],
                 metrics := ['revenue']);]

The example assumes a view named ``sales`` with a ``region`` dimension and a ``revenue`` metric. The output above is shown one column per line and wrapped to fit.

**A function that backs a statement points at the statement:**

.. code-block:: sql

   SELECT description, examples
   FROM duckdb_functions()
   WHERE function_name = 'show_semantic_metrics';

.. code-block:: text

   description = Backs SHOW SEMANTIC METRICS IN <view>: lists the metrics of one
                 semantic view. Use that statement rather than calling this
                 directly. The search_path parameter is reserved (kept so
                 views stored by earlier releases still bind); do not pass it.
      examples = [SHOW SEMANTIC METRICS IN sales;]

.. tip::

   A tool that starts knowing only the extension's name can learn the function surface from ``duckdb_functions()`` as above, list the views with :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>`, list a view's queryable members with :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>`, check which dimensions are safe with a metric with :ref:`SHOW SEMANTIC DIMENSIONS ... FOR METRIC <ref-show-dims-for-metric>`, and then query with :ref:`semantic_view() <ref-semantic-view-function>`.


.. _ref-functions-search-path:

The ``search_path`` Parameter
=============================

Every table function the extension registers has a named parameter called ``search_path`` (type ``VARCHAR[]``). It is **reserved**: each function reads the session's ``search_path`` itself, so an unqualified view name resolves the way an unqualified table name does, and the parameter is kept only so that views stored by releases before 0.16.0, whose SQL text carries it, still bind. Do not pass it yourself. To control which schema an unqualified name resolves to, set DuckDB's ``search_path`` setting, or qualify the view name (``'analytics.sales'``). See :ref:`ref-create-semantic-view` for the resolution rule.

The scalar functions (``get_ddl`` and ``read_yaml_from_semantic_view``) have no ``search_path`` parameter; they read the session's path the same way (see :ref:`ref-get-ddl-resolution`).


.. _ref-functions-subqueries:

Statements as Subqueries
========================

.. versionchanged:: 0.16.0

   ``SHOW SEMANTIC ...`` and ``DESCRIBE SEMANTIC VIEW`` can be used as
   subqueries. Earlier releases refused them there with a parser error.

``SHOW SEMANTIC ...``, ``SHOW COLUMNS IN SEMANTIC VIEW`` and ``DESCRIBE SEMANTIC VIEW`` are queries, like DuckDB's own ``SHOW`` and ``DESCRIBE``, so you can use them wherever DuckDB accepts a query: in ``FROM``, in a ``WITH`` clause or in a view body. That lets you filter, join and aggregate what they return:

.. code-block:: sql

   SELECT DISTINCT object_name FROM (DESCRIBE SEMANTIC VIEW sales) WHERE object_kind = 'METRIC';

``CREATE``, ``DROP`` and ``ALTER SEMANTIC VIEW`` are statements, not queries, so they can't appear inside one.

The :ref:`GET_DDL <ref-get-ddl-examples>` recipe for dumping every view reads from ``(SHOW SEMANTIC VIEWS)`` this way.
