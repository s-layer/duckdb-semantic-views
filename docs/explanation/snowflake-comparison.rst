.. meta::
   :description: Feature-by-feature comparison with Snowflake semantic views SQL DDL, covering syntax alignment, key differences, and unsupported features

.. _explanation-snowflake:

======================
Snowflake Comparison
======================

DuckDB Semantic Views is modeled on Snowflake's ``CREATE SEMANTIC VIEW`` SQL DDL interface. If you have used Snowflake semantic views, much of the syntax and concept model will be familiar. This page maps the key concepts and calls out the differences.

.. note::

   Snowflake has two distinct interfaces for semantic views: the SQL DDL (``CREATE SEMANTIC VIEW``)
   and the older YAML spec (``CREATE SEMANTIC VIEW FROM YAML``, designed for Cortex Analyst).
   All comparisons on this page target the SQL DDL interface only. The YAML spec includes
   concepts like ``time_dimensions``, ``custom_instructions``, and ``sample_values`` that
   exist to serve the AI SQL generation layer and have no equivalent in the SQL DDL.


.. _explanation-sf-concepts:

Concept Mapping
===============

.. list-table::
   :header-rows: 1
   :widths: 30 35 35

   * - Concept
     - Snowflake SQL DDL
     - DuckDB Semantic Views
   * - Define a semantic view
     - ``CREATE SEMANTIC VIEW``
     - ``CREATE SEMANTIC VIEW``
   * - Table declarations
     - ``TABLES`` clause with aliases
     - ``TABLES`` clause with aliases and optional ``PRIMARY KEY``
   * - Relationships
     - ``RELATIONSHIPS`` clause with FK REFERENCES
     - ``RELATIONSHIPS`` clause with FK REFERENCES
   * - Dimensions
     - ``DIMENSIONS`` clause
     - ``DIMENSIONS`` clause
   * - Metrics (measures)
     - ``METRICS`` clause
     - ``METRICS`` clause
   * - Reusable row-level expressions
     - ``FACTS`` clause
     - ``FACTS`` clause (queryable via ``facts := [...]``)
   * - Metric composition
     - Derived metrics (metric referencing other metrics)
     - Derived metrics (same pattern)
   * - Semi-additive metrics
     - ``SEMI ADDITIVE`` / ``NON ADDITIVE BY``
     - ``NON ADDITIVE BY`` (see :ref:`howto-semi-additive`)
   * - Window function metrics
     - ``OVER`` clause with ``PARTITION BY EXCLUDING``
     - ``OVER`` clause with ``PARTITION BY EXCLUDING`` (see :ref:`howto-window-metrics`)
   * - Metadata annotations
     - ``COMMENT``, ``WITH SYNONYMS``
     - ``COMMENT``, ``WITH SYNONYMS`` (see :ref:`howto-metadata-annotations`)
   * - Access modifiers
     - ``PRIVATE`` / ``PUBLIC`` on metrics and facts; `"You cannot mark a dimension as private. Dimensions are always public." <https://docs.snowflake.com/en/sql-reference/sql/create-semantic-view>`_
     - Same rule: ``PRIVATE`` / ``PUBLIC`` on metrics and facts, ``PRIVATE`` rejected on dimensions. ``PUBLIC`` is accepted on a dimension as an explicit no-op, since it asserts what is already true
   * - Materializations / pre-aggregation
     - Not part of Snowflake's ``CREATE SEMANTIC VIEW`` DDL
     - ``MATERIALIZATIONS`` clause for routing to pre-aggregated tables (see :ref:`howto-materializations`)
   * - Query interface
     - Direct SQL with semantic resolution
     - :ref:`semantic_view() <ref-semantic-view-function>` table function
   * - Wildcard selection
     - ``alias.*`` in query parameters
     - ``alias.*`` in ``dimensions``, ``metrics``, ``facts`` parameters (see :ref:`howto-wildcard-selection`)
   * - View inspection
     - ``DESCRIBE`` / ``DESC SEMANTIC VIEW``
     - :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` (``DESC`` abbreviation also accepted)
   * - List views
     - ``SHOW SEMANTIC VIEWS``
     - :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>`
   * - Terse view listing
     - ``SHOW TERSE SEMANTIC VIEWS``
     - :ref:`SHOW TERSE SEMANTIC VIEWS <ref-show-semantic-views>`
   * - Column listing
     - ``SHOW COLUMNS IN SEMANTIC VIEW``
     - :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>`
   * - Filter by scope
     - ``IN SCHEMA`` / ``IN DATABASE``
     - ``IN SCHEMA`` / ``IN DATABASE`` on SHOW commands
   * - Retrieve DDL text
     - ``GET_DDL('SEMANTIC_VIEW', ...)``
     - :ref:`GET_DDL('SEMANTIC_VIEW', ...) <ref-get-ddl>`
   * - Alter a view
     - ``ALTER SEMANTIC VIEW``
     - :ref:`ALTER SEMANTIC VIEW <ref-alter-semantic-view>` (RENAME TO, SET COMMENT, UNSET COMMENT)
   * - Drop a view
     - ``DROP SEMANTIC VIEW``
     - :ref:`DROP SEMANTIC VIEW <ref-drop-semantic-view>`


.. _explanation-sf-syntax:

Syntax Alignment
================

The DDL syntax is intentionally close to Snowflake's. The clause order (``TABLES``, ``RELATIONSHIPS``, ``FACTS``, ``DIMENSIONS``, ``METRICS``) matches Snowflake, and the entry syntax within each clause follows the same pattern.

.. note::

   **Syntax conveniences for porting.**

   .. versionadded:: 0.11.0

   Several Snowflake spellings are accepted to reduce friction when porting DDL: the
   table alias in ``TABLES`` is optional (``TABLES (orders PRIMARY KEY (id))``, matching
   Snowflake's ``[alias AS] table``); a view-level ``COMMENT = '...'`` may appear in the
   trailing position after the last clause; ``PUBLIC`` is accepted on dimensions;
   ``WITH SYNONYMS (...)`` is accepted without the ``=``; and ``DESC SEMANTIC VIEW`` is
   accepted as an abbreviation of ``DESCRIBE SEMANTIC VIEW``. See
   :ref:`ref-create-semantic-view`.

.. tab-set::
   :sync-group: platform

   .. tab-item:: Snowflake
      :sync: snowflake

      .. code-block:: sql

         -- Snowflake has no AS keyword before the body clauses.
         CREATE SEMANTIC VIEW analytics
         TABLES (
             o AS orders,
             c AS customers PRIMARY KEY (id)
         )
         RELATIONSHIPS (
             order_customer AS o(customer_id) REFERENCES c
         )
         DIMENSIONS (
             c.customer_name AS c.name,
             o.region AS o.region
         )
         METRICS (
             o.revenue AS SUM(o.amount)
         );

   .. tab-item:: DuckDB Semantic Views
      :sync: duckdb

      .. code-block:: sql

         CREATE SEMANTIC VIEW analytics AS
         TABLES (
             o AS orders    PRIMARY KEY (id),
             c AS customers PRIMARY KEY (id)
         )
         RELATIONSHIPS (
             order_customer AS o(customer_id) REFERENCES c
         )
         DIMENSIONS (
             c.customer_name AS c.name,
             o.region AS o.region
         )
         METRICS (
             o.revenue AS SUM(o.amount)
         );


.. _explanation-sf-differences:

Key Differences
===============

Primary Key Declarations
------------------------

.. note::

   ``PRIMARY KEY`` declarations in the ``TABLES`` clause are optional at the syntax
   level, but any table used as the target of a ``RELATIONSHIPS`` entry must declare
   a ``PRIMARY KEY`` or ``UNIQUE`` key in its own ``TABLES`` entry. An explicit column
   list on the referencing side (``REFERENCES c(id)``) does not replace that
   declaration: it has to name columns the target already declares as its
   ``PRIMARY KEY`` or as one of its ``UNIQUE`` keys.

Both systems take the key from the semantic view definition rather than from the
physical table. Snowflake's ``CREATE SEMANTIC VIEW`` reference describes a
referenced column as `"a column identified with the PRIMARY KEY or UNIQUE
constraint in the logical table definition"
<https://docs.snowflake.com/en/sql-reference/sql/create-semantic-view>`_, and
DuckDB Semantic Views applies the same rule. In both, a ``PRIMARY KEY`` in a
semantic view is a **logical assertion you make** about the data, not a physical
constraint imported from the catalog.

.. versionchanged:: 0.10.0

   Automatic PK inference from DuckDB's ``duckdb_constraints()`` catalog was **removed**
   (breaking). Earlier releases imported a native table's physical ``PRIMARY KEY`` at
   ``CREATE`` time when the ``TABLES`` entry declared none; this fallback is gone. You
   must now declare the key explicitly, whether the table is a native DuckDB table or an
   external source. Migration: add a ``PRIMARY KEY (...)`` (or ``UNIQUE (...)``) clause to
   any ``TABLES`` entry that previously relied on the auto-fallback.

.. tip::

   This uniform rule is convenient for data engineers using DuckDB with Iceberg,
   Parquet, CSV, or Postgres sources: those catalogs never surfaced PK/FK metadata
   through ``duckdb_constraints()`` anyway, so declaring keys in the ``TABLES`` clause was
   always required for them. Now native DuckDB tables follow the same explicit-declaration
   rule, so there is one consistent model regardless of data source.

.. code-block:: sql

   -- Every table used as a join target declares its key explicitly,
   -- regardless of whether it is a native DuckDB table or an external source.
   CREATE SEMANTIC VIEW analytics AS
   TABLES (
       o AS orders    PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (c.name AS c.name)
   METRICS (o.revenue AS SUM(o.amount));

If a relationship's target table declares neither key, the extension rejects the
definition at ``CREATE`` time, whether or not the ``REFERENCES`` side lists columns.
With ``c AS customers`` declared without a key in the example above, the error is:

.. code-block:: text

   Parser Error: Table 'c' has no PRIMARY KEY declared but is referenced by FK in 'o'.
   Add PRIMARY KEY (cols) or UNIQUE (cols) to the TABLES clause for c. (v0.10.0:
   physical-catalog PK auto-inference removed -- see CHANGELOG.)

This prevents the extension from synthesizing an incorrect JOIN ON clause.


Query Interface
---------------

.. warning::

   DuckDB Semantic Views queries a semantic view with ``SEMANTIC_VIEW(...)`` or a table function, not direct SQL.

In Snowflake, you can write standard SQL against a semantic view and the system resolves dimensions and metrics. In DuckDB, you name the dimensions and metrics, either with the same :ref:`SEMANTIC_VIEW(...) construct <ref-sv-construct>` Snowflake has, or with the :ref:`semantic_view() <ref-semantic-view-function>` table function it turns into.

.. code-block:: sql

   -- Both: the SEMANTIC_VIEW construct. The view name and the dimension /
   -- metric references are bare identifiers (not string literals), and there
   -- is no comma between the view name and the DIMENSIONS / METRICS keywords.
   SELECT * FROM SEMANTIC_VIEW(
       analytics
       DIMENSIONS orders.region
       METRICS orders.revenue
   );

   -- DuckDB: the same query as a table function with named lists
   SELECT * FROM semantic_view('analytics',
       dimensions := ['orders.region'],
       metrics := ['orders.revenue']
   );

   -- Snowflake: direct SQL with AGG view-defined aggregate function
   -- (NOT currently supported in DuckDB Semantic Views)
   SELECT region, AGG(revenue)
   FROM analytics
   GROUP BY region;


Cardinality Inference
---------------------

Both systems infer cardinality from constraints. In DuckDB Semantic Views, cardinality is inferred from ``PRIMARY KEY`` and ``UNIQUE`` declarations in the ``TABLES`` clause:

- If the FK columns on the "from" side match a PK or UNIQUE constraint, the relationship is one-to-one.
- Otherwise, the relationship is many-to-one (the default).

The extension uses inferred cardinality for :ref:`fan trap detection <howto-fan-traps>`.


.. _explanation-sf-expression-scope:

What an Expression May Reference
--------------------------------

Both systems scope a member expression to its **own** logical table. Snowflake's
validation rules state that `expressions can refer to base table columns or
other expressions on the same logical table
<https://docs.snowflake.com/en/user-guide/views-semantic/validation-rules>`_,
and that they "cannot refer to base table columns from other tables". To reach
another table you declare a relationship, define a fact on the source table, and
refer to *that fact* from the connected table. The rule here is the same.

A **named fact on another table** -- Snowflake's supported cross-table form --
is at parity: the fact's expression is inlined at its reference site and its
table is joined, so ``o.net_total AS SUM(o.amount - c.cust_discount)`` computes
over the joined relation. One safety rule is added on top of Snowflake's, for a
hazard its single-grain model does not face here: if the fact's table *fans* the
referencing member's -- a fact on a child table, reached across a one-to-many
edge -- joining it would multiply the member's rows, so the query is rejected
with a fan trap error rather than answered with an inflated aggregate. See
:ref:`howto-facts-cross-table`.

A **raw column of another table** (``o.margin AS o.amount - c.discount``) is
rejected at ``CREATE`` in both systems, with an error naming the rule and
pointing at the fact-based alternative. A qualifier that names no declared table
at all is left to DuckDB, which is the right place for a struct path, a bound
parameter, or a typo to be resolved.

.. versionchanged:: 0.12.0
   Before, the definition was accepted and the reference surfaced at query time
   as a DuckDB unknown-alias error -- the same rule, enforced later.

Derived metrics are unaffected: a metric that references metrics on other tables
(``m AS t1.metric_1 + t2.metric_2``) is supported in both systems, and is
computed per grain -- see :ref:`explanation-sf-metric-grain` below.


.. _explanation-sf-data-types:

Reported Data Types
-------------------

Snowflake populates the ``data_type`` column of
:ref:`SHOW SEMANTIC DIMENSIONS <ref-show-semantic-dimensions>` /
:ref:`METRICS <ref-show-semantic-metrics>` / :ref:`FACTS <ref-show-semantic-facts>`
(and the ``DATA_TYPE`` rows of :ref:`DESCRIBE <ref-describe-semantic-view>`) with the
member's actual data type.

.. versionchanged:: 0.14.0

   ``data_type`` is populated again. It had been empty for every view created
   since v0.10.0; views created in that window report their types too, with no
   need to re-create them.

The same holds here. The type is not stored at ``CREATE``; it is worked out
when the ``SHOW`` / ``DESCRIBE`` command runs, by preparing the query
:ref:`semantic_view() <ref-semantic-view-function>` would run for the member and
reading its column type. So ``data_type`` follows the current source columns, and it
is always the type a query returns for that member:

- **DuckDB type names.** Types are spelled the way DuckDB spells them
  (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``, ``VARCHAR[]``), not as Snowflake would
  (``TEXT``, ``NUMBER(38,0)``).
- **DuckDB's own types.** ``SUM`` over an integer column is a ``HUGEINT``, and
  ``ENUM`` / ``STRUCT`` / ``MAP`` columns keep their types, both in what
  ``semantic_view()`` returns and in ``data_type``.
- **When a member cannot be typed** -- its source table or column was dropped or
  renamed after ``CREATE`` -- ``data_type`` is empty for that member, and the command
  still succeeds for the rest. Querying the member reports the underlying error.
  The same goes for a metric that cannot be queried on its own because it would
  aggregate across a fan-out (see :ref:`howto-fan-traps`): its ``data_type`` is
  empty, and querying it reports the fan trap.
- **Over a temporary table, or a table changed in your open transaction,** members
  are typed as ``semantic_view()`` reads them there: the command runs in your
  session and transaction.
- **PRIVATE members** are typed too, though they cannot be queried directly.
- **Role-played members** -- on a table that several relationships point at -- are
  typed too, although querying one needs a metric whose ``USING`` names the role. The
  type is the same whichever role is used.

The cross-view forms (``SHOW SEMANTIC METRICS`` without ``IN``, or with
``IN SCHEMA``) type the members of every view, so they take longer as the catalog
grows: roughly a millisecond or two per view.


.. _explanation-sf-metric-grain:

Metric Grain
------------

.. versionchanged:: 0.12.0

Like Snowflake, each metric is computed **at the grain of its own logical
table**. When a query's metrics sit at different grains -- a metric on a parent
table alongside one on the base table, two metrics on different child tables, or
a single derived metric fusing two grains -- each is aggregated separately over
its own table and the results are joined on the queried dimensions. A metric on
a parent table is therefore not multiplied by the number of child rows, and a
parent row with no children is not dropped.

Before v0.12.0 the generated SQL was always anchored ``FROM <base table>``, so
these queries were rejected with a fan trap error rather than silently inflated.

.. versionchanged:: 0.14.0

   This now holds for a query with a single grain too. A metric on a child of
   the base table, queried on its own, is aggregated from the child table and
   joined out to the dimensions, as in Snowflake -- it used to be computed over
   ``FROM <base table> LEFT JOIN <child>``. So a child row whose foreign key is
   NULL is counted under a ``NULL`` dimension value, a parent with no children
   has no row (or a ``NULL`` child metric when a parent-table metric brings its
   row in), and a metric's value never depends on what else is queried. See
   :ref:`explanation-grain-orphans`. Only a query whose metrics all live on the
   base table is still a single base-anchored ``SELECT``; there the base table
   is every metric's own table, so the two coincide.

Four boundaries apply:

- A **dimension below a metric's grain** (``SUM(customers.balance)`` grouped by
  an order-grain dimension) is rejected in both systems. Snowflake's rule is
  that `the logical table for the dimension must be related to the logical
  table for the metric
  <https://docs.snowflake.com/en/user-guide/views-semantic/querying>`_ and must
  have "an equal or lower level of granularity than the logical table for the
  metric"; the extension's ``fan trap detected`` error enforces the same condition. Per-grain
  aggregation does not make these answerable -- the metric's rows genuinely fan
  across the dimension's values, so there is no single correct value per group.
- A **window metric** whose inner aggregate lives on a non-base table is computed
  at its own grain -- the inner aggregation is anchored at that table (a parent
  or a child of the base table), so it is neither inflated nor thinned by a
  base-table join. Window metrics whose inner aggregates sit
  at *different* grains still error, as those grains would need joining before the
  window runs. The exception is a pair joined one-to-one, where nothing fans out:
  it is answered from the base table instead (see :ref:`explanation-grain-refused`).
- An **active semi-additive metric** snapshots at its own grain, whether it is
  the query's only grain or one of several: the ``RANK()`` runs over the metric's
  own table rather than a base-anchored join, so the winning snapshot row is not
  added once per base-table row. A ``NON ADDITIVE BY`` dimension declared on
  another logical table is joined in so its ordering still binds. This matches
  Snowflake: probed directly, it returns each metric at its own grain, with
  the snapshot selection happening inside the semi-additive metric's own-grain
  aggregation rather than over the joined row set. Snowflake also accepts a
  ``NON ADDITIVE BY`` dimension declared on *another* logical table, provided the
  reference is qualified (``NON ADDITIVE BY (s.report_date)``); the bare form
  resolves only within the metric's own table. Both forms are accepted here too.
- Multi-grain queries **reaching a role-played table** are computed when a
  co-queried metric's ``USING`` names the role: each grain CTE joins that
  relationship under its scoped alias and groups by the dimension bound to it,
  as the single-grain path already did. Without ``USING`` the query keeps the
  fan trap error, since a grain CTE would otherwise choose among the
  relationship instances by declaration order. This covers only a queried
  dimension's own table -- a :ref:`where_clause <ref-sv-pre-agg-filtering>` member on a role-played table, a
  metric aggregated at one, or a table reachable only *through* one still error.
  A definition that merely *declares* role-playing does not lose per-grain
  emission: the test is what the query reaches, so unrelated grains in the same
  view are computed normally. The one pairing that still declines is a role-played
  dimension queried *together with* an active semi-additive metric: a snapshot
  group cannot carry a role, so the two grains would bind different instances of
  the same dimension and the outer join would compare them.


USING RELATIONSHIPS
-------------------

Both systems support ``USING`` on metrics to select which relationship path a metric traverses. The syntax is identical:

.. code-block:: sql

   METRICS (
       f.departures USING (dep_airport) AS COUNT(*)
   )


Facts Query Mode
----------------

.. versionadded:: 0.6.0

Both systems allow facts to be queried directly as row-level columns. In Snowflake, facts appear in the ``SEMANTIC_VIEW()`` query function. In DuckDB Semantic Views, use the ``facts`` parameter:

.. code-block:: sql

   -- DuckDB: query facts as row-level columns
   SELECT * FROM semantic_view('analytics',
       dimensions := ['region'],
       facts := ['net_price']
   );

.. warning::

   In both systems, facts and metrics cannot be combined in the same query. Use ``facts := [...]`` OR ``metrics := [...]``, not both.


Semi-Additive and Window Metrics
---------------------------------

.. versionadded:: 0.6.0

Both systems support semi-additive metrics (``NON ADDITIVE BY``) and window function metrics (``OVER`` with ``PARTITION BY EXCLUDING``). The syntax is aligned:

.. code-block:: sql

   -- Semi-additive: last balance per account, summed across customers
   METRICS (
       a.balance NON ADDITIVE BY (date_dim) AS SUM(a.amount)
   )

   -- Window: rolling average excluding region from partition
   METRICS (
       o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY month)
   )

Like Snowflake, the default (ascending) direction selects the **latest** snapshot and ``DESC`` selects the earliest (see :ref:`howto-semi-additive`).

The behavioral differences are:

- ``NON ADDITIVE BY`` dimensions must be declared in the view's ``DIMENSIONS`` clause. Snowflake validates against its own catalog.
- Window metrics and ``NON ADDITIVE BY`` cannot be combined on the same metric (mutually exclusive).
- NULL keys in a non-additive dimension: the default NULLS placement follows the direction (``ASC`` → ``NULLS LAST``, ``DESC`` → ``NULLS FIRST``), matching DuckDB/Snowflake. Under ``NULLS LAST`` a NULL key never wins (the latest/earliest *real* snapshot is selected); under ``NULLS FIRST`` a NULL key wins. Add an explicit ``NULLS LAST`` to exclude NULL keys regardless of direction.
- Window metrics cannot be mixed with aggregate metrics in the same query.


Materializations
-----------------

.. versionadded:: 0.7.0

Snowflake's ``CREATE SEMANTIC VIEW`` SQL DDL does not include a materializations or pre-aggregation concept. Pre-aggregation in Snowflake is handled through separate materialized views.

DuckDB Semantic Views introduces a ``MATERIALIZATIONS`` clause that declares mappings from pre-aggregated tables to the dimensions and metrics they cover. When a query exactly matches a materialization, the extension routes to the pre-aggregated table instead of expanding raw sources. See :ref:`howto-materializations` for details.


Transactional DDL
-----------------

.. versionadded:: 0.8.0

This is a divergence. In Snowflake, `each DDL statement executes as a separate transaction <https://docs.snowflake.com/en/sql-reference/transactions#ddl>`_: a DDL statement issued inside an open transaction implicitly commits that transaction first, then runs and commits on its own. A ``CREATE``, ``ALTER`` or ``DROP SEMANTIC VIEW`` therefore cannot be rolled back there.

DuckDB Semantic Views runs these statements inside the caller's transaction, the way DuckDB runs its own DDL. ``BEGIN ... ROLLBACK`` discards an uncommitted ``CREATE``, ``ALTER`` or ``DROP SEMANTIC VIEW``, and a ``COMMIT`` publishes the DDL together with any data changes in the same transaction.

Some DuckDB-specific details to know before you build on it:

- Reads inside an open transaction, ``DESCRIBE SEMANTIC VIEW`` and the ``SHOW SEMANTIC ...`` family included, see that transaction's uncommitted ``CREATE``, ``ALTER`` and ``DROP``. Other connections see them once you commit.
- ``CREATE SEMANTIC VIEW IF NOT EXISTS`` cannot fully absorb a race between two connections both running it against the same database at the same moment -- one will succeed and the other will see a constraint or commit-conflict error. On a single connection, ``IF NOT EXISTS`` is reliable.
- The non-``IF EXISTS`` ``DROP`` and ``ALTER`` forms raise ``semantic view '<name>' does not exist`` when the view is absent at check time, instead of silently no-opping. The existence check and the write are atomic only inside an explicit transaction; under autocommit a drop committed by another writer in the window between them is not detected. Wrap the DDL in ``BEGIN ... COMMIT`` when you need the check to be reliable under concurrency.

See :ref:`explanation-transactional-ddl` for the full mechanism and worked examples.


Schema Scoping and Name Resolution
----------------------------------

Both systems scope a semantic view to a schema, so ``analytics.sales`` and ``staging.sales`` are two different views and a ``<schema>.`` qualifier on ``CREATE`` / ``DROP`` / ``ALTER`` decides which one a statement means.

Where they differ is how an **unqualified** reference is resolved. Snowflake uses the session's current database and schema; DuckDB Semantic Views uses DuckDB's own rule, the session ``search_path`` -- the first schema on it holding a view of that name wins, so ``SET search_path = 'staging'`` makes a bare ``sales`` mean ``staging.sales``. A view that is the only one of its name resolves whether or not its schema is on the path.

A name that exists only in schemas *off* the path is a miss. The error names the schemas it does live in and the path that was searched, rather than reporting a bare "does not exist" for a view ``SHOW SEMANTIC VIEWS`` plainly lists.

Two DuckDB-specific consequences:

- **Identifiers are case-insensitive on both sides of the dot**, quoted or not, following DuckDB rather than Snowflake -- ``analytics.sales``, ``ANALYTICS.SALES`` and ``"Analytics"."Sales"`` all name the same view. Snowflake would treat the quoted spellings as case-sensitive. See :ref:`ref-create-semantic-view` for the full identifier rule.
- **One catalog per database.** A ``<database>.`` prefix that names a database other than the session's is rejected: the extension manages a single catalog, in the database it was loaded into.


.. _explanation-sf-not-supported:

Feature Parity Notes
====================

Snowflake ``CREATE SEMANTIC VIEW`` features that are commonly asked about, and where each one stands. Rows marked **Supported** have since landed; the rest are unimplemented, out of scope, or not planned, with the reason given:

.. list-table::
   :header-rows: 1
   :widths: 40 60

   * - Snowflake Feature
     - Status
   * - Direct SQL query interface
     - Not planned; query with the :ref:`SEMANTIC_VIEW(...) construct <ref-sv-construct>` or the :ref:`semantic_view() <ref-semantic-view-function>` table function
   * - Pre-aggregation ``WHERE`` -- ``SEMANTIC_VIEW( v METRICS ... DIMENSIONS ... WHERE <predicate> )``, where the predicate `may refer only to dimensions, facts, and expressions over them <https://docs.snowflake.com/en/sql-reference/constructs/semantic_view>`_ and "is applied before the metrics are computed"
     - **Supported** as the :ref:`where_clause <ref-sv-pre-agg-filtering>` named parameter, written ``where_clause := '...'`` (``where`` is a reserved word in DuckDB's named-parameter position, so it cannot be spelled ``where :=``). The predicate names declared dimensions and facts, is substituted to their expressions, and is applied before aggregation on every emission path -- before the ``GROUP BY`` when the query is a single ``SELECT`` (one grain, dimensions only, or facts), inside *each* per-grain aggregate for multi-grain queries, inside the snapshot selection before the ``RANK`` for semi-additive metrics, and inside the inner aggregation before the window function. So "revenue for orders shipped after X" recomputes each group over the matching rows. Referencing a metric is rejected, matching Snowflake, and members the predicate names participate in the same reachability and fan-out checks as queried dimensions.
   * - Query-time scalar expressions -- ``SEMANTIC_VIEW( v DIMENSIONS DATE_PART('year', orders.order_date) AS year )``, where a ``DIMENSIONS`` / ``FACTS`` item `may be a scalar expression over dimensions or facts of the same logical table <https://docs.snowflake.com/en/sql-reference/constructs/semantic_view>`_, a ``METRICS`` item may be an expression over metrics, and any item may take ``AS <alias>``
     - **Supported.** An item may be an expression, in the construct and in a ``semantic_view()`` list (``'DATE_PART(''year'', o.order_date) AS year'``), with Snowflake's rules: a ``DIMENSIONS`` or ``FACTS`` expression uses dimensions and facts of one table, and a ``METRICS`` expression uses metrics and aggregates of the dimensions and facts of one table. An expression item is planned as a member of its table, so ``COUNT(DISTINCT ...)``, ratios and per-grain metrics come out right, and the fan-trap checks apply to it. Any item may take ``AS <alias>`` (optional in the construct, as in Snowflake). See :ref:`ref-sv-expression-items`.
   * - Named filters -- ``LABELS = (FILTER)`` on a fact or dimension resolving to ``BOOLEAN``, referenced bare in a query's ``WHERE`` (`Snowflake GA May 2026 <https://docs.snowflake.com/en/user-guide/views-semantic/filters>`_)
     - **Supported.** ``LABELS = (FILTER)`` is accepted on a fact or dimension entry, in any order among ``COMMENT`` / ``WITH SYNONYMS``, and survives ``GET_DDL``, YAML export, and ``DESCRIBE`` (as a ``LABELS`` property row valued ``["FILTER"]``). Referencing the member bare in ``where_clause`` works via the pre-aggregation predicate above -- a filter is an ordinary boolean member, so the label declares *intent* and drives introspection rather than gating resolution. The ``BOOLEAN`` requirement is enforced by DuckDB's binder at query time, not at ``CREATE``: typing an arbitrary expression needs a binder, so a non-boolean filter raises DuckDB's own type error when queried. Labels other than ``FILTER`` are rejected rather than silently dropped.
   * - Column-level security
     - Out of scope; DuckDB handles access control
   * - ``ASOF`` / temporal relationships
     - Not planned; standard equi-joins cover most use cases
   * - ``CREATE OR ALTER``, tags / other ``LABELS`` values, ``MAX_STALENESS``, and the ``AI_*`` / ``COPILOT`` clauses
     - Not planned; these are Snowflake-catalog or Cortex-specific and have no DuckDB equivalent


.. _explanation-sf-yaml:

A Note on Snowflake's YAML Spec
================================

Snowflake's `YAML-based semantic view definition <https://docs.snowflake.com/en/user-guide/views-semantic/semantic-view-yaml-spec>`_ (``CREATE SEMANTIC VIEW FROM YAML``) is a separate interface designed for Cortex Analyst, Snowflake's AI SQL generation layer. The YAML spec includes concepts that do not exist in the SQL DDL:

- ``time_dimensions`` with granularity controls (the SQL DDL uses regular dimensions with ``date_trunc()``)
- ``custom_instructions`` for AI prompt tuning
- ``sample_values`` for AI context

DuckDB Semantic Views supports YAML definition import (``FROM YAML``) and export (:ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>`), but these use the extension's own YAML schema -- not Snowflake's Cortex Analyst YAML spec. The DuckDB YAML format is a serialization of the same model used by the SQL DDL (tables, relationships, facts, dimensions, metrics, materializations). It is designed for version control, migration, and sharing -- not for AI prompt tuning. Comparisons against Snowflake YAML-spec-only features remain not applicable.

See :ref:`howto-yaml-definitions` for the DuckDB YAML workflow.
