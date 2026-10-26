.. meta::
   :description: Full syntax and parameter reference for CREATE SEMANTIC VIEW, covering TABLES, RELATIONSHIPS, FACTS, DIMENSIONS, METRICS, and MATERIALIZATIONS clauses, plus FROM YAML and FROM YAML FILE variants

.. _ref-create-semantic-view:

======================
CREATE SEMANTIC VIEW
======================

Creates a semantic view definition, registering dimensions, metrics, relationships, facts, and materializations for on-demand query expansion.


.. _ref-create-syntax:

Syntax
======

**Keyword body (AS):**

.. code-block:: sqlgrammar

   CREATE [ OR REPLACE ] SEMANTIC VIEW [ IF NOT EXISTS ] <name>
       [ COMMENT = '<text>' ]
   AS
   TABLES (
       [ <alias> AS ] <table_name>
           [ PRIMARY KEY ( <column> [, <column> ...] ) ]
           [ UNIQUE ( <column> [, <column> ...] ) ]
           [ COMMENT = '<text>' ]
           [ WITH SYNONYMS = ( '<synonym>' [, '<synonym>' ...] ) ]
           [ [ WITH ] TAG ( <tag_name> = '<value>' [, ...] ) ]
       [, ... ]
   )
   [ RELATIONSHIPS (
       <rel_name> AS <from_alias>( <fk_column> [, <fk_column> ...] )
           REFERENCES <to_alias> [( <ref_column> [, <ref_column> ...] )]
       [, ... ]
   ) ]
   [ FACTS (
       [ PRIVATE ] <alias>.<fact_name> AS <row_level_expression>
           [ COMMENT = '<text>' ]
           [ WITH SYNONYMS = ( '<synonym>' [, '<synonym>' ...] ) ]
           [ LABELS = ( FILTER ) ]
           [ [ WITH ] TAG ( <tag_name> = '<value>' [, ...] ) ]
       [, ... ]
   ) ]
   [ DIMENSIONS (
       [ PUBLIC ] <alias>.<dim_name> AS <expression>
           [ COMMENT = '<text>' ]
           [ WITH SYNONYMS = ( '<synonym>' [, '<synonym>' ...] ) ]
           [ LABELS = ( FILTER ) ]
           [ [ WITH ] TAG ( <tag_name> = '<value>' [, ...] ) ]
       [, ... ]
   ) ]
   [ METRICS (
       [ PRIVATE ] <alias>.<metric_name>
           [ USING ( <rel_name> [, <rel_name> ...] ) ]
           [ NON ADDITIVE BY ( <dim_name> [ ASC | DESC ] [ NULLS FIRST | NULLS LAST ]
               [, <dim_name> ... ] ) ]
           AS <aggregate_expression>
           [ COMMENT = '<text>' ]
           [ WITH SYNONYMS = ( '<synonym>' [, '<synonym>' ...] ) ]
           [ [ WITH ] TAG ( <tag_name> = '<value>' [, ...] ) ]
       [, [ PRIVATE ] <metric_name> AS <derived_expression> ... ]
       [, [ PRIVATE ] <alias>.<metric_name> AS
           <window_func>( <inner_metric> [, <extra_arg> ...] )
               OVER ( [ PARTITION BY EXCLUDING <dim_name> [, <dim_name> ...] ]
                      | [ PARTITION BY <dim_name> [, <dim_name> ...] ]
                      [ ORDER BY <dim_name> [ ASC | DESC ] [ NULLS FIRST | NULLS LAST ]
                          [, <dim_name> ...] ]
                      [ <frame_clause> ] )
           [ COMMENT = '<text>' ]
           [ WITH SYNONYMS = ( '<synonym>' [, '<synonym>' ...] ) ]
           [ [ WITH ] TAG ( <tag_name> = '<value>' [, ...] ) ] ]
   ) ]
   [ MATERIALIZATIONS (
       <mat_name> AS (
           TABLE <table_name>,
           [ DIMENSIONS ( <dim_name> [, <dim_name> ...] ) , ]
           [ METRICS ( <metric_name> [, <metric_name> ...] ) ]
       )
       [, ... ]
   ) ]
   [ COMMENT = '<text>' ]
   [ [ WITH ] TAG ( <tag_name> = '<value>' [, ...] ) ]

.. note::

   **Snowflake syntax conveniences.**

   .. versionadded:: 0.11.0

   To make porting Snowflake DDL easier, several Snowflake spellings are now
   accepted in addition to the canonical forms above:

   - The table alias in ``TABLES`` is **optional** -- ``TABLES (orders PRIMARY
     KEY (id))`` defaults the alias to the table name, matching Snowflake's
     ``[alias AS] table``.
   - A view-level ``COMMENT = '<text>'`` may be written in Snowflake's
     **trailing** position (after the last clause) as well as between the name
     and ``AS``. Specifying it in both positions is rejected.
   - An explicit ``PUBLIC`` modifier is accepted on a dimension (a no-op, since
     public is the default). ``PRIVATE`` on a dimension is still rejected rather
     than silently downgraded.
   - ``WITH SYNONYMS (...)`` is accepted **without** the ``=``.
   - ``LABELS (...)`` is likewise accepted **without** the ``=``.

.. note::

   **Annotation order.** ``COMMENT``, ``WITH SYNONYMS``, ``LABELS`` and
   ``[WITH] TAG`` may appear in any order on a single entry; the grammar above shows one
   canonical spelling, not a required sequence. What *is* required is that the
   annotations be contiguous and complete: once the first annotation keyword is
   seen, the remainder of the entry must consist only of valid annotation
   clauses separated by whitespace. Trailing junk or a repeated clause is a
   parse error rather than being silently ignored.

   ``LABELS`` is accepted only on a ``FACTS`` or ``DIMENSIONS`` entry -- those
   are the entries that carry the flag. On a ``TABLES`` or ``METRICS`` entry, or
   in the trailing view-level annotation position, it is rejected rather than
   parsed and discarded.

.. _ref-create-tags:

.. note::

   **Tags.**

   .. versionadded:: 0.17.0

   ``[WITH] TAG (<tag_name> = '<value>', ...)`` attaches Snowflake-style
   name/value tags to a logical table, fact, dimension or metric entry, or to
   the view itself (written after the last clause, after any trailing
   ``COMMENT``). Tags are metadata: they never change what a query returns.

   - ``<tag_name>`` is a tag name with up to three dot-separated parts --
     ``tag``, ``schema.tag`` or ``database.schema.tag`` -- each part an
     identifier or a double-quoted identifier. No ``CREATE TAG`` object is
     needed or looked up: the name is stored as written.
   - ``<value>`` is a single-quoted string of at most 256 characters.
   - After a member's expression, a bare ``TAG (`` begins the clause only
     when a tag name and ``=`` follow, so a call to a macro or function named
     ``tag``, such as ``tag(o.x)``, stays part of the expression. A call whose
     first argument is ``<name> = ...``, such as ``tag(o.region = 'East')``,
     reads as a tag clause, as in Snowflake. ``WITH TAG`` always begins the
     clause.
   - A tag name may appear once per object (compared case-insensitively, quoted
     or not), and an object takes one ``TAG`` clause.
   - Tags appear as a ``TAGS`` row in :ref:`DESCRIBE SEMANTIC VIEW
     <ref-describe-semantic-view>` and are written back by ``GET_DDL`` and the
     YAML export. ``SHOW SEMANTIC ...`` output does not include them, matching
     Snowflake.

   .. code-block:: sql

      CREATE SEMANTIC VIEW sales AS
        TABLES (o AS orders PRIMARY KEY (id) WITH TAG (governance.pii = 'none'))
        DIMENSIONS (
          o.region AS o.region
            WITH TAG (S_LAYER_KIND = 'dimension', S_LAYER_LABEL = 'Sales region')
        )
        METRICS (o.revenue AS SUM(o.amount) TAG (S_LAYER_KIND = 'metric'))
        COMMENT = 'Sales by region'
        WITH TAG (owner = 'analytics');

**YAML body (FROM YAML):**

.. versionadded:: 0.7.0

.. code-block:: sqlgrammar

   CREATE [ OR REPLACE ] SEMANTIC VIEW [ IF NOT EXISTS ] <name>
       FROM YAML $$ <yaml_content> $$

   CREATE [ OR REPLACE ] SEMANTIC VIEW [ IF NOT EXISTS ] <name>
       FROM YAML FILE '<file_path>'

The ``FROM YAML`` variant accepts a YAML definition in a dollar-quoted string (``$$...$$`` or ``$tag$...$tag$``). The ``FROM YAML FILE`` variant reads the YAML definition from a file at the given path.


.. _ref-create-variants:

Statement Variants
==================

``CREATE SEMANTIC VIEW <name> AS ...``
   Creates a new semantic view. Returns an error if a view with the same name already exists.

``CREATE OR REPLACE SEMANTIC VIEW <name> AS ...``
   Creates or replaces an existing semantic view with the same name. If the view does not exist, creates it. If it does, replaces the definition.

``CREATE SEMANTIC VIEW IF NOT EXISTS <name> AS ...``
   Creates a new semantic view only if no view with the same name exists. If a view with the name already exists, the statement succeeds silently without modifying it.

All three variants work with both the ``AS`` keyword body and the ``FROM YAML`` / ``FROM YAML FILE`` body.

.. note::

   **View name case.** A ``<name>`` is folded to lowercase before it is stored, whether written quoted or not, and every later reference -- ``DROP`` / ``DESCRIBE`` / ``ALTER`` / ``SHOW COLUMNS`` statements, the view name in :ref:`SEMANTIC_VIEW(...) <ref-sv-construct>` and the :ref:`semantic_view() <ref-semantic-view-function>` lookup argument -- folds the same way, so ``CREATE SEMANTIC VIEW Sales``, ``DROP SEMANTIC VIEW SALES``, and ``DROP SEMANTIC VIEW "sales"`` all refer to the same view. This follows DuckDB's identifier semantics, where double-quoted identifiers are case-insensitive too; quoting a name only lets it contain whitespace or other special characters, it does not make it case-sensitive.

   *Migration:* lookups fold the requested name to lowercase and match the stored catalog name exactly, so a view is only reachable if its **stored** name is lowercase. Unquoted ``CREATE`` always stored a lowercase name, so those views are unaffected. Only a view created before v0.11 via a *quoted* mixed-case identifier (e.g. ``CREATE SEMANTIC VIEW "Sales"``) kept its original casing in the catalog, and it is no longer reachable by any spelling -- ``sales``, ``Sales``, and ``"Sales"`` all fold to ``sales``, which does not match the stored ``Sales``. Drop and recreate it, or rename its catalog row to lowercase, to make it reachable again.

.. note::

   **Semantic views live in a schema.** ``<name>`` may carry a ``<schema>.`` (or ``<database>.<schema>.``) qualifier, and that qualifier decides where the view is created -- ``CREATE SEMANTIC VIEW analytics.sales`` puts the view in ``analytics`` regardless of which schema the session is currently in. Without a qualifier the view is created in the session's current schema, exactly as an unqualified ``CREATE TABLE`` would be. Naming a schema that does not exist is an error rather than a silent fall back to the current schema.

   Two schemas may each hold a view of the same name, so ``analytics.sales`` and ``staging.sales`` are two different views. A reference that names neither -- ``DROP SEMANTIC VIEW sales``, ``SEMANTIC_VIEW(sales METRICS ...)`` -- resolves through the session's ``search_path``, exactly as an unqualified table reference does: the first schema on the path holding a view of that name wins. A view that is the only one of its name resolves whether or not its schema is on the path, so an ordinary single-schema setup needs no ``search_path`` at all. Reads and writes resolve identically -- ``DROP SEMANTIC VIEW v`` removes the view ``semantic_view('v')`` returns.

   A name that exists only in schemas *off* the path is a miss: the error names the schemas it does live in, the path that was searched, and the two ways out (qualify the reference, or add the schema to ``search_path``). ``IF EXISTS`` does not absorb it -- that clause means "do not complain if the view is absent", and a view sitting off the path is not absent.

   The rule holds everywhere a view is named, :ref:`ref-get-ddl` and :ref:`READ_YAML_FROM_SEMANTIC_VIEW <ref-read-yaml>` included (before 0.16.0 those two ignored ``search_path`` and required a unique match).

   Unqualified table names in the body resolve in the **creating session's** schema, not in the view's -- so a semantic view in ``analytics`` built over ``main.orders`` can be written as ``CREATE SEMANTIC VIEW analytics.sales AS TABLES (o AS orders ...)`` from a session in ``main``. This follows DuckDB's rule for a view body.

.. note::

   All four ``CREATE`` body variants participate in your surrounding transaction. ``BEGIN ... ROLLBACK`` discards an uncommitted ``CREATE``. See :ref:`explanation-transactional-ddl`.

.. note::

   **Every member is bound at CREATE.** Each dimension, fact and metric, ``PRIVATE`` ones included, is bound in a query of its own against the view's tables, in the creating session, so a temporary table or one created earlier in the same transaction is visible. A member DuckDB cannot bind (an unknown column, table or function, a syntax error, or an argument DuckDB rejects such as ``date_trunc('fortnight', ...)``) refuses the whole ``CREATE`` with an error naming it: see :ref:`ref-err-member-bind`. Every table in ``TABLES`` must exist too, including one that no member reads, or the ``CREATE`` is refused naming its alias (``table '<alias>' does not bind``). This follows Snowflake, which compiles each expression and resolves each logical table at ``CREATE``. A refused ``CREATE OR REPLACE`` leaves the existing view as it was. Some things are still not checked at ``CREATE``: materialization tables, since they may be created later; and a member's type, so a ``LABELS = (FILTER)`` member that is not ``BOOLEAN`` fails when a query's ``WHERE`` uses it. A source table changed after ``CREATE`` can also break a member, which then fails when a query reaches it.

   .. versionchanged:: 0.17.0
      Before, such a member was stored and its error appeared only when a query reached it.

.. note::

   ``CREATE SEMANTIC VIEW IF NOT EXISTS`` reliably absorbs duplicates within a single connection or transaction (re-running a setup script, repeated statements in the same ``BEGIN`` block). It cannot absorb a race between two connections that both run ``CREATE IF NOT EXISTS`` against the same database at the same time -- one will succeed and the other will see a constraint error. Concurrent DDL across connections is unusual for typical DuckDB workloads, so most users will never hit this. See :ref:`explanation-txn-ddl-create-race` for the workaround.

.. note::

   Requires a writable database. On a read-only database this statement fails with DuckDB's standard ``Cannot execute statement of type "..." on database "<name>" which is attached in read-only mode!`` error. See :ref:`explanation-txn-ddl-readonly`.


.. _ref-create-clauses:

Clauses
=======

Clauses must appear in the following order: ``TABLES``, ``RELATIONSHIPS``, ``FACTS``, ``DIMENSIONS``, ``METRICS``, ``MATERIALIZATIONS``. ``TABLES`` is required. At least one of ``DIMENSIONS`` or ``METRICS`` is required. All other clauses are optional.


.. _ref-create-tables:

TABLES
------

Declares the physical tables available to the semantic view. Each entry assigns an alias and maps it to a physical table. A primary key declaration is optional but recommended for multi-table views (it drives JOIN synthesis and cardinality inference).

.. code-block:: sql

   TABLES (
       o AS orders    PRIMARY KEY (id) COMMENT = 'Order transactions',
       c AS customers PRIMARY KEY (id) WITH SYNONYMS = ('clients', 'buyers')
   )

**Parameters:**

- ``<alias>``, a short name used to reference this table in all other clauses. Optional -- ``AS <table_name>`` may be omitted (``TABLES (orders PRIMARY KEY (id))``), in which case the alias defaults to the table name.
- ``<table_name>``, the physical table name. Supports catalog-qualified names (``catalog.schema.table``).
- ``PRIMARY KEY (<column>, ...)``, optional. One or more columns forming the table's primary key. Used for JOIN synthesis and cardinality inference. This is semantic metadata, not a DuckDB constraint. Omit for fact tables that do not need to be join targets.
- ``COMMENT = '<text>'``, optional. A human-readable description of the table.
- ``WITH SYNONYMS = ('<synonym>', ...)``, optional. Alternative names for discoverability. May be written before or after ``COMMENT`` when both are present.
- ``[WITH] TAG (<tag_name> = '<value>', ...)``, optional. Name/value metadata tags. See :ref:`Tags <ref-create-tags>`.

**Optional: UNIQUE constraints:**

.. code-block:: sql

   TABLES (
       o AS orders PRIMARY KEY (id) UNIQUE (email)
   )

``UNIQUE (<column>, ...)`` declares additional unique constraints. Used for cardinality inference: if a relationship's FK columns match a UNIQUE constraint, the relationship is inferred as one-to-one.

The first table in the ``TABLES`` clause is the **base table** (the root of the relationship graph). All other tables must be reachable from the base table through declared relationships.


.. _ref-create-relationships:

RELATIONSHIPS
-------------

Declares FK/PK join paths between tables. Each entry names a relationship, specifies the FK columns on the "from" side, and references the target table alias.

.. code-block:: sql

   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       order_product  AS o(product_id)  REFERENCES p
   )

**Parameters:**

- ``<rel_name>``, a unique name identifying this relationship. It may be a quoted identifier (``"Dep Airport"``), including one with spaces or an embedded ``""``.
- ``<from_alias>``, the table alias containing the FK columns.
- ``(<fk_column>, ...)``, one or more FK column names on the "from" table.
- ``REFERENCES <to_alias> [(<ref_column>, ...)]``, the target table alias. Optionally specify which columns on the target table to join against. If omitted, the target's ``PRIMARY KEY`` columns are used. The JOIN ON clause is synthesized as ``from_alias.fk_column = to_alias.ref_column``.

**Cardinality inference:**

The extension infers cardinality from the "from" table's constraints:

- If the FK columns match a ``PRIMARY KEY`` or ``UNIQUE`` constraint on the "from" table, the relationship is **one-to-one**.
- Otherwise, the relationship is **many-to-one** (the default).

Cardinality is used for :ref:`fan trap detection <howto-fan-traps>`.

**Validation rules:**

- The relationship graph must form a tree rooted at the base table: each table is reached from exactly one other table.
- Cycles are rejected: ``cycle detected in relationships: a -> b -> a``.
- Several named relationships from the **same** source table to the same target are allowed. This is the role-playing pattern (``dep_airport`` and ``arr_airport`` from ``flights`` to ``airports``); pick the path per metric with ``USING``.
- One target reached from two **different** source tables is a diamond, and is rejected whatever the relationships are named: ``diamond: 's' is reachable from multiple tables ('a', 'b'); the join path is ambiguous.`` To model it, declare the shared table twice under two aliases, one per path, so each is joined once.
- Relationship names must be unique within the view, whichever tables the relationships connect. Names are compared the way DuckDB compares identifiers (case-insensitively, quoted or not), so ``route`` and ``"ROUTE"`` are the same name: ``duplicate relationship name 'route'``. A role-playing pair that repeats a name gets this error too.
- Self-references (``from_alias`` equals ``to_alias``) are rejected.
- Every table must be reachable from the base table (the first table in ``TABLES``) through some chain of relationships, in either direction. An orphan table (declared in ``TABLES`` but not connected by any relationship), a base table with no relationship, two separate groups of related tables, and several tables with no ``RELATIONSHIPS`` clause at all are all rejected, naming the table that cannot be reached. A single-table view needs no relationships.

.. versionchanged:: 0.14.0

   Duplicate relationship names and tables that cannot be reached from the base
   table are now rejected at ``CREATE``. Both used to be accepted: a repeated
   name made ``USING`` silently pick the first relationship declared with it,
   and an unreachable table made every query that needed it fail when bound.
   Relationship names are now matched the way DuckDB matches identifiers, and
   quoted relationship names work on role-playing relationships. Views stored
   by earlier versions keep loading.


.. _ref-create-facts:

FACTS
-----

Declares named row-level expressions. Facts are inlined into metric expressions at expansion time.

.. code-block:: sql

   FACTS (
       li.net_price  AS li.extended_price * (1 - li.discount)
           COMMENT = 'Price after discount',
       PRIVATE li.internal_cost AS li.unit_cost * li.quantity,
       li.tax_amount AS li.net_price * li.tax_rate
           WITH SYNONYMS = ('tax', 'vat')
   )

**Parameters:**

- ``PRIVATE``, optional. When present, the fact cannot be queried directly via ``facts := [...]`` but can still be referenced in metric expressions.
- ``<alias>.<fact_name>``, the table alias and fact name, appearing **before** ``AS``. Facts are scoped to a single table. The fact name is what you query (``FACTS net_price``) and what ``DESCRIBE`` returns as the output column. As in Snowflake, the entry reads ``name AS expression`` -- the reverse of a plain SQL ``expression AS alias``.
- ``<row_level_expression>``, the SQL expression **after** ``AS``: any expression that operates on individual rows. Must not contain aggregate functions. A fact may be named after its own backing column (``s.unit_price AS s.unit_price``), giving a passthrough fact.
- ``COMMENT = '<text>'``, optional. A human-readable description.
- ``WITH SYNONYMS = ('<synonym>', ...)``, optional. Alternative names for discoverability.
- ``LABELS = (FILTER)``, optional. Declares the fact a :ref:`named filter <howto-annotations-filters>` -- a boolean-valued member meant for reuse in a query's :ref:`WHERE <ref-sv-pre-agg-filtering>`. Metadata only: it does not restrict querying, and the ``BOOLEAN`` requirement is enforced by DuckDB's binder at query time. ``FILTER`` is the only accepted label.
- ``[WITH] TAG (<tag_name> = '<value>', ...)``, optional. Name/value metadata tags. See :ref:`Tags <ref-create-tags>`.

**Fact chaining:**

Facts can reference other facts by name. The extension resolves dependencies in topological order and inlines them recursively. Circular references between distinct facts are rejected at define time. A fact whose expression contains its *own* name (``s.unit_price AS s.unit_price``) is treated as a reference to the physical column, not a self-cycle.

**Validation rules:**

- Aggregate functions (``SUM``, ``COUNT``, ``AVG``, ``MIN``, ``MAX``, etc.) in fact expressions are rejected. An aggregate inside a subquery or with ``OVER`` (a windowed aggregate) is one value per row and is allowed. See :ref:`ref-error-messages`.
- A prepared statement parameter (``?``, ``$1``, ``$name``) in a fact expression is rejected; use one in the query's ``WHERE`` clause instead.
- Circular references between distinct facts are rejected (a fact referencing its own name resolves to the physical column).

**Reported data type:**

As with dimensions and metrics, ``CREATE`` records no output type for a fact. The ``data_type`` column reported by :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>` and :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` is worked out when that command runs: it is the type of the fact's expression over the current source columns. See :ref:`Reported Data Types <explanation-sf-data-types>`.


.. _ref-create-dimensions:

DIMENSIONS
----------

Declares named grouping expressions available for queries.

.. code-block:: sql

   DIMENSIONS (
       o.region   AS o.region COMMENT = 'Sales region',
       o.category AS o.category WITH SYNONYMS = ('product_category'),
       o.month    AS date_trunc('month', o.ordered_at)
   )

**Parameters:**

- ``PUBLIC``, optional. Accepted as an explicit no-op (dimensions are always public). ``PRIVATE`` is **not** allowed on a dimension and is rejected rather than silently downgraded. This matches Snowflake, whose grammar lists ``{ PRIVATE | PUBLIC }`` on a dimension but whose rule is that `"You cannot mark a dimension as private. Dimensions are always public." <https://docs.snowflake.com/en/sql-reference/sql/create-semantic-view>`_
- ``<alias>.<dim_name>``, the table alias and dimension name. The alias indicates which table the dimension comes from (used for join dependency resolution).
- ``<expression>``, any row-level SQL expression: no aggregate (outside a subquery or ``OVER``) and no prepared statement parameter. Can be a simple column reference (``o.region``) or a computed expression (``date_trunc('month', o.ordered_at)``).
- ``COMMENT = '<text>'``, optional. A human-readable description.
- ``WITH SYNONYMS = ('<synonym>', ...)``, optional. Alternative names for discoverability.
- ``LABELS = (FILTER)``, optional. Declares the dimension a :ref:`named filter <howto-annotations-filters>` -- a boolean-valued member meant for reuse in a query's :ref:`WHERE <ref-sv-pre-agg-filtering>`. Metadata only: it does not hide the dimension from output, and the ``BOOLEAN`` requirement is enforced by DuckDB's binder at query time. ``FILTER`` is the only accepted label.
- ``[WITH] TAG (<tag_name> = '<value>', ...)``, optional. Name/value metadata tags. See :ref:`Tags <ref-create-tags>`.

.. note::

   **Column qualification in expressions.** The ``<alias>.<dim_name>`` on the left of ``AS`` (the logical name) must always carry its table-alias prefix. Column references *inside the expression* on the right of ``AS`` may be either qualified (``o.region``) or unqualified (``region``). Unqualified references resolve fine in single-table views, but in multi-table views they can raise an ambiguity error when more than one joined table exposes the same column name. Qualifying expression columns (``alias.column``) is recommended -- it is unambiguous in every case, which is why all examples in these docs use it. The same rule applies to ``METRICS`` and ``FACTS`` expressions. An unqualified column that belongs to *another* table is refused at ``CREATE`` when the member's own query does not join that table, such as a metric on a child table, which scans that table alone. When the member's own query does join it (a dimension on a child table reading a parent's column), the column binds to that table, possibly at a different grain from the member, where Snowflake rejects it. Qualify it, or define a fact on the other table.

**Validation rules:**

- Dimension names must be unique within the view (case-insensitive, quoted or not -- DuckDB treats double-quoted identifiers as case-insensitive too). For example, ``region`` and ``Region`` cannot both appear in the same ``DIMENSIONS`` clause. See :ref:`ref-err-name-uniqueness`.
- A dimension name cannot collide with any metric name (case-insensitive, quoted or not -- DuckDB treats double-quoted identifiers as case-insensitive too). See :ref:`ref-err-name-uniqueness`.
- An aggregate function in the expression is rejected, unless it is inside a subquery or windowed with ``OVER``: a query groups by a dimension. So is a prepared statement parameter. See :ref:`ref-error-messages`.
- The table alias that qualifies the dimension name must be declared in ``TABLES``, in any case (``o`` or ``O`` for alias ``o``). This applies to a single-table view too: ``x.foo`` in a view whose only table is ``o`` is rejected with ``unknown source table 'x'; did you mean 'o'?`` (since v0.14.0; earlier versions accepted it). Metrics follow the same rule.

  .. note::

     A *quoted* qualifier (``"O".region`` for alias ``o``) is currently accepted only in a single-table view. In a view with several tables, ``CREATE`` rejects it with ``unknown source table '"O"'`` even though DuckDB treats ``"O"`` as the same identifier as ``o``; facts reject it in every view. Write the qualifier unquoted. This is a known limitation.

**Reported data type:**

``CREATE`` does not record an output type for a dimension, and no surface can declare one: the DDL grammar above has no clause for it, and the YAML ``output_type`` field was withdrawn because :ref:`ref-get-ddl` could not carry it (a restored view silently lost the cast). To force a type, write the cast into the expression (``CAST(o.code AS VARCHAR)``).

The ``data_type`` column reported by :ref:`SHOW SEMANTIC DIMENSIONS <ref-show-semantic-dimensions>` and :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` is worked out when that command runs, the same way :ref:`semantic_view() <ref-semantic-view-function>` types its output columns when a query is bound. It therefore always matches the type a query returns, and follows a source column whose type changes after ``CREATE``. See :ref:`Reported Data Types <explanation-sf-data-types>`.

.. versionchanged:: 0.10.0

   The define-time inference pass was removed, and ``data_type`` was empty for
   new views.

.. versionchanged:: 0.14.0

   ``data_type`` is worked out when the ``SHOW`` / ``DESCRIBE`` command runs,
   so it is populated again, also for views created by v0.10.0 to v0.13.0.


.. _ref-create-metrics:

METRICS
-------

Declares named aggregation expressions, derived metrics, semi-additive metrics, and window metrics.

**Clause order.** A metric entry reads ``[ PRIVATE ] <alias>.<name> [ USING (...) ] [ NON ADDITIVE BY (...) ] AS <expr>``. Both ``USING`` and ``NON ADDITIVE BY`` belong **before** ``AS``, never after it. Writing either after ``AS`` is rejected at define time -- ``CREATE`` fails with an error naming the misplaced clause rather than absorbing it into the aggregate expression, where its meaning would be silently lost. Wrapping the expression in parentheses does not change this.

**Base metrics** (with table alias, containing aggregate functions):

.. code-block:: sql

   METRICS (
       o.revenue     AS SUM(o.amount) COMMENT = 'Total revenue',
       o.order_count AS COUNT(*) WITH SYNONYMS = ('num_orders')
   )

**Derived metrics** (no table alias, referencing other metric names):

.. code-block:: sql

   METRICS (
       li.revenue AS SUM(li.net_price),
       li.cost    AS SUM(li.unit_cost),
       profit     AS revenue - cost,
       margin     AS profit / revenue * 100
   )

A derived metric is an unqualified name (no table alias) whose expression combines other metrics. ``USING``, ``NON ADDITIVE BY``, and ``OVER`` apply only to qualified (``alias.name``) metrics and are rejected on derived metrics.

**USING clause** (for role-playing dimensions):

.. code-block:: sql

   METRICS (
       f.departures USING (dep_airport) AS COUNT(*),
       f.arrivals   USING (arr_airport) AS COUNT(*)
   )

**PRIVATE metrics:**

.. code-block:: sql

   METRICS (
       PRIVATE o.raw_total AS SUM(o.amount),
       net_total AS raw_total * 0.9
   )

**Semi-additive metrics** (with ``NON ADDITIVE BY``):

.. code-block:: sql

   METRICS (
       a.total_balance NON ADDITIVE BY (report_date) AS SUM(a.balance)
   )

When a query does not include the non-additive dimension, the extension generates a CTE with ``RANK`` to select the snapshot rows per group before aggregating (rows tied at the snapshot ordering value all aggregate). The default (ascending) direction selects the latest snapshot; ``DESC`` selects the earliest. When the non-additive dimension is included in the query, the metric behaves as a standard additive metric.

See :ref:`howto-semi-additive` for details.

**Window metrics** (with ``OVER`` clause):

Window metrics wrap another metric in a SQL window function. The ``OVER`` clause supports two mutually exclusive partitioning modes:

``PARTITION BY EXCLUDING`` computes the partition set dynamically at query time as "all queried dimensions minus the excluded ones." The partition changes depending on which dimensions are requested:

.. code-block:: sql

   METRICS (
       s.total_qty AS SUM(s.quantity),
       s.rolling_avg AS
           AVG(total_qty) OVER (PARTITION BY EXCLUDING date
               ORDER BY date NULLS LAST
               RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW)
   )

``PARTITION BY`` (without ``EXCLUDING``) specifies an explicit, fixed set of partition dimensions. The partition set is always exactly the listed dimensions, regardless of what other dimensions are queried:

.. code-block:: sql

   METRICS (
       s.total_qty AS SUM(s.quantity),
       s.store_avg AS
           AVG(total_qty) OVER (PARTITION BY store ORDER BY date NULLS LAST)
   )

See :ref:`howto-window-metrics` for details on both modes.

.. warning::

   ``NON ADDITIVE BY`` and ``OVER`` cannot be combined on the same metric. A metric is either semi-additive or a window metric, not both.

**Parameters:**

- ``PRIVATE``, optional. When present, the metric cannot be queried directly but can be referenced by derived metric expressions.
- ``<alias>.<metric_name>``, table alias and metric name for base metrics.
- ``<metric_name>``, name only (no alias) for derived metrics.
- ``USING (<rel_name>, ...)``, optional. Specifies which named relationship(s) this metric traverses. Used to disambiguate when a dimension comes from a role-playing table.
- ``NON ADDITIVE BY (<dim_name> [ASC|DESC] [NULLS FIRST|NULLS LAST], ...)``, optional. Declares the metric as semi-additive, specifying which dimensions trigger snapshot selection.
- ``<aggregate_expression>``, for base metrics: any expression containing aggregate functions.
- ``<expression>``, for derived metrics: an expression referencing other metric names (no aggregate functions).
- ``<window_func>(<inner_metric>, ...) OVER (...)``, for window metrics: wraps another metric in a window function.
- ``COMMENT = '<text>'``, optional. A human-readable description.
- ``WITH SYNONYMS = ('<synonym>', ...)``, optional. Alternative names for discoverability.
- ``[WITH] TAG (<tag_name> = '<value>', ...)``, optional. Name/value metadata tags. See :ref:`Tags <ref-create-tags>`.

**Validation rules:**

- Metric names must be unique within the view (case-insensitive, quoted or not -- DuckDB treats double-quoted identifiers as case-insensitive too), across both base and derived metrics. See :ref:`ref-err-name-uniqueness`.
- A metric name cannot collide with any dimension name (case-insensitive, quoted or not -- DuckDB treats double-quoted identifiers as case-insensitive too). See :ref:`ref-err-name-uniqueness`.
- Derived metrics must not contain aggregate functions.
- A prepared statement parameter (``?``, ``$1``, ``$name``) in a metric expression, or in a window metric's arguments, ``ORDER BY`` or frame, is rejected. See :ref:`ref-error-messages`.
- Circular derived metric references are rejected.
- ``USING`` relationship names must match declared relationships. Names match the way DuckDB matches identifiers: case-insensitively, whether or not they are quoted, so ``USING (dep_airport)`` names a relationship declared as ``"Dep_Airport"``. Two relationships whose names differ only in case or quoting (``dep`` and ``"dep"``) are the same name. A name keeps its parts, though: the quoted ``"x.y"`` is one identifier containing a dot, so it is a different name from the two-part ``x.y`` (which ``"x"."y"`` also spells).
- ``NON ADDITIVE BY`` dimension names must match declared dimensions.
- Window metric inner metric name must match a declared metric.
- Window metric ``EXCLUDING`` dimension names must match declared dimensions.
- Window metric ``PARTITION BY`` dimension names must match declared dimensions.
- Window metric ``ORDER BY`` dimension names must match declared dimensions.
- ``NON ADDITIVE BY`` and ``OVER`` cannot both appear on the same metric.
- ``OVER`` cannot appear on a derived metric (one without a table alias). Only qualified metrics (``alias.name``) can use ``OVER``.

**Reported data type:**

As with dimensions, ``CREATE`` does not record an output type for a metric. The ``data_type`` column reported by :ref:`SHOW SEMANTIC METRICS <ref-show-semantic-metrics>` and :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` is worked out when that command runs and is the type of the aggregate, not of its input: ``COUNT`` is ``BIGINT``, ``AVG`` is ``DOUBLE``, and ``SUM`` of an integer column is ``HUGEINT``, as DuckDB types the aggregate and :ref:`semantic_view() <ref-semantic-view-function>` returns it. See :ref:`Reported Data Types <explanation-sf-data-types>`.


.. _ref-create-materializations:

MATERIALIZATIONS
-----------------

.. versionadded:: 0.7.0

Declares named materializations that map pre-aggregated tables to the dimensions and metrics they cover. When a query's requested dimensions and metrics exactly match a materialization, the extension routes to the pre-aggregated table instead of expanding raw sources.

.. code-block:: sql

   MATERIALIZATIONS (
       region_agg AS (
           TABLE revenue_by_region,
           DIMENSIONS (region),
           METRICS (revenue, order_count)
       ),
       global_agg AS (
           TABLE global_totals,
           METRICS (revenue)
       )
   )

**Parameters:**

- ``<mat_name>``, a unique name identifying this materialization.
- ``TABLE <table_name>``, the physical table containing pre-aggregated data. Supports catalog-qualified names (``catalog.schema.table``). The table is not validated for existence at define time (it may be created later by external tools like dbt).
- ``DIMENSIONS (<dim_name>, ...)``, optional. Dimension names from the view's ``DIMENSIONS`` clause that the materialization table covers.
- ``METRICS (<metric_name>, ...)``, optional. Metric names from the view's ``METRICS`` clause that the materialization table covers.

At least one of ``DIMENSIONS`` or ``METRICS`` must be specified in each materialization entry.

**Routing behavior:**

- Routing uses **exact match**: both the dimension set and metric set must exactly equal the query's requested sets (case-insensitive comparison).
- Materializations are scanned in **definition order**; the **first match wins**.
- Semi-additive metrics (``NON ADDITIVE BY``) and window metrics (``OVER``) are **always excluded** from routing.
- When no match is found, the extension falls back to standard expansion.

See :ref:`howto-materializations` for a detailed guide.

**Validation rules:**

- Materialization names must be unique within a view.
- Dimension names must match declared dimensions in the view's ``DIMENSIONS`` clause.
- Metric names must match declared metrics in the view's ``METRICS`` clause.
- Each materialization must specify at least one of ``DIMENSIONS`` or ``METRICS``.


.. _ref-create-from-yaml:

FROM YAML
---------

.. versionadded:: 0.7.0

Creates a semantic view from a YAML definition instead of the keyword-based ``AS`` body. Two forms are supported:

**Inline YAML (dollar-quoted):**

.. code-block:: duckdb-sql

   CREATE SEMANTIC VIEW order_metrics FROM YAML $$
   tables:
     - alias: o
       table: orders
       pk_columns:
         - id
   dimensions:
     - name: region
       expr: o.region
       source_table: o
   metrics:
     - name: revenue
       expr: SUM(o.amount)
       source_table: o
   $$;

The YAML content is enclosed in dollar-quote delimiters. Tagged dollar-quoting (``$yaml$...$yaml$``) is also supported.

**YAML from file:**

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics FROM YAML FILE '/path/to/definition.yaml';

The file path must be single-quoted. DuckDB reads the file contents and parses as YAML.

See :ref:`howto-yaml-definitions` for a detailed workflow guide.

**YAML size limit:** YAML definitions are capped at 1 MiB. Definitions exceeding this limit are rejected with an error.


.. _ref-create-examples:

Examples
========

**Single-table view:**

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region AS o.region
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

**Multi-table star schema:**

.. code-block:: sql

   CREATE SEMANTIC VIEW analytics AS
   TABLES (
       o AS orders    PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id),
       p AS products  PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       order_product  AS o(product_id)  REFERENCES p
   )
   DIMENSIONS (
       c.customer AS c.name,
       p.product  AS p.name,
       o.region   AS o.region
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   );

**Full feature set (facts, derived metrics, USING):**

.. code-block:: sql

   CREATE SEMANTIC VIEW flight_analytics AS
   TABLES (
       f AS flights  PRIMARY KEY (flight_id),
       a AS airports PRIMARY KEY (airport_code)
   )
   RELATIONSHIPS (
       dep_airport AS f(departure_code) REFERENCES a,
       arr_airport AS f(arrival_code)   REFERENCES a
   )
   FACTS (
       f.is_international AS CASE WHEN f.departure_country != f.arrival_country
                             THEN 1 ELSE 0 END
   )
   DIMENSIONS (
       a.city    AS a.city,
       f.carrier AS f.carrier
   )
   METRICS (
       f.departures            USING (dep_airport) AS COUNT(*),
       f.arrivals              USING (arr_airport) AS COUNT(*),
       f.international_flights AS SUM(f.is_international),
       total_flights           AS departures + arrivals
   );

**With metadata annotations:**

.. code-block:: sql

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       li AS line_items PRIMARY KEY (id) COMMENT = 'Transaction line items'
   )
   FACTS (
       li.net_price AS li.extended_price * (1 - li.discount)
           COMMENT = 'Price after discount' WITH SYNONYMS = ('discounted_price')
   )
   DIMENSIONS (
       li.region AS li.region COMMENT = 'Sales territory'
   )
   METRICS (
       li.total_net AS SUM(li.net_price) COMMENT = 'Net revenue'
   );

**Semi-additive metric:**

.. code-block:: sql

   CREATE SEMANTIC VIEW account_metrics AS
   TABLES (
       a AS accounts PRIMARY KEY (id)
   )
   DIMENSIONS (
       a.customer_id AS a.customer_id,
       a.report_date AS a.report_date
   )
   METRICS (
       a.total_balance NON ADDITIVE BY (report_date) AS SUM(a.balance)
   );

**Window metric with PARTITION BY EXCLUDING:**

.. code-block:: sql

   CREATE SEMANTIC VIEW store_analytics AS
   TABLES (
       s AS sales PRIMARY KEY (id)
   )
   DIMENSIONS (
       s.store AS s.store,
       s.date  AS s.sale_date
   )
   METRICS (
       s.total_qty AS SUM(s.quantity),
       s.rolling_avg AS
           AVG(total_qty) OVER (PARTITION BY EXCLUDING date
               ORDER BY date NULLS LAST)
   );

**Window metric with explicit PARTITION BY:**

.. code-block:: sql

   CREATE SEMANTIC VIEW store_analytics AS
   TABLES (
       s AS sales PRIMARY KEY (id)
   )
   DIMENSIONS (
       s.store  AS s.store,
       s.date   AS s.sale_date,
       s.region AS s.region
   )
   METRICS (
       s.total_qty AS SUM(s.quantity),
       s.store_avg AS
           AVG(total_qty) OVER (PARTITION BY store
               ORDER BY date NULLS LAST)
   );

**With materializations:**

.. versionadded:: 0.7.0

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region AS o.region,
       o.status AS o.status
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   )
   MATERIALIZATIONS (
       region_agg AS (
           TABLE revenue_by_region,
           DIMENSIONS (region),
           METRICS (revenue, order_count)
       ),
       region_status_agg AS (
           TABLE revenue_by_region_status,
           DIMENSIONS (region, status),
           METRICS (revenue)
       )
   );

**From inline YAML:**

.. versionadded:: 0.7.0

.. code-block:: duckdb-sql

   CREATE SEMANTIC VIEW order_metrics FROM YAML $$
   tables:
     - alias: o
       table: orders
       pk_columns:
         - id
   dimensions:
     - name: region
       expr: o.region
       source_table: o
   metrics:
     - name: revenue
       expr: SUM(o.amount)
       source_table: o
   $$;

**From YAML file:**

.. versionadded:: 0.7.0

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics FROM YAML FILE '/path/to/definition.yaml';
