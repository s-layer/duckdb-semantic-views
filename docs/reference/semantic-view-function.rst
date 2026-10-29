.. meta::
   :description: Syntax reference for querying a semantic view with the SEMANTIC_VIEW(...) construct or the semantic_view() table function: clauses, parameters, expression items, filters, variables and output

.. _ref-semantic-view-function:

======================================
SEMANTIC_VIEW(...) and semantic_view()
======================================

Query a semantic view by naming the dimensions, metrics or facts you want. The extension generates the SQL (SELECT, FROM, JOIN, GROUP BY) and returns the result set. The same query has two spellings:

- The ``SEMANTIC_VIEW(...)`` construct in ``FROM``, written as in Snowflake. Use it in the SQL you write.
- The ``semantic_view()`` table function, which takes the request as named lists. The construct turns into a call to it. Use it when application code builds the lists, binds them as parameters, or passes values in ``params``.

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop DIMENSIONS region METRICS revenue);

   -- is the same query as
   SELECT * FROM semantic_view('shop', dimensions := ['region'], metrics := ['revenue']);

Both are supported and return the same result. The examples on this page use the construct, and the ``shop`` view defined under :ref:`ref-sv-examples`.

A third spelling names the view in ``FROM`` like a table, with the dimensions in ``GROUP BY`` and each metric passed to ``AGG``: ``SELECT region, AGG(revenue) FROM shop GROUP BY region``. It turns into the construct, so everything on this page applies to it as well. Its clause mapping and rules are in :ref:`howto-standard-sql`, and its errors under :ref:`ref-err-standard-sql`.


.. _ref-sv-syntax:

Syntax
======

**The SEMANTIC_VIEW(...) construct:**

.. code-block:: sqlgrammar

   SELECT * FROM SEMANTIC_VIEW(
       <view_name>
       [ DIMENSIONS <item> [, ...] ]
       [ METRICS <item> [, ...] ]
       [ FACTS <item> [, ...] ]
       [ WHERE <predicate> ]
       [ VARIABLES <var_name> { = | => } <value> [, ...] ]
   )

where each ``<item>`` is ``<member_name> [ [AS] <alias> ]``, ``<expression> [ [AS] <alias> ]`` or ``<table_alias>.*``. The view name and the items are identifiers, not strings, and no comma follows the view name. The clauses can come in any order, each at most once, and the order you write them in is the order of the columns. ``VARIABLES`` is the exception: it comes last. See :ref:`ref-sv-construct`.

**The semantic_view() table function:**

.. code-block:: sqlgrammar

   SELECT * FROM semantic_view(
       '<view_name>',
       [ dimensions := [ '<item>' [, ...] ] , ]
       [ metrics := [ '<item>' [, ...] ] , ]
       [ facts := [ '<item>' [, ...] ] , ]
       [ where_clause := '<predicate>' , ]
       [ params := row( <value> [, ...] ) , ]
       [ variables := { '<var_name>': <value> [, ...] } , ]
       [ column_order := [ '<list_name>' [, ...] ] ]
   )

Each item in ``dimensions``, ``metrics`` or ``facts`` is a string holding ``<member_name>`` or ``<expression>``, either one optionally followed by ``AS <alias>``, or ``<table_alias>.*``.

Each clause of the construct is the function parameter of the same name, and ``WHERE`` is ``where_clause``. ``DIMENSIONS region METRICS revenue WHERE ordered_at >= DATE '2024-01-01'`` is ``dimensions := ['region'], metrics := ['revenue'], where_clause := 'ordered_at >= DATE ''2024-01-01'''``, and writing ``METRICS`` before ``DIMENSIONS`` is ``column_order := ['metrics', 'dimensions']``. ``VARIABLES min_amount = 2000, target_region => 'West'`` is ``variables := {'min_amount': 2000, 'target_region': 'West'}``. See :ref:`ref-sv-expression-items` for what an expression item may hold.


.. _ref-sv-params:

Parameters
==========

The table lists the parameters of ``semantic_view()``. The construct's clauses are the parameters of the same name, with ``WHERE`` for ``where_clause`` and ``VARIABLES`` for ``variables``, and its view name is the same name written as an identifier (``shop``, ``analytics.sales``) instead of a string. The construct has no ``params`` or ``column_order``: its ``WHERE`` takes prepared-statement parameters directly (see :ref:`ref-sv-construct`), and the order of its clauses sets the order of the columns.

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
     - Optional list of dimensions to include in the result. Each item names a dimension defined in the semantic view, or is an :ref:`expression <ref-sv-expression-items>`. A fact listed here is grouped on, like a dimension. Supports ``alias.*`` wildcard patterns.
   * - ``metrics``
     - VARCHAR[] (named)
     - Optional list of metrics to include in the result. Each item names a metric defined in the semantic view, or is an :ref:`expression <ref-sv-expression-items>`. Supports ``alias.*`` wildcard patterns.
   * - ``facts``
     - VARCHAR[] (named)
     - Optional list of facts to include in the result. Each item names a fact defined in the semantic view, or is an :ref:`expression <ref-sv-expression-items>`. A dimension listed here is read row by row, like a fact. Cannot be combined with ``dimensions``. Supports ``alias.*`` wildcard patterns.
   * - ``where_clause``
     - VARCHAR (named)
     - Optional predicate applied **before** metrics are aggregated -- the construct's ``WHERE``, as in Snowflake's ``SEMANTIC_VIEW( ... WHERE <predicate> )``. See :ref:`ref-sv-pre-agg-filtering`. An omitted, empty, or whitespace-only value is treated as absent.
   * - ``params``
     - row (named)
     - Optional values for ``where_clause``, which refers to them as ``$1``, ``$2``, ... in order. Each is put in the predicate as a value of its own type, never as SQL text. See :ref:`values from params <ref-sv-where-params>`.
   * - ``variables``
     - STRUCT (named)
     - Optional values for the view's :ref:`variables <ref-create-variables>`, by name: ``{'min_amount': 2000, 'target_region': 'West'}``. It must be a struct with named fields. A variable left out takes its default. See :ref:`ref-sv-variables`.

       .. versionadded:: 0.18.0
   * - ``column_order``
     - VARCHAR[] (named)
     - Optional order of the lists' columns in the result, as list names: ``'dimensions'``, ``'metrics'`` and ``'facts'``, matched case-insensitively. A list you leave out follows in the default order, dimensions, then metrics, then facts. Naming a list twice, or something that is not a list, is an error. See :ref:`ref-sv-output`.
   * - ``search_path``
     - VARCHAR[] (named)
     - **Reserved. Do not pass it.** The function reads the session's ``search_path`` itself, so an unqualified ``<view_name>`` resolves the way an unqualified table name does. Every table function the extension registers has this parameter (see :ref:`ref-functions-search-path`).

At least one of ``dimensions``, ``metrics``, or ``facts`` (in the construct, ``DIMENSIONS``, ``METRICS`` or ``FACTS``) must be specified. A filter alone is not a query.

Every argument accepts a prepared-statement parameter (``?`` or ``$1``) in place of a literal, including the view name, the three lists, ``where_clause`` and the values in ``params`` and ``variables``. A parameter written *inside* the ``where_clause`` text is not one: pass the value in ``params`` instead (see :ref:`values from params <ref-sv-where-params>`).

.. warning::

   Facts and metrics cannot be combined in the same query. Use ``FACTS`` or ``METRICS`` (``facts := [...]`` or ``metrics := [...]``), not both.

   Nor can ``FACTS`` and ``DIMENSIONS``, as in Snowflake. A dimension listed in ``FACTS`` is read row by row, like a fact; a fact listed in ``DIMENSIONS`` is grouped on, like a dimension. See :ref:`howto-query-facts-dims`.


.. _ref-sv-construct:

The ``SEMANTIC_VIEW(...)`` Construct
====================================

Snowflake queries a semantic view with ``SEMANTIC_VIEW(...)`` in ``FROM``, and so can you. The view name and the items are written as identifiers, so a name with a space or a reserved word is double-quoted (``"Total Revenue"``, ``"order"``), and ``AS`` before an alias is optional. ``WHERE`` takes one predicate and filters the rows the metrics aggregate (see :ref:`ref-sv-pre-agg-filtering`).

The clauses can come in any order, each at most once, and the order you write them in is the order of the columns. The one exception is ``VARIABLES``, which comes last (see :ref:`ref-sv-variables`):

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop METRICS revenue AS total DIMENSIONS region);

   -- is the same query as
   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue AS total'],
       column_order := ['metrics', 'dimensions']
   );

The construct stands wherever a table can: in a join, a CTE, a subquery, a ``LATERAL`` join or a view body, with a table alias and column aliases:

.. code-block:: sql

   -- targets(r VARCHAR, goal DECIMAL) is a table of your own
   SELECT t.r, t.goal, s.rev
   FROM targets t
   JOIN SEMANTIC_VIEW(shop DIMENSIONS region METRICS revenue) AS s(r, rev)
     ON s.r = t.r;

A DuckDB view over the construct stores the ``semantic_view()`` call, so it expands against the semantic view's current definition each time it is queried.

A prepared statement's parameter (``?``, ``$1`` or ``$name``) in the construct's ``WHERE`` is one of the statement's parameters, numbered in the order it is written along with the rest of the statement:

.. code-block:: sql

   PREPARE by_region AS
   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS region
       METRICS revenue
       WHERE ordered_at >= ? AND region = ?
   );

   EXECUTE by_region(DATE '2024-01-01', 'East');

It lowers to ``where_clause := 'ordered_at >= $1 AND region = $2'`` with the parameters in ``params``, so each value reaches the predicate as a value, never as SQL text. A value in ``VARIABLES`` can be a parameter too (see :ref:`ref-sv-variables`). A parameter anywhere else in the construct is refused.

.. versionadded:: 0.16.0


.. _ref-sv-modes:

Query Modes
===========

A query works in one of four modes, depending on which lists it gives. The examples query the ``shop`` view defined under :ref:`ref-sv-examples`.

**Dimensions + Metrics** (grouped aggregation):

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS region
       METRICS revenue, order_count
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

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS region
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

   SELECT * FROM SEMANTIC_VIEW(shop
       METRICS revenue, order_count
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

   SELECT * FROM SEMANTIC_VIEW(shop
       FACTS net_price, tax_amount
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

Returns one row per source row with the requested fact expressions as columns. No aggregation or GROUP BY is applied. A dimension listed in ``FACTS`` is read row by row too, as a column of the same rows.


.. _ref-sv-wildcard:

Wildcard Selection
==================

``DIMENSIONS``, ``METRICS`` and ``FACTS``, and the three lists of ``semantic_view()``, accept ``alias.*`` patterns that expand to all items scoped to the specified table alias. Items declared without a table alias, such as derived metrics, count as belonging to the first table in ``TABLES``, so ``o.*`` on a view whose first table is ``o`` also returns its public derived metrics:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS o.*
       METRICS o.*
   );

``PRIVATE`` metrics and facts are excluded from wildcard expansion. Bare ``*`` (unqualified) is not supported -- all wildcards must be qualified with a table alias.

When an item appears both explicitly and via wildcard expansion, it appears only once in the result (deduplication).


.. _ref-sv-output:

Output
======

Returns a result set with one column per requested dimension, metric, or fact. In the construct, the columns come in the order of its clauses, and within a clause in the order written. In ``semantic_view()``, dimensions come first, then metrics or facts, each in the order requested; ``column_order`` changes which list's columns come first, as in ``column_order := ['metrics', 'dimensions']``.

A column is named after its member, or after the alias the item gives it:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       METRICS revenue AS "Total revenue"
       DIMENSIONS region AS r
   );

   -- the same query with semantic_view()
   SELECT * FROM semantic_view('shop',
       dimensions := ['region AS r'],
       metrics := ['revenue AS "Total revenue"'],
       column_order := ['metrics', 'dimensions']
   );

The alias is one identifier, quoted or not, or a string (``AS 'r'``). Two columns with the same name are an error, and so is an alias on a wildcard. In a ``semantic_view()`` item the text before ``AS`` is the member, so a member whose name contains the word ``as`` on its own (``Sales as of``) has to be quoted there: ``'"Sales as of"'``.

.. versionadded:: 0.16.0

   Aliases in items and the ``column_order`` parameter.

DuckDB plans the generated SQL as part of your query, so the column types are the ones DuckDB gives the generated SQL: ``SUM`` over an integer column is a ``HUGEINT``, and an ``ENUM`` or ``STRUCT`` column keeps its type. Nothing is cached at ``CREATE``.

Because it is part of your query, a semantic-view query reads your open transaction, including rows you have not committed yet, and DuckDB can push your query's filters and column choices down into it. The ``WHERE`` predicate can refer to a column of an enclosing query, as in a ``LATERAL`` join:

.. code-block:: sql

   SELECT t.r, s.revenue
   FROM targets t,
        LATERAL (SELECT * FROM SEMANTIC_VIEW(shop
                    METRICS revenue
                    WHERE region = t.r)) s;

If the generated SQL does not bind, for example because a metric names a column that has since been dropped, the query fails with DuckDB's own error. Its caret points at the ``semantic_view(...)`` call, or, for the construct, at the start of the query. Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to see the generated SQL.

.. versionchanged:: 0.16.0

   Column types are DuckDB's own. Earlier releases returned a ``HUGEINT`` as ``BIGINT``, a ``UHUGEINT`` as ``UBIGINT``, and ``ENUM``, ``STRUCT`` and ``MAP`` columns as ``VARCHAR``. Errors in the generated SQL used to be wrapped as ``semantic_view: SQL execution failed: ...`` or ``semantic_view: type inference failed for query ...``.

.. versionchanged:: 0.11.0

   A dimension, metric, or fact declared with a double-quoted name (e.g.
   ``"order date"``) now produces an output column named by its **logical
   value** (``order date``), with the quote characters stripped. Previously the
   column was named ``"order date"`` -- quote characters included. A consumer
   selecting the old quote-laden column name must update. Queries over unquoted
   names are unaffected.


.. _ref-sv-expression-items:

Expression Items
================

An item can be an expression over the view's members instead of a member's name, as in Snowflake. The expansion treats it as a member it adds for this query alone, so it is grouped, joined and checked for fan traps exactly as a member declared on its table would be:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS DATE_PART('year', ordered_at) AS year
       METRICS revenue / order_count AS avg_order, max(o.net_price) AS biggest
   );

What an expression may use depends on its list:

- In ``DIMENSIONS`` and ``FACTS``, an expression uses dimensions and facts, and all of them must belong to one table. It is a row-level value, so it holds no aggregate and no metric.
- In ``METRICS``, an expression uses metrics, and aggregates of dimensions and facts. Each aggregate is computed as a metric of its table, so its dimensions and facts must belong to one table; ``count(DISTINCT c.region)`` counts customers over the customer table, whatever else the query asks for. A dimension or fact outside an aggregate is refused, and so is an aggregate of a metric.

A member is named in an expression as it is as an item: by its name, or qualified by its table, where a member declared without a table is on the base table. A field of a ``STRUCT`` dimension or fact is read from it, so ``upper(o.address.city)``, or the item ``address.city``, is the field ``city`` of the dimension ``address``.

An expression reads only the view's members and its :ref:`variables <ref-sv-variables>`. Any other name is refused, so an item cannot read a column of a table the query joins that the view does not declare, such as the column under a ``PRIVATE`` fact. Keywords, numbers, a lambda's parameters (``lambda x: x * 2``), named arguments and collations are not names of this kind. A positional reference (``#2``), ``COLUMNS(...)`` and a method call (``x.abs()``; write ``abs(x)``) are refused with them. ``current_date`` and the other functions SQL calls without parentheses are called as functions, never read as a column of the same name. The ``WHERE`` clause differs: it can still filter on such a column (see :ref:`ref-sv-filtering`).

This is a check of the expression as written, not access control, which DuckDB does not have: the body of a macro the expression calls is not checked.

No item holds a subquery, a window function (declare a :ref:`window metric <howto-window-metrics>` instead) or a prepared statement parameter, and a ``PRIVATE`` fact or metric cannot be used, as anywhere else in a query. An item is one expression: separate two with a comma, in two items.

Without an alias, the column is named by the expression as written, so ``DIMENSIONS lower(region)`` gives a column named ``lower(region)``. An error about an expression item, such as a fan trap, names the expression as written.

.. versionadded:: 0.16.0


.. _ref-sv-filtering:

Filtering
=========

There are two filters, and they run on opposite sides of the aggregation.

Post-Aggregation -- Outer ``WHERE``
-----------------------------------

Use standard SQL ``WHERE`` on the outer query to filter the rows the query returns:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS region
       METRICS revenue
   ) WHERE region = 'East';

The ``WHERE`` clause applies to the result set after the semantic view expansion. DuckDB's optimizer pushes predicates down into the generated query where possible.


.. _ref-sv-pre-agg-filtering:

Pre-Aggregation -- ``WHERE`` in the Construct, or ``where_clause``
------------------------------------------------------------------

A ``WHERE`` inside ``SEMANTIC_VIEW(...)`` filters the rows the metrics aggregate **over**, before they are aggregated, as in Snowflake. In ``semantic_view()`` the same predicate is the ``where_clause := '<predicate>'`` parameter:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS region
       METRICS revenue
       WHERE ordered_at >= DATE '2024-01-01'
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬──────────┐
   │ region │ revenue  │
   ├────────┼──────────┤
   │ East   │ 250.0000 │
   │ West   │ 330.0000 │
   └────────┴──────────┘

Each region's ``revenue`` is recomputed over only the matching orders: East loses its December 2023 order and falls from 340.0000 to 250.0000. An outer ``WHERE`` cannot express this: by then the aggregation has already run over every row, and ``ordered_at`` is not in the output to filter by.

With ``semantic_view()``, the predicate is a string, so a quote inside it is doubled:

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue'],
       where_clause := 'ordered_at >= DATE ''2024-01-01'''
   ) ORDER BY region;

The rules below apply to both spellings.

.. _ref-sv-where-params:

**Values from params.** In the construct, a value supplied at run time is a prepared-statement parameter in the ``WHERE`` (see :ref:`ref-sv-construct`). With ``semantic_view()``, write ``$1``, ``$2``, ... in the predicate and pass the values in ``params``, a row whose fields are taken in order:

.. code-block:: sql

   SELECT * FROM semantic_view('shop',
       dimensions := ['region'],
       metrics := ['revenue'],
       where_clause := 'ordered_at >= $1 AND region <> $2',
       params := row(DATE '2024-01-01', 'West')
   );

Each value goes into the predicate as a constant of its own type, never as SQL text, so a value holding a quote is just a string. A value can itself be a prepared-statement parameter (``params := row(?, ?)``), which is what the ``SEMANTIC_VIEW(...)`` construct passes. A reference with no value in ``params`` is an error, and so is any other parameter in the predicate (``?``, ``$name``): it would be one of the calling statement's own parameters.

.. versionadded:: 0.16.0

The parameter is spelled ``where_clause`` rather than ``where`` because DuckDB reserves ``where`` in named-parameter position -- ``where := '...'`` is a parse error before the extension is consulted.

**What the predicate may name.** Declared dimensions and facts, referenced by their logical names; each is substituted to its expression, wrapped in parentheses so a member that binds looser than its surrounding context keeps its grouping. Members declared :ref:`LABELS = (FILTER) <howto-annotations-filters>` are the intended case, but any dimension or fact works -- the label records intent and does not gate resolution. Naming a **metric** is rejected, matching Snowflake: the filter runs before aggregation, so an aggregate has no value yet. The bare name of one of the view's :ref:`variables <ref-sv-variables>` is the variable's value, as in ``WHERE region = target_region``.

A name that is not a declared dimension or fact is left as written. It binds only if it is a column of a table the query already reads (``price`` or ``o.price`` on a single-table view, for example); it does not cause a join, so a column of any other table fails with DuckDB's ``Referenced column ... not found`` binder error. Declare the column as a dimension or fact to make it reachable.

**Subqueries.** A subquery in the predicate is uncorrelated, as in Snowflake. Names inside it are its own tables' columns, never members, and it sees none of the semantic view's tables. In ``region IN (SELECT code FROM codes)``, ``region`` is the member and ``code`` is a column of ``codes``. A name the subquery's tables lack, such as ``region`` in ``EXISTS (SELECT 1 FROM codes WHERE code = region)``, fails with DuckDB's ``Referenced column ... not found`` error. Write the condition on the member outside the subquery.

.. versionchanged:: 0.16.0

   A member name inside a subquery used to be replaced with the member's expression, and a name the subquery's tables lacked could read a column of the semantic view's tables.

**Joins and fan-out.** Tables the predicate reaches are joined in and subjected to the same reachability and fan-out checks as a queried dimension's, matching Snowflake's rule that WHERE-clause members participate in the same-logical-table constraint. A member on a role-played table -- one reached through two named relationships -- is rejected rather than bound to whichever relationship was declared first, since the predicate has no way to say which role is meant.

**Where it is applied.** Before the ``GROUP BY`` when the query is a single ``SELECT`` (one grain, dimensions only, or facts), inside each grain CTE for multi-grain queries, inside the snapshot CTE ahead of the ranking for semi-additive metrics, and inside the aggregate CTE ahead of the window function for window metrics -- so a filtered number is always recomputed rather than filtered after the fact.

An omitted, empty, or whitespace-only ``where_clause`` is treated as absent. The two filters compose: a pre-aggregation predicate and an outer ``WHERE`` in the same query each do their own job.

.. warning::

   The predicate is SQL that runs with the caller's privileges, and it may contain anything a ``WHERE`` clause can, including subqueries over other tables. Never build it from request input. In the construct, bind each user-supplied value as a ``?`` parameter. With ``semantic_view()``, passing the predicate as a prepared-statement parameter (``where_clause := ?``) saves you quoting the string, but it does **not** make untrusted text safe. Write the predicate yourself and pass each user-supplied value in ``params``, where it is a value and never SQL (see :ref:`values from params <ref-sv-where-params>`), and prefer predicates made of :ref:`named filters <howto-annotations-filters>` that you declared, so that user input chooses which filter applies rather than supplying SQL.


.. _ref-sv-variables:

Variables
=========

.. versionadded:: 0.18.0

When a view declares :ref:`VARIABLES <ref-create-variables>`, each query can give them values. In the construct, set them in a ``VARIABLES`` clause, which comes last, after ``WHERE``; ``name = value`` and ``name => value`` mean the same. In ``semantic_view()``, pass them by name in the ``variables`` struct. These examples query the ``sales_analysis`` view from :ref:`ref-create-variables`:

.. code-block:: duckdb-sql

   SELECT * FROM SEMANTIC_VIEW(
       sales_analysis
       DIMENSIONS region
       METRICS weighted_sales
       VARIABLES target_region = '', multiplier = 3
   ) ORDER BY ALL;

   -- the same query with semantic_view()
   SELECT * FROM semantic_view('sales_analysis',
       dimensions := ['region'],
       metrics := ['weighted_sales'],
       variables := {'target_region': '', 'multiplier': 3}
   ) ORDER BY ALL;

.. code-block:: text

   ┌────────┬────────────────┐
   │ region │ weighted_sales │
   ├────────┼────────────────┤
   │ East   │        10500.0 │
   │ West   │        10500.0 │
   └────────┴────────────────┘

``target_region`` has no default, so the query sets it even though ``weighted_sales`` does not read it. ``min_amount`` is left unset and takes its default.

- **Unset variables.** A variable the query leaves unset takes its default, which the query evaluates. A variable with no default must be set by every query of the view, even one that reads no member that uses it, as in Snowflake.
- **Unknown names.** Setting a variable the view does not declare is an error, which suggests the closest declared name.
- **Values.** A value is cast to the variable's type, when it has one, and reaches the query as a value, never as SQL text. A value can be any expression a table function argument can hold, such as ``DATE '2000-01-01'``, but not a query: ``VARIABLES target_region = (SELECT ...)`` is refused.
- **Where variables are read.** Members read them, and so do the query's ``WHERE`` and its :ref:`expression items <ref-sv-expression-items>`, by bare name. A qualified name is a column, never a variable.
- **Materializations.** A materialization that covers a member reading a variable is not used, whatever values the query sets, because its table was computed with values the query may not have. Other materializations still route. See :ref:`ref-create-materializations`.
- **Standard SQL.** A :ref:`standard-SQL query <howto-standard-sql>` over the view cannot set variables, so each takes its default, and a view with a variable that has no default cannot be queried that way.

An expression item reads a variable like a member does:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(
       sales_analysis
       DIMENSIONS region
       METRICS total_sales * multiplier AS w
       VARIABLES target_region = '', multiplier = 2
   ) ORDER BY ALL;

.. code-block:: text

   ┌────────┬────────┐
   │ region │   w    │
   ├────────┼────────┤
   │ East   │ 7000.0 │
   │ West   │ 7000.0 │
   └────────┴────────┘

A value can be a prepared-statement parameter, numbered with the statement's other parameters. ``VARIABLES`` comes after ``WHERE``, so its parameters come after the ones in ``WHERE``:

.. code-block:: sql

   PREPARE weighted AS
   SELECT * FROM SEMANTIC_VIEW(sales_analysis
       METRICS weighted_sales
       WHERE region = ?
       VARIABLES target_region = '', multiplier = ?
   );

   EXECUTE weighted('West', 3);   -- 10500.0
   EXECUTE weighted('East', 0.5); -- 1750.0

To see how a query's variables reach the generated SQL, pass the same ``variables`` to :ref:`explain_semantic_view() <ref-explain-semantic-view>`.


.. _ref-sv-ordering:

Ordering and Limiting
=====================

Use standard SQL ``ORDER BY`` and ``LIMIT`` on the outer query:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS region
       METRICS revenue
   ) ORDER BY revenue DESC
   LIMIT 10;


.. _ref-sv-time-limits:

Time Limits
===========

.. versionadded:: 0.15.0

DuckDB 2.0's per-query time limit, ``SET max_execution_time`` (in milliseconds), applies to ``semantic_view()`` the same way it applies to any other query. A query that runs past the limit stops with DuckDB's own error:

.. code-block:: sql

   SET max_execution_time = 5000;  -- 5 seconds per query

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS region
       METRICS revenue
   );

.. code-block:: text

   INTERRUPT Error: Query exceeded maximum execution time

The limit also covers the commands that report member data types, which type the members by planning queries against the view's tables and stop between them once the limit has passed: :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`, :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>`, :ref:`SHOW SEMANTIC DIMENSIONS <ref-show-semantic-dimensions>`, :ref:`SHOW SEMANTIC METRICS <ref-show-semantic-metrics>`, :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>`, and :ref:`SHOW SEMANTIC DIMENSIONS FOR METRIC <ref-show-dims-for-metric>`. ``RESET max_execution_time`` removes the limit. See :ref:`ref-err-time-limit`.

As with DuckDB's own queries, the limit counts from when the query starts running. Time DuckDB spends planning a source that is slow to bind, such as a ``read_parquet`` over a long list of files, is not limited.


.. _ref-sv-name-resolution:

Name Resolution
===============

Dimension, metric, and fact names are resolved case-insensitively, following DuckDB's identifier semantics: matching ignores case whether the reference is written unquoted (``region``, ``REGION``) or double-quoted (``"Region"``), in the construct or as a string in a ``semantic_view()`` list (``'region'``, ``'"Region"'``) -- DuckDB treats double-quoted identifiers as case-insensitive too, so quoting a reference only lets it carry whitespace or special characters, it does not make it case-sensitive. Names can optionally be table-qualified (e.g., ``o.region``), which matches against the ``source_table`` alias of the dimension, metric, or fact.

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

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS customer, product
       METRICS revenue, order_count
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

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS customer
       METRICS revenue
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

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS customer
       METRICS revenue
       WHERE ordered_at >= DATE '2024-01-01'
   ) ORDER BY customer;

.. code-block:: text

   ┌──────────┬──────────┐
   │ customer │ revenue  │
   ├──────────┼──────────┤
   │ Acme     │ 250.0000 │
   │ Globex   │  60.0000 │
   │ Initech  │ 270.0000 │
   └──────────┴──────────┘

**Dimensions read as facts (row-level):**

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       FACTS region, net_price
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

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS o.*
       METRICS o.*
   );

Returns every dimension and public metric on ``o``: ``customer``, ``product``, ``region``, ``ordered_at``, ``revenue``, and ``order_count``.

**Bind values from application code:**

.. code-block:: python

   import datetime

   import duckdb

   con = duckdb.connect()
   con.execute("LOAD semantic_views")
   # ... create the orders table and the shop view shown above ...

   rows = con.execute(
       """
       SELECT * FROM SEMANTIC_VIEW(shop
           DIMENSIONS region
           METRICS revenue
           WHERE ordered_at >= ?
       ) ORDER BY region
       """,
       [datetime.date(2024, 1, 1)],
   ).fetchall()
   print(rows)
   # [('East', Decimal('250.0000')), ('West', Decimal('330.0000'))]

The date is bound as a parameter of the statement, so it reaches the predicate as a value, never as SQL text.

To choose the dimensions or metrics at run time, use ``semantic_view()``, whose lists can be bound too:

.. code-block:: python

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
