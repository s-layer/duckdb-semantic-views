.. meta::
   :description: Syntax and parameter reference for semantic_view(), the table function that queries any combination of dimensions, metrics, or facts

.. _ref-semantic-view-function:

=====================
semantic_view()
=====================

Table function that queries a semantic view with a specified combination of dimensions, metrics, or facts. The extension generates the SQL (SELECT, FROM, JOIN, GROUP BY) and returns the result set.

You can also write the same query as Snowflake does, with the :ref:`SEMANTIC_VIEW(...) construct <ref-sv-construct>`, which the extension turns into a ``semantic_view()`` call.


.. _ref-sv-syntax:

Syntax
======

.. code-block:: sqlgrammar

   SELECT * FROM semantic_view(
       '<view_name>',
       [ dimensions := [ '<dim_name>' [, ...] ] , ]
       [ metrics := [ '<metric_name>' [, ...] ] , ]
       [ facts := [ '<fact_name>' [, ...] ] , ]
       [ where_clause := '<predicate>' , ]
       [ column_order := [ '<list_name>' [, ...] ] ]
   )

Each item in ``dimensions``, ``metrics`` or ``facts`` is ``'<member_name>'``, ``'<member_name> AS <alias>'`` or ``'<table_alias>.*'``.


.. _ref-sv-params:

Parameters
==========

.. list-table::
   :header-rows: 1
   :widths: 20 15 65

   * - Parameter
     - Type
     - Description
   * - ``<view_name>``
     - VARCHAR (positional)
     - The name of the semantic view to query. Must match a registered view. The name is folded to lowercase and matched case-insensitively, quoted or not (``'Sales'``, ``'SALES'``, and ``'"sales"'`` all resolve to the same view), following DuckDB's identifier semantics. May carry a ``<schema>.`` (or ``<database>.<schema>.``) qualifier -- ``'analytics.sales'`` -- which pins the schema. An unqualified name resolves through the session's ``search_path``, exactly as an unqualified table reference does: the first schema on the path holding a view of that name wins. A view that is the only one of its name resolves whether or not its schema is on the path, so a single-schema setup needs no ``search_path`` at all (see :ref:`ref-create-semantic-view`).
   * - ``dimensions``
     - VARCHAR[] (named)
     - Optional list of dimension names to include in the result. Each name must match a dimension defined in the semantic view. Supports ``alias.*`` wildcard patterns.
   * - ``metrics``
     - VARCHAR[] (named)
     - Optional list of metric names to include in the result. Each name must match a metric defined in the semantic view. Supports ``alias.*`` wildcard patterns.
   * - ``facts``
     - VARCHAR[] (named)
     - Optional list of fact names to include in the result. Each name must match a fact defined in the semantic view. Supports ``alias.*`` wildcard patterns.
   * - ``where_clause``
     - VARCHAR (named)
     - Optional predicate applied **before** metrics are aggregated -- the equivalent of Snowflake's ``SEMANTIC_VIEW( ... WHERE <predicate> )``. See :ref:`ref-sv-pre-agg-filtering`. An omitted, empty, or whitespace-only value is treated as absent.
   * - ``column_order``
     - VARCHAR[] (named)
     - Optional order of the lists' columns in the result, as list names: ``'dimensions'``, ``'metrics'`` and ``'facts'``, matched case-insensitively. A list you leave out follows in the default order, dimensions, then metrics, then facts. Naming a list twice, or something that is not a list, is an error. See :ref:`ref-sv-output`.
   * - ``search_path``
     - VARCHAR[] (named)
     - **Reserved. Do not pass it.** The function reads the session's ``search_path`` itself, so an unqualified ``<view_name>`` resolves the way an unqualified table name does. Every table function the extension registers has this parameter (see :ref:`ref-functions-search-path`).

At least one of ``dimensions``, ``metrics``, or ``facts`` must be specified. ``where_clause`` alone is not a query.

Every argument accepts a prepared-statement parameter (``?`` or ``$1``) in place of a literal, including the view name, the three lists, and ``where_clause``. See :ref:`ref-sv-pre-agg-filtering` before binding a user-supplied predicate.

.. warning::

   ``facts`` and ``metrics`` cannot be combined in the same query. Use ``facts := [...]`` or ``metrics := [...]``, not both.


.. _ref-sv-modes:

Query Modes
===========

The function operates in four modes depending on which parameters are provided. The examples query the ``shop`` view defined under :ref:`ref-sv-examples`.

**Dimensions + Metrics** (grouped aggregation):

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue', 'order_count']
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬──────────┬─────────────┐
   │ region │ revenue  │ order_count │
   ├────────┼──────────┼─────────────┤
   │ East   │ 340.0000 │           2 │
   │ West   │ 330.0000 │           2 │
   └────────┴──────────┴─────────────┘

Generates ``SELECT <dims>, <metrics> FROM ... GROUP BY <dims>``.

**Dimensions only** (distinct values):

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region']
   ) ORDER BY region;

.. code-block:: text

   ┌────────┐
   │ region │
   ├────────┤
   │ East   │
   │ West   │
   └────────┘

Generates ``SELECT DISTINCT <dims> FROM ...``.

**Metrics only** (grand total, global aggregate):

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       metrics := ['revenue', 'order_count']
   );

.. code-block:: text

   ┌──────────┬─────────────┐
   │ revenue  │ order_count │
   ├──────────┼─────────────┤
   │ 670.0000 │           4 │
   └──────────┴─────────────┘

Generates ``SELECT <metrics> FROM ...`` with no GROUP BY (returns one row).

**Facts mode** (row-level, no aggregation):

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       facts := ['net_price', 'tax_amount']
   );

.. code-block:: text

   ┌───────────┬────────────┐
   │ net_price │ tax_amount │
   ├───────────┼────────────┤
   │   90.0000 │   7.200000 │
   │  250.0000 │  20.000000 │
   │   60.0000 │   3.000000 │
   │  270.0000 │  13.500000 │
   └───────────┴────────────┘

Returns one row per source row with the requested fact expressions as columns. No aggregation or GROUP BY is applied. Dimensions can be combined with facts (they appear as columns without triggering grouping).


.. _ref-sv-wildcard:

Wildcard Selection
==================

All three list parameters accept ``alias.*`` patterns that expand to all items scoped to the specified table alias. Items declared without a table alias, such as derived metrics, count as belonging to the first table in ``TABLES``, so ``o.*`` on a view whose first table is ``o`` also returns its public derived metrics:

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['o.*'],
       metrics := ['o.*']
   );

``PRIVATE`` metrics and facts are excluded from wildcard expansion. Bare ``*`` (unqualified) is not supported -- all wildcards must be qualified with a table alias.

When an item appears both explicitly and via wildcard expansion, it appears only once in the result (deduplication).


.. _ref-sv-output:

Output
======

Returns a result set with one column per requested dimension, metric, or fact, in the order: dimensions first (in the order requested), then metrics or facts (in the order requested). ``column_order`` changes which list's columns come first, as in ``column_order := ['metrics', 'dimensions']``; within a list the columns stay in the order requested.

A column is named after its member, or after the alias the item gives it with ``AS``:

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region AS r'],
       metrics := ['revenue AS "Total revenue"'],
       column_order := ['metrics', 'dimensions']
   );

The alias is one identifier, quoted or not, or a string (``AS 'r'``). Two columns with the same name are an error, and so is an alias on a wildcard. Since the text before ``AS`` is the member, a member whose name contains the word ``as`` on its own (``Sales as of``) has to be quoted in an item: ``'"Sales as of"'``.

.. versionadded:: 0.16.0

   Aliases in items and the ``column_order`` parameter.

DuckDB plans the generated SQL as part of the query that calls ``semantic_view()``, so the column types are the ones DuckDB gives the generated SQL: ``SUM`` over an integer column is a ``HUGEINT``, and an ``ENUM`` or ``STRUCT`` column keeps its type. Nothing is cached at ``CREATE``.

Because it is part of your query, ``semantic_view()`` reads your open transaction, including rows you have not committed yet, and DuckDB can push your query's filters and column choices down into it. ``where_clause`` can refer to a column of an enclosing query, as in a ``LATERAL`` join:

.. code-block:: sql

   SELECT t.r, s.revenue
   FROM targets t,
        LATERAL (SELECT * FROM semantic_view('shop',
                    metrics := ['revenue'],
                    where_clause := 'region = t.r')) s;

If the generated SQL does not bind, for example because a metric names a column that has since been dropped, the query fails with DuckDB's own error, and the caret points at the ``semantic_view(...)`` call. Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to see the generated SQL.

.. versionchanged:: 0.16.0

   Column types are DuckDB's own. Earlier releases returned a ``HUGEINT`` as ``BIGINT``, a ``UHUGEINT`` as ``UBIGINT``, and ``ENUM``, ``STRUCT`` and ``MAP`` columns as ``VARCHAR``. Errors in the generated SQL used to be wrapped as ``semantic_view: SQL execution failed: ...`` or ``semantic_view: type inference failed for query ...``.

.. versionchanged:: 0.11.0

   A dimension, metric, or fact declared with a double-quoted name (e.g.
   ``"order date"``) now produces an output column named by its **logical
   value** (``order date``), with the quote characters stripped. Previously the
   column was named ``"order date"`` -- quote characters included. A consumer
   selecting the old quote-laden column name must update. Queries over unquoted
   names are unaffected.


.. _ref-sv-construct:

The ``SEMANTIC_VIEW(...)`` Construct
====================================

Snowflake queries a semantic view with ``SEMANTIC_VIEW(...)`` in ``FROM``, and so can you:

.. code-block:: sqlgrammar

   SELECT * FROM SEMANTIC_VIEW(
       <view_name>
       [ DIMENSIONS <item> [, ...] ]
       [ METRICS <item> [, ...] ]
       [ FACTS <item> [, ...] ]
       [ WHERE <predicate> ]
   )

where each ``<item>`` is ``<member_name> [ [AS] <alias> ]`` or ``<table_alias>.*``. The view name and the items are identifiers, not strings, and no comma follows the view name. The clauses can come in any order, each at most once, and the order you write them in is the order of the columns. ``WHERE`` takes one predicate and filters the rows the metrics aggregate, as ``where_clause`` does.

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop METRICS revenue AS total DIMENSIONS region);

   -- is the same query as
   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue AS total'],
       column_order := ['metrics', 'dimensions']
   );

The construct parses wherever a table can: in a join, a CTE, a subquery or a view body, with a table alias and column aliases. A view over it stores the ``semantic_view()`` call, so it expands against the semantic view's current definition each time it is queried. Items are member names for now: an expression is refused, and so is a prepared statement's ``?`` inside the construct. To bind a parameter, call ``semantic_view()`` and pass it as a whole argument.

.. versionadded:: 0.16.0


.. _ref-sv-filtering:

Filtering
=========

There are two filters, and they run on opposite sides of the aggregation.

Post-Aggregation -- Outer ``WHERE``
-----------------------------------

Use standard SQL ``WHERE`` on the outer query to filter the rows the function returns:

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue']
   ) WHERE region = 'East';

The ``WHERE`` clause applies to the result set after the semantic view expansion. DuckDB's optimizer pushes predicates down into the generated query where possible.


.. _ref-sv-pre-agg-filtering:

Pre-Aggregation -- ``where_clause``
-----------------------------------

``where_clause := '<predicate>'`` filters the rows the metrics aggregate **over**, before they are aggregated -- the equivalent of Snowflake's ``SEMANTIC_VIEW( ... WHERE <predicate> )``:

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue'],
       where_clause := 'ordered_at >= DATE ''2024-01-01'''
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬──────────┐
   │ region │ revenue  │
   ├────────┼──────────┤
   │ East   │ 250.0000 │
   │ West   │ 330.0000 │
   └────────┴──────────┘

Each region's ``revenue`` is recomputed over only the matching orders: East loses its December 2023 order and falls from 340.0000 to 250.0000. An outer ``WHERE`` cannot express this: by then the aggregation has already run over every row, and ``ordered_at`` is not in the output to filter by.

The parameter is spelled ``where_clause`` rather than ``where`` because DuckDB reserves ``where`` in named-parameter position -- ``where := '...'`` is a parse error before the extension is consulted.

**What the predicate may name.** Declared dimensions and facts, referenced by their logical names; each is substituted to its expression, wrapped in parentheses so a member that binds looser than its surrounding context keeps its grouping. Members declared :ref:`LABELS = (FILTER) <howto-annotations-filters>` are the intended case, but any dimension or fact works -- the label records intent and does not gate resolution. Naming a **metric** is rejected, matching Snowflake: the filter runs before aggregation, so an aggregate has no value yet.

A name that is not a declared dimension or fact is left as written. It binds only if it is a column of a table the query already reads (``price`` or ``o.price`` on a single-table view, for example); it does not cause a join, so a column of any other table fails with DuckDB's ``Referenced column ... not found`` binder error. Declare the column as a dimension or fact to make it reachable.

**Subqueries.** A subquery in the predicate is uncorrelated, as in Snowflake. Names inside it are its own tables' columns, never members, and it sees none of the semantic view's tables. In ``region IN (SELECT code FROM codes)``, ``region`` is the member and ``code`` is a column of ``codes``. A name the subquery's tables lack, such as ``region`` in ``EXISTS (SELECT 1 FROM codes WHERE code = region)``, fails with DuckDB's ``Referenced column ... not found`` error. Write the condition on the member outside the subquery.

.. versionchanged:: 0.16.0

   A member name inside a subquery used to be replaced with the member's expression, and a name the subquery's tables lacked could read a column of the semantic view's tables.

**Joins and fan-out.** Tables the predicate reaches are joined in and subjected to the same reachability and fan-out checks as a queried dimension's, matching Snowflake's rule that WHERE-clause members participate in the same-logical-table constraint. A member on a role-played table -- one reached through two named relationships -- is rejected rather than bound to whichever relationship was declared first, since the predicate has no way to say which role is meant.

**Where it is applied.** Before the ``GROUP BY`` when the query is a single ``SELECT`` (one grain, dimensions only, or facts), inside each grain CTE for multi-grain queries, inside the snapshot CTE ahead of the ranking for semi-additive metrics, and inside the aggregate CTE ahead of the window function for window metrics -- so a filtered number is always recomputed rather than filtered after the fact.

An omitted, empty, or whitespace-only ``where_clause`` is treated as absent. The two filters compose: a pre-aggregation predicate and an outer ``WHERE`` in the same query each do their own job.

.. warning::

   The predicate is SQL text that runs with the caller's privileges, and it may contain anything a ``WHERE`` clause can, including subqueries over other tables. Passing it as a prepared-statement parameter (``where_clause := ?``) saves you quoting the string, but it does **not** make untrusted text safe. Never build the predicate from request input. Validate each user-supplied value against the values you expect (a date, a region from a known list), and prefer predicates made of :ref:`named filters <howto-annotations-filters>` that you declared, so that user input chooses which filter applies rather than supplying SQL.


.. _ref-sv-ordering:

Ordering and Limiting
=====================

Use standard SQL ``ORDER BY`` and ``LIMIT`` on the outer query:

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue']
   ) ORDER BY revenue DESC
   LIMIT 10;


.. _ref-sv-time-limits:

Time Limits
===========

.. versionadded:: 0.15.0

DuckDB 2.0's per-query time limit, ``SET max_execution_time`` (in milliseconds), applies to ``semantic_view()`` the same way it applies to any other query. A query that runs past the limit stops with DuckDB's own error:

.. code-block:: sql

   SET max_execution_time = 5000;  -- 5 seconds per query

   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue']
   );

.. code-block:: text

   INTERRUPT Error: Query exceeded maximum execution time

The limit also covers the commands that report member data types, because they run a typing query against the view's tables: :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`, :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>`, :ref:`SHOW SEMANTIC DIMENSIONS <ref-show-semantic-dimensions>`, :ref:`SHOW SEMANTIC METRICS <ref-show-semantic-metrics>`, :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>`, and :ref:`SHOW SEMANTIC DIMENSIONS FOR METRIC <ref-show-dims-for-metric>`. ``RESET max_execution_time`` removes the limit. See :ref:`ref-err-time-limit`.

As with DuckDB's own queries, the limit counts from when the query starts running. Time DuckDB spends planning a source that is slow to bind, such as a ``read_parquet`` over a long list of files, is not limited.


.. _ref-sv-name-resolution:

Name Resolution
===============

Dimension, metric, and fact names are resolved case-insensitively, following DuckDB's identifier semantics: matching ignores case whether the reference is written unquoted (``'region'``, ``'REGION'``) or double-quoted (``'"Region"'``) -- DuckDB treats double-quoted identifiers as case-insensitive too, so quoting a reference only lets it carry whitespace or special characters, it does not make it case-sensitive. Names can optionally be table-qualified (e.g., ``'o.region'``), which matches against the ``source_table`` alias of the dimension, metric, or fact.

Wildcard patterns (``alias.*``) are expanded before name resolution. The expansion respects ``PRIVATE`` access modifiers -- private items are excluded.

If a name does not match any defined dimension, metric, or fact, the error message lists available names and suggests the closest match (if one exists within 3 edits).

**Tables in the definition.** Each table in ``TABLES`` is the one CREATE found. A name written without a schema is the table of that name in the database and schema the view was created in, or your session's temporary table of that name if that is what it named at CREATE, so a semantic view over a temporary table can be queried in the session that has the table. A name written ``schema.table`` is looked up in the database the view was created in. A table created later, in your current schema or as a temporary table, is not read in its place.

**Names in member expressions.** A table in a subquery inside a dimension, fact or metric expression, a function or macro, and a type in a cast resolve as they would in a DuckDB view stored in the schema the semantic view was created in: your session's temporary objects first, then that schema, then its database's default schema, then your search path. A CTE of the same name in your query, or an object of that name in your current schema, is not used in their place. A table named in a ``where_clause`` subquery is part of your query and resolves there, so it can read your CTEs. A function or type ``where_clause`` names outside its subqueries resolves like the definition's.

.. versionchanged:: 0.16.0
   Names in member expressions no longer resolve to your query's CTEs or your current schema, and a semantic view over a temporary table can be queried.


.. _ref-sv-examples:

Examples
========

The examples on this page use this table and view:

.. code-block:: sql

   CREATE TABLE orders (
       id INTEGER,
       customer VARCHAR,
       product VARCHAR,
       region VARCHAR,
       ordered_at DATE,
       price DECIMAL(10, 2),
       discount DECIMAL(4, 2),
       tax_rate DECIMAL(4, 2)
   );
   INSERT INTO orders VALUES
       (1, 'Acme',    'Widget', 'East', DATE '2023-12-15', 100.00, 0.10, 0.08),
       (2, 'Acme',    'Gadget', 'East', DATE '2024-01-10', 250.00, 0.00, 0.08),
       (3, 'Globex',  'Widget', 'West', DATE '2024-02-03',  80.00, 0.25, 0.05),
       (4, 'Initech', 'Gadget', 'West', DATE '2024-03-21', 300.00, 0.10, 0.05);

   CREATE SEMANTIC VIEW shop AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   FACTS (
       o.net_price  AS o.price * (1 - o.discount),
       o.tax_amount AS o.net_price * o.tax_rate
   )
   DIMENSIONS (
       o.customer   AS o.customer,
       o.product    AS o.product,
       o.region     AS o.region,
       o.ordered_at AS o.ordered_at
   )
   METRICS (
       o.revenue     AS SUM(o.net_price),
       o.order_count AS COUNT(*)
   );

**Two dimensions, two metrics:**

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['customer', 'product'],
       metrics := ['revenue', 'order_count']
   ) ORDER BY customer, product;

.. code-block:: text

   ┌──────────┬─────────┬──────────┬─────────────┐
   │ customer │ product │ revenue  │ order_count │
   ├──────────┼─────────┼──────────┼─────────────┤
   │ Acme     │ Gadget  │ 250.0000 │           1 │
   │ Acme     │ Widget  │  90.0000 │           1 │
   │ Globex   │ Widget  │  60.0000 │           1 │
   │ Initech  │ Gadget  │ 270.0000 │           1 │
   └──────────┴─────────┴──────────┴─────────────┘

**Filter and order the result (outer WHERE, after aggregation):**

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['customer'],
       metrics := ['revenue']
   ) WHERE revenue > 100
   ORDER BY revenue DESC;

.. code-block:: text

   ┌──────────┬──────────┐
   │ customer │ revenue  │
   ├──────────┼──────────┤
   │ Acme     │ 340.0000 │
   │ Initech  │ 270.0000 │
   └──────────┴──────────┘

**Pre-aggregation filtering (revenue per customer, over 2024 orders only):**

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['customer'],
       metrics := ['revenue'],
       where_clause := 'ordered_at >= DATE ''2024-01-01'''
   ) ORDER BY customer;

.. code-block:: text

   ┌──────────┬──────────┐
   │ customer │ revenue  │
   ├──────────┼──────────┤
   │ Acme     │ 250.0000 │
   │ Globex   │  60.0000 │
   │ Initech  │ 270.0000 │
   └──────────┴──────────┘

**Facts with dimensions (row-level):**

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       facts := ['net_price']
   );

.. code-block:: text

   ┌────────┬───────────┐
   │ region │ net_price │
   ├────────┼───────────┤
   │ East   │   90.0000 │
   │ East   │  250.0000 │
   │ West   │   60.0000 │
   │ West   │  270.0000 │
   └────────┴───────────┘

**Wildcard selection:**

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['o.*'],
       metrics := ['o.*']
   );

Returns every dimension and public metric on ``o``: ``customer``, ``product``, ``region``, ``ordered_at``, ``revenue``, and ``order_count``.

**Bind arguments from application code:**

.. code-block:: python

   import duckdb

   con = duckdb.connect()
   con.execute("LOAD semantic_views")
   # ... create the orders table and the shop view shown above ...

   rows = con.execute(
       """
       SELECT * FROM semantic_view(
           'shop',
           dimensions := ?,
           metrics := ['revenue']
       ) ORDER BY region
       """,
       [["region"]],
   ).fetchall()
   print(rows)
   # [('East', Decimal('340.0000')), ('West', Decimal('330.0000'))]

The list of dimensions is bound as a parameter. ``where_clause`` can be bound the same way, but read the warning in :ref:`ref-sv-pre-agg-filtering` first: a bound predicate is still SQL text.
