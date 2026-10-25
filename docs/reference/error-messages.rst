.. meta::
   :description: Reference for all error messages produced by the extension, with causes and fixes for syntax, DDL, and query errors

.. _ref-error-messages:

==============
Error Messages
==============

This page documents the error messages produced by DuckDB Semantic Views, their causes, and how to resolve them.


.. _ref-err-load:

Load Errors
===========

These errors occur when ``LOAD semantic_views`` runs.


.. _ref-err-load-version:

Built for a different DuckDB release
------------------------------------

.. versionchanged:: 0.15.0

   This release is built for DuckDB 2.0. Earlier releases were built for
   DuckDB 1.5, and v0.14.2 is the latest release for 1.5.

.. code-block:: text

   semantic_views was built for DuckDB <built> and cannot run in DuckDB <host>,
   because it depends on that exact DuckDB release. If a semantic_views build
   for this DuckDB has been published, install it with: FORCE INSTALL
   semantic_views FROM community;

DuckDB prefixes the message with ``An error was thrown during initialization of the extension '<path>':``.

**Cause:** Each build of the extension is compiled against one exact DuckDB release. The file being loaded was built for ``<built>``, but the running DuckDB is ``<host>``. The extension checks this before it starts, so the ``LOAD`` fails instead of crashing DuckDB. A common source is an extension file installed before a DuckDB upgrade or downgrade.

**Fix:** Run ``SELECT version();`` to see which DuckDB you are running, then install the build that matches it and load it. ``FORCE INSTALL`` replaces the installed file even when one is already present:

.. code-block:: sql

   FORCE INSTALL semantic_views FROM community;
   LOAD semantic_views;

If no build for ``<host>`` has been published yet, the install fails. Until one is, run DuckDB ``<built>``. The latest release for DuckDB 1.5 is v0.14.2.

When ``<host>`` is a development or nightly DuckDB build (its version contains a ``-``, such as ``v2.0.1-dev42``), the message ends with ``Load a semantic_views build compiled against this DuckDB.`` instead. The community repository only has builds for tagged releases, so build the extension from source against that DuckDB.


.. _ref-err-ddl:

DDL Errors (CREATE SEMANTIC VIEW)
=================================

These errors occur at define time when creating or replacing a semantic view.


Expected AS or FROM YAML
-------------------------

.. code-block:: text

   Expected 'AS' or 'FROM YAML' after view name. Use: CREATE SEMANTIC VIEW
   name AS TABLES (...) DIMENSIONS (...) METRICS (...) or: ...

**Cause:** The statement has no view name (``CREATE SEMANTIC VIEW AS ...`` reads ``AS`` as the name), or has a view name but is missing the ``AS`` keyword or ``FROM YAML`` keywords before the body.

**Fix:** Use either the keyword body (``CREATE SEMANTIC VIEW my_view AS TABLES (...)``) or the YAML body (``CREATE SEMANTIC VIEW my_view FROM YAML $$ ... $$``).


Missing TABLES clause
---------------------

.. code-block:: text

   Missing required clause 'TABLES'.

**Cause:** The body does not include a ``TABLES`` clause.

**Fix:** Add a ``TABLES`` clause as the first clause in the body.


No DIMENSIONS or METRICS
-------------------------

.. code-block:: text

   At least one of 'DIMENSIONS' or 'METRICS' is required.

**Cause:** The body has a ``TABLES`` clause but neither ``DIMENSIONS`` nor ``METRICS``.

**Fix:** Add at least one ``DIMENSIONS`` or ``METRICS`` clause.


No tables in TABLES
-------------------

.. code-block:: text

   A semantic view needs at least one table in TABLES.

**Cause:** The ``TABLES`` clause is empty: ``TABLES ()``.

**Fix:** List the view's tables in ``TABLES``, for example ``TABLES (o AS orders PRIMARY KEY (id))``.


No dimension or metric
----------------------

.. code-block:: text

   A semantic view needs at least one dimension or metric.

**Cause:** The ``DIMENSIONS`` and ``METRICS`` clauses that the body has are all empty, as in ``DIMENSIONS () METRICS ()`` or a lone ``METRICS ()``. Facts alone do not count: a view is queried through its dimensions and metrics.

**Fix:** Declare at least one dimension or metric.


Unknown clause keyword
----------------------

.. code-block:: text

   Unknown clause keyword '<word>'; did you mean '<KEYWORD>'?

**Cause:** A word in the body does not match any known clause keyword (TABLES, RELATIONSHIPS, FACTS, DIMENSIONS, METRICS, MATERIALIZATIONS).

**Fix:** Check spelling. The error suggests the closest valid keyword.


Duplicate clause
----------------

.. code-block:: text

   Duplicate clause keyword '<KEYWORD>'.

**Cause:** The same clause keyword appears more than once.

**Fix:** Combine entries into a single clause.


Clause out of order
-------------------

.. code-block:: text

   Clause '<KEYWORD>' appears out of order; clauses must appear as: TABLES,
   RELATIONSHIPS (optional), FACTS (optional), DIMENSIONS (optional),
   METRICS (optional), MATERIALIZATIONS (optional).

**Cause:** Clauses are not in the required order.

**Fix:** Reorder clauses to: TABLES, RELATIONSHIPS, FACTS, DIMENSIONS, METRICS, MATERIALIZATIONS.


Unclosed parenthesis
--------------------

.. code-block:: text

   Unclosed '(' in clause '<KEYWORD>'.

**Cause:** A clause's opening parenthesis has no matching closing parenthesis.

**Fix:** Check for mismatched parentheses in the clause body.


Parser strictness
-----------------

.. versionchanged:: 0.11.0

   Several malformed statements that earlier releases silently accepted (dropping
   or mis-storing part of the input) are now rejected. If a statement that used
   to "work" begins to error after upgrading, it was almost certainly one of
   these:

- **Stray or leading commas** in a clause list are rejected -- ``DIMENSIONS (a AS x,, b AS y)`` and ``TABLES (,o AS orders ...)`` no longer silently drop an entry. A single *trailing* comma (``METRICS (a AS ..., )``) is still tolerated.
- **Malformed identifier slots** are rejected instead of being stored as unqueryable names: a whitespace-separated multi-token name (``o.d junk AS ...``, which used to name the dimension ``d junk``) and an empty quoted identifier ``""`` in a name or alias slot now error. An unqualified entry name whose expression happens to contain a dot (``region AS upper(o.region)``) now reports the missing ``alias.name`` qualifier rather than a misleading "Expected 'AS'".
- **Trailing tokens** after the view name on a name-only statement (``DROP`` / ``DESCRIBE`` / :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>`) are rejected -- ``DROP SEMANTIC VIEW a b c`` no longer executes and silently discards the extra tokens. ``ALTER`` sub-operations do the same.
- **DDL prefix keywords require a word boundary** -- ``DROP SEMANTIC VIEWS`` (plural typo) no longer drops a view named ``s``, and ``CREATE SEMANTIC VIEWfoo`` is no longer recognized as ``CREATE SEMANTIC VIEW``.


.. _ref-err-quoted-identifier:

Malformed quoted identifier
---------------------------

.. versionchanged:: 0.15.0

   DuckDB 2.0 rejects these statements before the extension sees them, so the
   messages are DuckDB's own. Earlier releases reported
   ``Unterminated quoted identifier`` and ``empty quoted identifier``.

.. code-block:: text

   Parser Error: unterminated quoted identifier

   Parser Error: zero-length delimited identifier

**Cause:** A double-quoted identifier in a semantic-view statement has no closing ``"`` (for example ``ORDER BY "unclosed ASC`` in a window metric, or ``SHOW SEMANTIC VIEWS IN SCHEMA "oops``), or is empty (``TABLES ("" AS orders ...)``).

**Fix:** Close the quote, or give the identifier a name. Quote only names that need it (spaces, special characters, reserved words): DuckDB matches identifiers case-insensitively whether they are quoted or not, so quoting does not change which name matches.


.. _ref-err-syntax:
.. _ref-err-one-statement:

Syntax errors in semantic-view statements
-----------------------------------------

.. versionchanged:: 0.16.0

   A statement the grammar cannot match gets DuckDB's own syntax error. Earlier
   releases answered some of them with the extension's messages, such as
   ``Did you mean 'CREATE SEMANTIC VIEW'?``, ``Missing view name after DDL
   prefix.`` or ``semantic-view DDL must be sent as its own statement``.

.. code-block:: text

   Parser Error: syntax error at or near "CREAT"

   LINE 1: CREAT SEMANTIC VIEW sales AS ...
           ^^^^^

**Cause:** The semantic-view statements are part of DuckDB's grammar, so a statement that does not match any of them is a syntax error at the first token that does not fit, as for DuckDB's own SQL. Common causes:

- A misspelt keyword: ``CREAT SEMANTIC VIEW``, ``DROP SEMANTC VIEW``.
- A missing view name, or an ``ALTER SEMANTIC VIEW`` with no operation.
- A view name that is a DuckDB reserved word written without quotes, such as ``CREATE SEMANTIC VIEW select AS ...`` or ``DESCRIBE SEMANTIC VIEW order``. DuckDB accepts its reserved keywords, and most of its ``type_function`` keywords such as ``left`` and ``join``, as a name only when it is quoted; ``duckdb_keywords()`` lists both kinds.
- A connection that was already open when another connection ran ``LOAD semantic_views``. It parses without the semantic-view statements until it runs a statement DuckDB parses on its own, so retrying the failed statement fails again (see :ref:`explanation-txn-ddl-peg`).

**Fix:** Correct the token the pointer marks. Quote a reserved-word name (``CREATE SEMANTIC VIEW "select" AS ...``); ``GET_DDL`` quotes such names for you. On a connection opened before the ``LOAD``, run ``LOAD semantic_views`` (or any other statement, such as ``SELECT 1``) on it first, then the semantic-view statement.


.. _ref-err-name-uniqueness:

Duplicate or colliding names
----------------------------

.. code-block:: text

   duplicate name '<name>': <kind> '<name>' collides with <kind> '<other>'
   -- dimension, metric, and fact names share one namespace and are
   case-insensitive

**Cause:** Two items in the same semantic view share a name. Dimensions, metrics, and facts are resolved from one namespace at query time, so the collision may be within a kind (two metrics named ``revenue``) or across kinds (a dimension and a metric both named ``amount``). The comparison is case-insensitive, so ``region`` and ``Region`` collide.

**Fix:** Rename one of the items so that every dimension, metric, and fact name is unique (ignoring case).

.. code-block:: sql

   -- This will fail: "duplicate name 'Region': dimension 'Region' collides
   -- with dimension 'region' ..."
   DIMENSIONS (
       o.region AS o.region,
       c.Region AS c.sales_region
   )

   -- Fix: use distinct names
   DIMENSIONS (
       o.order_region AS o.region,
       c.customer_region AS c.sales_region
   )

.. code-block:: sql

   -- This will fail: "duplicate name 'Amount': metric 'Amount' collides
   -- with dimension 'amount' ..."
   DIMENSIONS (
       o.amount AS o.amount_category
   )
   METRICS (
       o.Amount AS SUM(o.amount)
   )

   -- Fix: use a distinct name for the metric
   DIMENSIONS (
       o.amount AS o.amount_category
   )
   METRICS (
       o.total_amount AS SUM(o.amount)
   )

.. note::

   The name uniqueness check runs at ``CREATE`` (and ``ALTER``) time, before the view definition is persisted. It is independent of the query-time duplicate checks described in :ref:`the next section <ref-err-query>`, which catch the same dimension or metric being requested twice in a single ``semantic_view()`` call. Semantic views created before this check existed keep working at query time: lookups resolve to the first declaration.


Graph validation errors
-----------------------

.. versionchanged:: 0.14.0

   ``CREATE`` now also rejects a table that cannot be reached from the base
   table (whether or not the view has a ``RELATIONSHIPS`` clause), a YAML
   relationship without ``fk_columns``, two relationships with the same name,
   and a member qualified by an undeclared alias in a single-table view. Each of
   these used to be accepted and failed later, at query time, or silently used
   the first relationship with a repeated name. Views stored by earlier versions
   keep loading.

.. code-block:: text

   table '<alias>' cannot reference itself

   cycle detected in relationships: <alias1> -> <alias2> -> <alias1>

   diamond: '<alias>' is reachable from multiple tables ('<from1>', '<from2>');
   the join path is ambiguous. Declare the target under a second table alias
   so it is joined once per path.

   orphan table '<alias>' is not connected by any relationship; did you mean '<suggestion>'?

   base table '<alias>' (the first table in TABLES) is not connected by any
   relationship to the other tables ('<alias>', ...); declare a relationship that
   reaches it, or list a connected table first

   table '<alias>' is not reachable from the base table '<alias>' (the first table
   in TABLES) through any chain of relationships; unreachable: '<alias>', ...

   relationship '<name>' from '<alias>' to '<alias>' declares no foreign-key columns;
   the form is 'rel_name AS from_alias(fk_cols) REFERENCES to_alias'

   duplicate relationship name '<name>': '<name>' (<from> -> <to>) and '<other>'
   (<from> -> <to>) name the same relationship; relationship names must be unique
   within a semantic view (compared case-insensitively, quoted or not)

   relationships '<name>' and '<other>' to '<alias>' would both be joined as
   '<alias>__<name>'; role-playing relationships to one table need names that differ
   once quotes are removed and case is folded

   unknown source table '<alias>'; did you mean '<suggestion>'?

**Cause:** The relationship graph violates tree structure requirements.

**Fix:**

- **Self-reference:** A table cannot have a relationship pointing to itself.
- **Cycle:** Follow the chain in the error message to find the circular dependency and remove it.
- **Diamond:** One target table is reached from two different source tables. Naming the relationships does not help; declare the shared table a second time under another alias (for example ``s_a AS stores`` and ``s_b AS stores``) so each path has its own copy. Several relationships from the *same* source table to one target are allowed; that is the role-playing pattern (see :ref:`howto-role-playing`).
- **Orphan:** Add a relationship that connects the table, or remove it from ``TABLES``.
- **Not connected / not reachable:** Every table must be reachable from the first table in ``TABLES``. Add the missing relationship, or remove the table. A view that lists several tables but no ``RELATIONSHIPS`` at all gets the base-table form of this error.
- **No foreign-key columns:** A YAML ``joins`` entry needs ``fk_columns`` (the DDL form always has them).
- **Duplicate relationship name:** Two relationships share a name. Names are compared the way DuckDB compares identifiers, so ``route``, ``ROUTE`` and ``"route"`` are one name, whichever tables the relationships connect. Rename one of them. Before this check, ``USING`` silently used the first relationship with the name. Views stored before the check keep loading and keep that behaviour. A quoted name containing a dot (``"x.y"``) is one identifier, a different name from the two-part ``x.y``.
- **Role-playing names sharing an alias:** Two relationships from one table to the same target are joined under ``<target>__<name>``, with the name's quotes removed and its case folded. ``x.y`` and ``"x.y"`` are different names but would get the same alias. Rename one of them.
- **Unknown source table:** A dimension or metric is qualified by a table alias that ``TABLES`` does not declare, such as ``x.foo`` when the only table is ``o``. This applies to single-table views too. Qualify the member with a declared alias, in any case. Write the alias unquoted: a quoted qualifier such as ``"O"`` for alias ``o`` is accepted only in a single-table view, and a view with several tables rejects it with this error (a known limitation; DuckDB itself treats ``"O"`` and ``o`` as the same identifier).


Key missing on a relationship target
------------------------------------

.. code-block:: text

   Table '<target>' has no PRIMARY KEY declared but is referenced by FK in
   '<from>'. Add PRIMARY KEY (cols) or UNIQUE (cols) to the TABLES clause for
   <target>. (v0.10.0: physical-catalog PK auto-inference removed -- see
   CHANGELOG.)

**Cause:** A relationship references a table whose ``TABLES`` entry declares neither ``PRIMARY KEY`` nor ``UNIQUE``. An explicit column list on the relationship (``REFERENCES <target>(<cols>)``) does not satisfy the requirement.

**Fix:** Declare the key on the target table's ``TABLES`` entry: ``c AS customers PRIMARY KEY (id)``, or ``UNIQUE (email)`` when the relationship joins on a unique column.


Expression references another table's column
--------------------------------------------

.. code-block:: text

   semantic view: fact '<name>' references '<alias>.<column>', a column of
   table '<alias>', but a fact expression may only reference columns of its
   own table ('<own>'). To use a value from another table, define a FACT on
   that table and reference the fact by name (e.g. '<alias>.<fact_name>').

The same message is raised for a dimension or metric expression, with ``fact`` replaced by the member kind.

**Cause:** A fact, dimension, or metric expression names a raw column of a different logical table.

**Fix:** Define a fact on the other table and reference that fact by name. See :ref:`howto-facts`.


Unknown metric in a derived metric
----------------------------------

.. code-block:: text

   unknown metric '<name>' referenced in derived metric '<derived>'; did you mean '<suggestion>'?.
   Available metrics: [<list>]

**Cause:** A derived metric's expression names a metric that the view does not declare, often a typo. The ``did you mean`` clause appears only when a declared metric's name is close enough to suggest.

**Fix:** Correct the name. The error lists every metric in the view.


LABELS on a metric
------------------

.. code-block:: text

   LABELS is not valid on a metric; it applies to facts and dimensions.

**Cause:** A ``METRICS`` entry carries ``LABELS = (FILTER)``. Named filters are row-level members, so only facts and dimensions can be labeled.

**Fix:** Remove ``LABELS`` from the metric. To reuse a filter condition, declare it as a fact or dimension with ``LABELS = (FILTER)``. See :ref:`howto-annotations-filters`.


Aggregate in a dimension or fact
--------------------------------

.. code-block:: text

   semantic view: dimension '<name>' must not contain the aggregate function
   '<func>'. A dimension is a row-level value; define a metric for the aggregate.

   semantic view: fact '<name>' must not contain the aggregate function
   '<func>'. A fact is a row-level value; define a metric for the aggregate.

**Cause:** A dimension or fact expression calls an aggregate function such as ``SUM``, ``COUNT``, ``AVG`` or ``arg_max``. A query groups by a dimension, and a metric aggregates a fact, so neither can hold an aggregate of its own.

**Fix:** Move the aggregation to the ``METRICS`` clause. An aggregate inside a subquery (``(SELECT max(t) FROM tiers)``) and a windowed aggregate (``SUM(o.amount) OVER (PARTITION BY o.region)``) are one value per row and are allowed.


Parameter in a member
---------------------

.. code-block:: text

   semantic view: dimension '<name>' holds the prepared statement parameter $1,
   which has no value inside the view's definition. Use parameters in the WHERE
   clause of a SEMANTIC_VIEW(...) query instead.

**Cause:** A dimension, fact or metric expression (or a window metric's arguments, ``ORDER BY`` or frame) holds a prepared statement parameter: ``?``, ``?1``, ``$1`` or ``$name``. A member is expanded inside the view's own query, where no parameter has a value. The message names ``fact`` or ``metric`` for those members.

**Fix:** Write the value into the expression, or filter with a parameter in the query's ``WHERE`` clause. See :ref:`ref-sv-pre-agg-filtering`.


Circular fact or metric references
----------------------------------

.. code-block:: text

   cycle detected in facts: <name1> -> <name2> -> <name1>

   cycle detected in derived metrics: <name1> -> <name2> -> <name1>

**Cause:** Facts or derived metrics reference each other in a cycle.

**Fix:** Break the cycle by removing or restructuring the circular reference.


NON ADDITIVE BY dimension not found
------------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   NON ADDITIVE BY dimension '<dim>' on metric '<name>' does not match any
   declared dimension. Did you mean '<suggestion>'?

**Cause:** A ``NON ADDITIVE BY`` clause references a dimension name that does not exist in the view's ``DIMENSIONS`` clause.

**Fix:** Check the dimension name against the declared dimensions. The error suggests close matches when available.


Window metric inner metric not found
-------------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   Window metric '<name>': inner metric '<inner>' not found in semantic view
   metrics. Did you mean '<suggestion>'?

**Cause:** A window function metric wraps another metric (e.g., ``AVG(total_qty) OVER (...)``), but the inner metric ``total_qty`` does not exist in the ``METRICS`` clause.

**Fix:** Ensure the inner metric is declared before the window metric. The error suggests close matches.


Window metric EXCLUDING dimension not found
--------------------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   Window metric '<name>': EXCLUDING dimension '<dim>' not found in semantic
   view dimensions. Did you mean '<suggestion>'?

**Cause:** A window metric's ``PARTITION BY EXCLUDING`` clause references a dimension that does not exist.

**Fix:** Check the dimension name. The error suggests close matches. All dimensions in ``EXCLUDING`` must be declared in the view's ``DIMENSIONS`` clause.


Window metric PARTITION BY dimension not found
-----------------------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   Window metric '<name>': PARTITION BY dimension '<dim>' not found in semantic
   view dimensions. Did you mean '<suggestion>'?

**Cause:** A window metric's ``PARTITION BY`` clause (without ``EXCLUDING``) references a dimension that does not exist in the view's ``DIMENSIONS`` clause.

**Fix:** Check the dimension name. The error suggests close matches. All dimensions in ``PARTITION BY`` must be declared in the view's ``DIMENSIONS`` clause.


Window metric ORDER BY dimension not found
-------------------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   Window metric '<name>': ORDER BY dimension '<dim>' not found in semantic
   view dimensions. Did you mean '<suggestion>'?

**Cause:** A window metric's ``ORDER BY`` clause references a dimension that does not exist in the view's ``DIMENSIONS`` clause.

**Fix:** Check the dimension name. The error suggests close matches.


OVER and NON ADDITIVE BY conflict
-----------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   Cannot combine OVER clause with NON ADDITIVE BY on metric '<name>'.
   Use one or the other.

**Cause:** A metric definition includes both a window function ``OVER (...)`` clause and a ``NON ADDITIVE BY (...)`` clause. These are mutually exclusive features.

**Fix:** Use either ``NON ADDITIVE BY`` (for semi-additive snapshot aggregation) or ``OVER`` (for window functions), not both on the same metric.


OVER clause not allowed on derived metric
-------------------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   OVER clause not allowed on derived metric '<name>'. Only qualified metrics
   (alias.name) can use OVER.

**Cause:** A derived metric (one without a table alias) has an ``OVER`` clause. Window metrics require a qualified name (``alias.metric_name``) because they need a source table for join resolution.

**Fix:** Add a table alias to the metric name: change ``my_metric AS AVG(total) OVER (...)`` to ``alias.my_metric AS AVG(total) OVER (...)``.


.. _ref-err-materialization:

Materialization Errors
-----------------------

.. versionadded:: 0.7.0

These errors occur when validating the ``MATERIALIZATIONS`` clause.


Duplicate materialization name
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

.. code-block:: text

   Duplicate materialization name '<name>'.

**Cause:** Two or more materializations in the same view share the same name.

**Fix:** Give each materialization a unique name.


Materialization dimension not found
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

.. code-block:: text

   Materialization '<name>': dimension '<dim>' not found in semantic view
   dimensions. Did you mean '<suggestion>'?

**Cause:** A materialization references a dimension name that does not exist in the view's ``DIMENSIONS`` clause.

**Fix:** Check the dimension name against the declared dimensions. The error suggests close matches.


Materialization metric not found
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

.. code-block:: text

   Materialization '<name>': metric '<met>' not found in semantic view
   metrics. Did you mean '<suggestion>'?

**Cause:** A materialization references a metric name that does not exist in the view's ``METRICS`` clause.

**Fix:** Check the metric name against the declared metrics. The error suggests close matches.


Empty materialization coverage
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

.. code-block:: text

   Materialization '<name>': must specify at least one of DIMENSIONS or METRICS.

**Cause:** A materialization entry declares only a ``TABLE`` with neither ``DIMENSIONS`` nor ``METRICS``.

**Fix:** Add at least one of ``DIMENSIONS (...)`` or ``METRICS (...)`` to the materialization entry.


.. _ref-err-yaml:

YAML Errors
============

.. versionadded:: 0.7.0

These errors occur when using ``FROM YAML`` or ``FROM YAML FILE`` to create a semantic view, or when exporting with :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>`.


Dollar-quote errors
--------------------

.. code-block:: text

   Expected '$' to begin dollar-quoted string

   Unterminated dollar-quote opening delimiter

   Unterminated dollar-quoted string (expected closing '<delimiter>')

**Cause:** The ``FROM YAML`` body is not properly enclosed in dollar-quote delimiters.

**Fix:** Ensure the YAML content starts with ``$$`` (or ``$tag$``) and ends with the matching closing delimiter.


Trailing content after dollar-quote
-------------------------------------

.. code-block:: text

   Unexpected content after closing dollar-quote: '<text>'

**Cause:** Extra text appears after the closing ``$$`` delimiter.

**Fix:** Remove any content between the closing ``$$`` and the statement terminator.


Empty file path
----------------

.. code-block:: text

   File path cannot be empty.

**Cause:** ``FROM YAML FILE`` was used with an empty single-quoted string.

**Fix:** Provide a valid file path: ``FROM YAML FILE '/path/to/file.yaml'``


Missing file path quotes
--------------------------

.. code-block:: text

   Expected single-quoted file path after FILE keyword.

**Cause:** The file path after ``FROM YAML FILE`` is not enclosed in single quotes.

**Fix:** Use single quotes around the file path: ``FROM YAML FILE '/path/to/file.yaml'``


YAML size limit exceeded
--------------------------

.. code-block:: text

   YAML definition for semantic view '<name>' exceeds size limit
   (<size> bytes > <cap> bytes).

**Cause:** The YAML content exceeds the 1 MiB (1,048,576 bytes) size cap.

**Fix:** Reduce the YAML definition size, or split the semantic view into multiple smaller views.


YAML parsing error
-------------------

.. code-block:: text

   invalid YAML definition for semantic view '<name>': <details>

For example, a definition without a ``metrics`` key fails with:

.. code-block:: text

   invalid YAML definition for semantic view 'sales': missing field `metrics`

**Cause:** The YAML content is not valid YAML or does not match the expected semantic view definition schema.

**Fix:** Check the YAML syntax and structure against :ref:`ref-yaml-format`. The definition must include ``tables`` and both the ``dimensions`` and ``metrics`` keys (use ``[]`` for one of them if the view has none), and at least one of the two must be non-empty.


.. _ref-err-query:

Query Errors
============

These errors occur at query time, when querying with the :ref:`SEMANTIC_VIEW(...) <ref-sv-construct>` construct, :ref:`semantic_view() <ref-semantic-view-function>` or :ref:`explain_semantic_view() <ref-explain-semantic-view>`. The construct turns into a ``semantic_view()`` call, so a message that names a list such as ``metrics := [...]`` means the clause of the same name, ``METRICS``, when you wrote the construct.


.. _ref-err-construct:

Malformed SEMANTIC_VIEW(...)
----------------------------

.. versionadded:: 0.16.0

.. code-block:: text

   Parser Error: expected DIMENSIONS, METRICS, FACTS or WHERE after the view name in SEMANTIC_VIEW(...), got: ,

   Parser Error: METRICS given twice in SEMANTIC_VIEW(...)

   Parser Error: METRICS needs at least one item

**Cause:** The construct's clauses are not where the grammar expects them. The usual slips are a comma after the view name (the construct has none), a clause written twice, and a clause with no items.

**Fix:** Write the view name, then each clause at most once, each with at least one item: ``SEMANTIC_VIEW(shop DIMENSIONS region METRICS revenue, order_count)``. To add items, extend the one clause rather than repeating it.


Parameter outside WHERE
-----------------------

.. versionadded:: 0.16.0

.. code-block:: text

   Parser Error: prepared statement parameters are only supported in the WHERE clause of SEMANTIC_VIEW(...)

**Cause:** A prepared-statement parameter (``?``, ``$1``, ``$name``) appears in ``DIMENSIONS``, ``METRICS``, ``FACTS`` or the view name of the construct.

**Fix:** Use parameters only in the construct's ``WHERE``. To choose the view, the dimensions or the metrics at run time, use :ref:`semantic_view() <ref-semantic-view-function>`, whose arguments can all be bound.


Two columns with the same name
------------------------------

.. versionadded:: 0.16.0

.. code-block:: text

   semantic view '<view>': two columns of the query are named '<name>'; give one of them another name with AS

**Cause:** Two items produce the same column name, through an alias or because an alias repeats a member's name.

**Fix:** Give one of them another alias.


.. _ref-err-expression-items:

Expression item errors
----------------------

.. versionadded:: 0.16.0

An :ref:`expression item <ref-sv-expression-items>` is checked against the rules for its clause. Each message quotes the expression as you wrote it:

.. code-block:: text

   '<name>' in '<expression>' is not a dimension, fact or metric of semantic view '<view>'; an expression item can only read the view's members

   '<expression>' uses '<a>' and '<b>', which belong to different tables; the dimensions and facts of <item> must belong to one table

   '<expression>' in DIMENSIONS holds the aggregate function '<function>'; an aggregate goes in METRICS

   '<name>' in '<expression>' is a fact; in METRICS a dimension or fact can only be used inside an aggregate

   '<name>' in '<expression>' is a metric; an aggregate in METRICS can only use dimensions and facts

   '<expression>' refers to no dimension or fact; an aggregate in METRICS must use at least one

   '<expression>' holds a window function; a query item cannot, so declare a window metric in the semantic view

   '<expression>' holds a subquery; a query item cannot, only the WHERE clause can

**Cause and fix**, in the same order:

- The expression names a column the view does not declare, such as a raw table column. An expression reads only the view's members. Declare the column as a dimension or fact, or use the member that already wraps it.
- A ``DIMENSIONS`` or ``FACTS`` expression, or one aggregate in ``METRICS``, mixes members of two tables. Its members must belong to one table, so query them as two items.
- An aggregate in ``DIMENSIONS`` or ``FACTS``. Move the item to ``METRICS``.
- A dimension or fact used bare in ``METRICS``. Wrap it in an aggregate (``max(amount)``), or move it to ``DIMENSIONS``.
- An aggregate over a metric, as in ``sum(revenue)``. A metric is already aggregated; use it as it is, or aggregate the fact it is built from.
- An aggregate that names no member, such as ``count(*)``, has no table to be computed over. Declare it as a metric in the view (``o.order_count AS COUNT(*)``) and query that.
- A window function. Declare a :ref:`window metric <howto-window-metrics>`.
- A subquery. Filter with a subquery in ``WHERE`` instead, or declare a member over it.


View not found
--------------

.. code-block:: text

   Semantic view '<name>' not found. Did you mean '<suggestion>'?
   Available views: [<list>].
   Run SHOW SEMANTIC VIEWS to see all registered views.

**Cause:** No semantic view with the given name exists.

**Fix:** Check the view name. The error shows available views and suggests close matches.


Empty request
-------------

.. code-block:: text

   semantic view '<name>': specify at least dimensions := [...], metrics := [...],
   or facts := [...].
   Run DESCRIBE SEMANTIC VIEW <name> to see available dimensions, metrics, and facts.

**Cause:** Neither ``dimensions``, ``metrics``, nor ``facts`` was specified in the query.

**Fix:** Add at least one of ``DIMENSIONS``, ``METRICS`` or ``FACTS`` (``dimensions := [...]``, ``metrics := [...]``, or ``facts := [...]``).


Unknown dimension
-----------------

.. code-block:: text

   semantic view '<view>': unknown dimension '<name>'. Available: [<list>].
   Did you mean '<suggestion>'?

**Cause:** A requested dimension name does not match any dimension in the view.

**Fix:** Check the dimension name. Use :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` to see all available dimensions.


Unknown metric
--------------

.. code-block:: text

   semantic view '<view>': unknown metric '<name>'. Available: [<list>].
   Did you mean '<suggestion>'?

**Cause:** A requested metric name does not match any metric in the view.

**Fix:** Check the metric name. Use :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` to see all available metrics.


Duplicate dimension or metric
-----------------------------

.. code-block:: text

   semantic view '<view>': duplicate dimension '<name>'

   semantic view '<view>': duplicate metric '<name>'

**Cause:** The same dimension or metric appears more than once in the request. Duplicates are detected on the *resolved* item, so requesting both the bare and the table-qualified spelling of the same item (``'region'`` and ``'o.region'``) is also rejected.

**Fix:** Remove the duplicate from the ``dimensions`` or ``metrics`` list.


COUNT(*) on a joined table requires a PRIMARY KEY
-------------------------------------------------

.. code-block:: text

   semantic view '<view>': metric '<name>' uses COUNT(*) on joined table
   '<alias>'. The generated LEFT JOIN produces one NULL-extended row per
   base-table row with no match in '<alias>', which COUNT(*) would count --
   so the expansion rewrites COUNT(*) to COUNT(<primary key>) for non-base
   tables, but table '<alias>' has no PRIMARY KEY declared in the TABLES
   clause. Add PRIMARY KEY (cols) to '<alias>' or use an explicit column:
   COUNT(<alias>.<column>).

**Cause:** A queried metric (directly, via a derived metric, or as a window metric's inner aggregate) is ``COUNT(*)`` on a table other than the base table. Generated joins are ``LEFT JOIN``\ s, so ``COUNT(*)`` would also count the NULL-extended row produced for each base row with no match -- silently inflating the result. The expansion protects against this by rewriting ``COUNT(*)`` to ``COUNT(<first primary key column>)`` for non-base source tables, which requires the table to declare a ``PRIMARY KEY``.

Since v0.14.0 a metric is normally aggregated from its own table, where there are no NULL-extended rows, so this error is rare: it is raised only for the query shapes that still use a base-table ``LEFT JOIN`` (for example a dimension with no source table in a definition stored by an older version).

**Fix:** Declare ``PRIMARY KEY (<cols>)`` for the metric's source table in the ``TABLES`` clause, or define the metric as ``COUNT(<alias>.<column>)`` over a NOT NULL column.


Fan trap detected
-----------------

.. code-block:: text

   semantic view '<view>': fan trap detected -- metric '<metric>' (table '<table>')
   would be duplicated when joined to dimension '<dim>' (table '<table>') via
   relationship '<rel>' (many-to-one cardinality, inferred: FK is not PK/UNIQUE).
   This would inflate aggregation results. Remove the dimension, use a metric from
   the same table, or restructure the relationship.

**Cause:** The query would traverse a one-to-many join boundary, inflating aggregate results.

**Fix:** See :ref:`howto-fan-traps` for detailed solutions: remove the problematic dimension, use a metric from the same table as the dimension, or restructure the view.


Fact reached across a fan-out
-----------------------------

.. code-block:: text

   semantic view '<view>': fan trap detected -- '<metric>' (table '<table>')
   references the fact '<fact>' on table '<fact_table>', and relationship
   '<rel>' fans out on the way there, so joining it would multiply '<metric>'s
   rows. Reference a fact on a table reachable without fanning out, or define
   the fact on '<table>'.

**Cause:** A metric references a fact on a table that sits on the "many" side of a relationship from the metric's table (for example, a metric on ``orders`` using a fact on ``line_items``). Joining that table would repeat each of the metric's rows once per child row.

**Fix:** Move the fact to the metric's own table, reference a fact on a parent (many-to-one) table instead, or define the metric on the child table. See :ref:`howto-facts`.


Ambiguous dimension path
-------------------------

.. code-block:: text

   semantic view '<view>': dimension '<dim>' is ambiguous -- table '<table>'
   is reached via multiple relationships: [<rel1>, <rel2>]. Specify a metric
   with USING to disambiguate, or use a dimension from a non-ambiguous table.

**Cause:** A dimension comes from a table reachable via multiple named relationships (role-playing pattern), and no co-queried metric has a ``USING`` clause that selects one path.

**Fix:** See :ref:`howto-role-playing`. Add a metric with ``USING (<rel_name>)`` to disambiguate, or use a dimension from a table that is not a role-playing target.


Ambiguous fact or metric on a role-playing table
------------------------------------------------

.. versionchanged:: 0.14.0

   These queries used to return the numbers of whichever relationship was
   declared first.

.. code-block:: text

   semantic view '<view>': '<member>' is ambiguous -- it references fact '<fact>'
   on table '<table>', which requires the role-playing table '<table>', joined via
   multiple relationships: [<rel1>, <rel2>]. A metric over a fact on '<table>'
   itself must name exactly one of them with USING; a dimension, or a fact on a
   table reached only through '<table>', has no way to pick a role.

   semantic view '<view>': metric '<metric>' is ambiguous -- it aggregates table
   '<table>', which requires the role-playing table '<table>', joined via
   multiple relationships: [<rel1>, <rel2>]. A metric on that table cannot name a
   role (USING relationships must originate from the metric's own table); declare
   it on the referencing table with USING instead, e.g.
   <table>.<metric> USING (<relationship>) AS SUM(<fact>).

**Cause:** The first form: a metric, dimension, or fact reaches a fact on a role-playing table (or on a table reached only through one), and nothing says which relationship to read it through. The second form: a metric is declared on the role-playing table itself, where ``USING`` cannot be attached. When the relationships fan out, the second case raises ``fan trap detected`` instead. If the role-playing table is the view's base table (declared first in ``TABLES``), a metric on it is answered when it is queried without dimensions, alongside only other base-table metrics, and with a filter (if any) that names only base-table members. A dimension, filter, co-queried metric or ``NON ADDITIVE BY`` dimension on another table brings this error back. Grouping by a dimension on the base table itself, or a ``NON ADDITIVE BY`` on one, is rejected too, with the ``dimension ... is ambiguous`` error (a known limitation).

**Fix:** Declare the metric on the referencing table and give it a ``USING`` that names one relationship, for example ``f.dep_elev USING (dep_airport) AS SUM(a.elev)``. See :ref:`howto-rp-facts`.


Metric cannot be co-queried with a semi-additive metric
--------------------------------------------------------

.. code-block:: text

   semantic view '<view>': metric '<name>' (expression: <expr>) cannot be
   co-queried with semi-additive metric '<semi>': <reason>. Snapshot expansion
   for NON ADDITIVE BY requires every co-queried metric to be a single
   aggregate call SUM/COUNT/AVG/MIN/MAX(<expression>) without '*', DISTINCT,
   or surrounding expression text. Query '<name>' and '<semi>' separately.

**Cause:** The query mixes an active semi-additive metric (its ``NON ADDITIVE BY`` dimension is not in the query) with a metric whose expression cannot be decomposed for the snapshot CTE: ``COUNT(*)``, a ``DISTINCT`` aggregate, arithmetic around the aggregate (``SUM(x) * 0.1``), a wrapped aggregate (``COALESCE(SUM(x), 0)``), or a derived metric.

**Fix:** Query the two metrics separately, or redefine the co-queried metric as a single bare aggregate call (e.g. ``COUNT(<pk_column>)`` instead of ``COUNT(*)``).


Semi-additive metric expression cannot be expanded
---------------------------------------------------

.. code-block:: text

   semantic view '<view>': semi-additive metric '<name>' (expression: <expr>)
   cannot be expanded: <reason>. NON ADDITIVE BY snapshot expansion requires
   the metric to be a single aggregate call SUM/COUNT/AVG/MIN/MAX(<expression>)
   without '*' or DISTINCT.

**Cause:** The ``NON ADDITIVE BY`` metric's own expression is not a single decomposable aggregate call, so the snapshot CTE cannot split it into a per-row column plus an outer re-aggregation.

**Fix:** Redefine the metric as a single aggregate call over a plain expression (e.g. ``SUM(a.balance)``).


Private metric
--------------

.. versionadded:: 0.6.0

.. code-block:: text

   semantic view '<view>': metric '<name>' is private and cannot be queried
   directly. Private metrics can only be used in derived metric expressions.

**Cause:** A metric marked ``PRIVATE`` was requested in ``METRICS`` (the ``metrics := [...]`` list), or used in an expression item.

**Fix:** Private metrics exist for internal composition only. Query a public derived metric that uses the private metric, or recreate the view without the ``PRIVATE`` keyword. See :ref:`howto-metadata-annotations`.


Private fact
------------

.. versionadded:: 0.6.0

.. code-block:: text

   semantic view '<view>': fact '<name>' is private and cannot be queried
   directly. Private facts can only be used in derived expressions.

**Cause:** A fact marked ``PRIVATE`` was requested in ``FACTS`` (the ``facts := [...]`` list), or used in an expression item.

**Fix:** Remove the ``PRIVATE`` keyword from the fact to make it queryable, or reference the fact only from metric expressions.


Cannot combine facts and metrics
----------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   semantic view '<view>': cannot combine facts and metrics in the same query.
   Use facts := [...] OR metrics := [...], not both.

**Cause:** The query includes both ``FACTS`` and ``METRICS`` (``facts := [...]`` and ``metrics := [...]``). Facts are row-level expressions; metrics are aggregated. These modes are mutually exclusive.

**Fix:** Run two separate queries -- one for facts and one for metrics.


Unknown fact
------------

.. versionadded:: 0.6.0

.. code-block:: text

   semantic view '<view>': unknown fact '<name>'. Available: [<list>].
   Did you mean '<suggestion>'?

**Cause:** A requested fact name does not match any fact in the view's ``FACTS`` clause.

**Fix:** Check the fact name against declared facts. The error suggests close matches.


Duplicate fact
--------------

.. versionadded:: 0.6.0

.. code-block:: text

   semantic view '<view>': duplicate fact '<name>'

**Cause:** The same fact name appears more than once in the ``facts := [...]`` list.

**Fix:** Remove the duplicate from the ``facts`` list.


Incompatible table paths for facts
------------------------------------

.. versionadded:: 0.6.0

.. versionchanged:: 0.12.0
   The rule is now about row multiplication rather than tree position, and the
   message wording changed to match. Pairs that are reachable one way without
   fanning out -- a fact on a table that references the base table, with a
   dimension on a table the base table references -- are now accepted.

.. code-block:: text

   semantic view '<view>': fact query references objects from incompatible
   table paths -- neither table '<table_a>' nor '<table_b>' can be reached
   from the other without crossing a one-to-many relationship, so joining
   them would duplicate the rows returned

**Cause:** A fact query does not aggregate, so it returns rows as they are. Reaching one of these two tables from the other means traversing a one-to-many relationship *against* its direction -- every row on one side matching many on the other -- and that holds whichever of the two you start from. The rows returned would silently be duplicates. The usual shape is two tables that both reference a third (``line_items`` and ``shipments`` both referencing ``orders``): joining both multiplies each one's rows by the other's.

**Fix:** Query the two tables separately. Facts and dimensions can be combined freely as long as one side is reachable from the other without fanning out -- a chain of many-to-one relationships in either direction is fine, however long.


Window and aggregate metric mixing
------------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   semantic view '<view>': cannot mix window function metrics [<window_metrics>]
   with aggregate metrics [<aggregate_metrics>] in the same query

**Cause:** The query requests both window function metrics and standard aggregate metrics. These produce different result shapes (row-level vs. grouped) and cannot be combined.

**Fix:** Run separate queries for window metrics and aggregate metrics.


Window metric required dimension missing
------------------------------------------

.. versionadded:: 0.6.0

.. code-block:: text

   semantic view '<view>': window function metric '<metric>' requires
   dimension '<dim>' to be included in the query (used in <reason>)

**Cause:** A window function metric references a dimension in its ``PARTITION BY EXCLUDING``, ``PARTITION BY``, or ``ORDER BY`` clause, but that dimension was not included in the query's ``dimensions := [...]`` list. The ``<reason>`` value indicates which clause requires the dimension (``PARTITION BY EXCLUDING``, ``PARTITION BY``, or ``ORDER BY``).

**Fix:** Add the required dimension to the query. Use :ref:`SHOW SEMANTIC DIMENSIONS FOR METRIC <ref-show-dims-for-metric>` to see which dimensions are required (``required = TRUE``) for a window metric.


.. _ref-err-time-limit:

Query time limit exceeded
-------------------------

.. versionadded:: 0.15.0

.. code-block:: text

   INTERRUPT Error: Query exceeded maximum execution time

**Cause:** The session has a per-query time limit (``SET max_execution_time``, in milliseconds), and the query ran past it. This is DuckDB's own error. It applies to semantic-view queries and to the ``SHOW`` and ``DESCRIBE`` commands that report member data types (see :ref:`ref-sv-time-limits`).

**Fix:** Raise the limit, or turn it off for the session:

.. code-block:: sql

   SET max_execution_time = 60000;  -- 60 seconds
   RESET max_execution_time;        -- no limit


.. _ref-err-concurrent-ddl:

Concurrent DDL Errors
=====================

These errors relate to :ref:`DROP <ref-drop-semantic-view>` or :ref:`ALTER SEMANTIC VIEW <ref-alter-semantic-view>` when another writer modifies the catalog around the same time.


Existence guard on DROP / ALTER (and its autocommit window)
-----------------------------------------------------------

A non-``IF EXISTS`` ``DROP`` or ``ALTER`` runs a small existence check before its ``DELETE`` / ``UPDATE``. When the view is absent at check time you get:

.. code-block:: text

   semantic view '<name>' does not exist

and ``ALTER ... RENAME`` additionally raises, when the target name is taken:

.. code-block:: text

   semantic view '<new_name>' already exists

**Cause:** The view was not present (or the rename target was already present) when the guard evaluated -- either it never existed, or another connection had already committed the change by the time your statement ran.

**A subtlety under autocommit:** the guard and the ``DELETE`` / ``UPDATE`` are separate statements. Under autocommit (the default) they commit independently, so a concurrent commit that lands in the window *between* them is **not** caught by the guard: a concurrent ``DROP`` leaves your ``DROP`` deleting 0 rows and reporting success, and a rename target taken in that window surfaces a raw ``Constraint Error: Duplicate key`` from DuckDB instead of the friendly ``already exists`` message.

**Fix:** Decide on the contract you want. Use ``IF EXISTS`` if a missing target should silently no-op (``DROP SEMANTIC VIEW IF EXISTS my_view``, ``ALTER SEMANTIC VIEW IF EXISTS my_view ...``). If you need the check and the write to be atomic under concurrency, wrap the statement in an explicit transaction (``BEGIN; DROP SEMANTIC VIEW my_view; COMMIT;``): all statements then share one snapshot, and a conflicting concurrent commit makes your ``COMMIT`` fail with a retryable transaction-conflict error instead of slipping through the window. See :ref:`explanation-transactional-ddl` for the full mechanism.


.. _ref-err-resolution:

Name Resolution and Catalog Errors
==================================

These errors come from how a view name is resolved to a schema, and from which database holds the semantic view catalog.


Unqualified name that is not on the search path
-----------------------------------------------

.. code-block:: text

   semantic view '<name>' does not exist on the search path. It exists in
   schemas <schemas>, none of which are on the current search path (<path>).
   Qualify the reference as <schema>.<name>, or add the schema to search_path.

**Cause:** An unqualified reference resolves through the session's ``search_path``, the same rule DuckDB applies to an unqualified table name. A view of this name exists, but only in schemas that are not on the path -- so by that rule it is not visible from here. DuckDB would report a plain "does not exist"; this message says where the view actually is instead, because a bare not-found is confusing for something :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>` plainly lists.

This applies to reads and writes alike, and ``IF EXISTS`` does not suppress it: that clause means "do not complain if the view is absent", and a view sitting off the path is not absent. Suppressing it would let a ``DROP`` silently remove one of several same-named views.

**Fix:** Either qualify the reference (``DROP SEMANTIC VIEW analytics.sales``, ``SEMANTIC_VIEW(analytics.sales METRICS ...)``) or put the schema on the path (``SET search_path = 'analytics'``). When several schemas hold the name, the first one on the path wins.

**Note:** A view that is the *only* one of its name resolves whether or not its schema is on the path, so this error only appears once a second view shares the name.


DDL issued from an attached database
------------------------------------

.. code-block:: text

   semantic_views: semantic-view DDL was issued against database '<db>', but
   the semantic view catalog lives in a different database. Semantic views are
   single-catalog: manage them from the database the extension was loaded
   into, without USE-ing into an attached database.

**Cause:** The session ran ``USE`` on an attached database, then issued ``CREATE``, ``ALTER``, or ``DROP SEMANTIC VIEW``. Semantic view definitions are stored only in the database the extension was loaded into.

**Fix:** Switch back to that database (``USE memory``, or the name of your file database) before running semantic-view DDL. A view body can still read tables from an attached database by qualifying them (``TABLES (o AS other.main.orders ...)``). See :ref:`explanation-txn-ddl-attach`.


.. _ref-err-wildcard:

Wildcard Errors
================

.. versionadded:: 0.6.0

These errors occur when using ``alias.*`` wildcard patterns in ``dimensions``, ``metrics``, or ``facts`` parameters.


Unqualified wildcard
--------------------

.. code-block:: text

   unqualified wildcard '*' is not supported. Use table_alias.* to select
   all items for a specific table.

**Cause:** A bare ``*`` was used without a table alias prefix.

**Fix:** Use ``alias.*`` instead of ``*``. For example: ``DIMENSIONS o.*`` (``dimensions := ['o.*']``) to select all dimensions from the ``o`` table alias.


Unknown table alias in wildcard
---------------------------------

.. code-block:: text

   unknown table alias '<alias>' in wildcard '<alias>.*'. Available aliases:
   [<list>]

**Cause:** The table alias in a wildcard expression does not match any alias declared in the view's ``TABLES`` clause.

**Fix:** Check the alias name against the declared table aliases. The error lists all available aliases.
