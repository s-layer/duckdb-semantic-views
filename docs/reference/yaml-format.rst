.. meta::
   :description: Complete field-by-field reference for the YAML schema accepted by CREATE SEMANTIC VIEW FROM YAML

.. _ref-yaml-format:

======================
YAML Definition Format
======================

Specification of the YAML schema accepted by ``CREATE SEMANTIC VIEW ... FROM YAML``. Every semantic view definition -- whether created from inline YAML, a YAML file, or exported via :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>` -- follows this format.

.. versionadded:: 0.7.0

YAML keys differ from the SQL clause names in some cases:

.. list-table::
   :header-rows: 1
   :widths: 30 30 40

   * - SQL Clause
     - YAML Key
     - Notes
   * - ``TABLES``
     - ``tables``
     -
   * - ``RELATIONSHIPS``
     - ``joins``
     - Different name -- YAML uses ``joins``
   * - ``FACTS``
     - ``facts``
     -
   * - ``DIMENSIONS``
     - ``dimensions``
     -
   * - ``METRICS``
     - ``metrics``
     -
   * - ``MATERIALIZATIONS``
     - ``materializations``
     -
   * - ``COMMENT``
     - ``comment``
     - View-level comment
   * - ``[WITH] TAG``
     - ``tags``
     - A list of ``{name, value}`` entries in Snowflake's shape. See `Tag`_


.. _ref-yaml-format-example:

Complete Example
================

A YAML definition covering all supported features:

.. code-block:: yaml

   tables:
     - alias: o
       table: orders
       pk_columns:
         - id
       comment: Order transactions
       synonyms:
         - order_facts
     - alias: c
       table: customers
       pk_columns:
         - id

   joins:
     - table: c
       from_alias: o
       fk_columns:
         - customer_id
       name: order_customer
       cardinality: ManyToOne

   facts:
     - name: net_price
       expr: o.extended_price * (1 - o.discount)
       source_table: o
       comment: Price after discount
       synonyms:
         - discounted_price

   dimensions:
     - name: region
       expr: o.region
       source_table: o
       comment: Sales territory
       tags:
         - name: {tag: S_LAYER_KIND}
           value: dimension
     - name: customer_name
       expr: c.name
       source_table: c
     - name: report_date
       expr: o.report_date
       source_table: o

   metrics:
     - name: revenue
       expr: SUM(o.amount)
       source_table: o
       comment: Total revenue
       synonyms:
         - total_revenue
     - name: order_count
       expr: COUNT(*)
       source_table: o
     - name: avg_order
       expr: revenue / order_count
     - name: balance
       expr: SUM(o.amount)
       source_table: o
       non_additive_by:
         - dimension: report_date
           order: Desc
           nulls: First

   materializations:
     - name: region_agg
       table: revenue_by_region
       dimensions:
         - region
       metrics:
         - revenue
         - order_count

   comment: Revenue analytics view
   tags:
     - name: {database: analytics, schema: governance, tag: owner}
       value: finance


.. _ref-yaml-format-minimal:

Minimal Definition
==================

A minimal definition has ``tables``, ``dimensions``, and ``metrics``. Both the ``dimensions`` and ``metrics`` keys must be present, and at least one of them must be non-empty. Write ``[]`` for the one you do not need:

.. code-block:: yaml

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

A definition with metrics only:

.. code-block:: yaml

   tables:
     - alias: o
       table: orders
       pk_columns:
         - id
   dimensions: []
   metrics:
     - name: revenue
       expr: SUM(o.amount)
       source_table: o

.. note::

   ``tables`` must list at least one table. A definition that omits ``tables``, or sets it to ``[]``, is rejected by ``CREATE SEMANTIC VIEW ... FROM YAML``.

   .. versionchanged:: 0.16.0

      Earlier versions accepted such a definition, and the view failed at its first query.


.. _ref-yaml-format-toplevel:

Top-Level Keys
==============

.. list-table::
   :header-rows: 1
   :widths: 20 15 12 53

   * - Key
     - Type
     - Required
     - Description
   * - ``tables``
     - list of `Table`_
     - Yes
     - Physical tables available to the view. Must list at least one table: a definition without ``tables``, or with ``tables: []``, is rejected. See :ref:`ref-yaml-format-minimal`.
   * - ``dimensions``
     - list of `Dimension`_
     - Yes :sup:`*`
     - Named grouping expressions.
   * - ``metrics``
     - list of `Metric`_
     - Yes :sup:`*`
     - Named aggregation or derived expressions.
   * - ``joins``
     - list of `Join`_
     - No
     - FK/PK relationships between tables. Maps to the SQL ``RELATIONSHIPS`` clause.
   * - ``facts``
     - list of `Fact`_
     - No
     - Named row-level expressions (no aggregates).
   * - ``materializations``
     - list of `Materialization`_
     - No
     - Pre-aggregated table mappings for query routing.
   * - ``comment``
     - string
     - No
     - View-level human-readable description.
   * - ``tags``
     - list of `Tag`_
     - No
     - View-level tags.
   * - ``max_staleness``
     - integer or string
     - No
     - ``MAX_STALENESS``: whole seconds (``3600``) or an interval (``'1 hour'``), at least 120 seconds. See :ref:`Cortex Analyst metadata <ref-create-cortex-metadata>`.
   * - ``module_custom_instructions``
     - mapping
     - No
     - Cortex Analyst instructions: ``sql_generation`` and ``question_categorization``, both optional strings. The YAML form of ``AI_SQL_GENERATION`` and ``AI_QUESTION_CATEGORIZATION``.
   * - ``verified_queries``
     - list of `VerifiedQuery`_
     - No
     - The YAML form of ``AI_VERIFIED_QUERIES``.

:sup:`*` Both keys must be present -- a definition without a ``metrics`` key is rejected with a "missing field" error -- and at least one of the two must be non-empty. Use ``[]`` for the empty one.


.. _ref-yaml-format-table:

Table
=====

Each entry in the ``tables`` list declares a physical table with an alias. The first table is the **base table** (root of the relationship graph).

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``alias``
     - string
     - Yes
     -
     - Short name used to reference this table in all other sections.
   * - ``table``
     - string
     - Yes
     -
     - Physical table name. Supports catalog-qualified names (``catalog.schema.table``).
   * - ``pk_columns``
     - list of string
     - No
     - ``[]``
     - Primary key column names. Used for JOIN synthesis and cardinality inference.
   * - ``unique_constraints``
     - list of list of string
     - No
     - ``[]``
     - UNIQUE constraint column lists. Each inner list is one constraint. Used for cardinality inference.
   * - ``comment``
     - string
     - No
     - null
     - Human-readable description.
   * - ``synonyms``
     - list of string
     - No
     - ``[]``
     - Alternative names for discoverability.
   * - ``tags``
     - list of `Tag`_
     - No
     - ``[]``
     - Name/value metadata tags.

.. code-block:: yaml

   tables:
     - alias: o
       table: orders
       pk_columns:
         - id
       unique_constraints:
         - - email
       comment: Order transactions
       synonyms:
         - order_facts


.. _ref-yaml-format-dimension:

Dimension
=========

Each entry in the ``dimensions`` list declares a named grouping expression.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``name``
     - string
     - Yes
     -
     - Dimension name.
   * - ``expr``
     - string
     - Yes
     -
     - SQL expression. Can be a column reference (``o.region``) or computed (``date_trunc('month', o.ordered_at)``).
   * - ``source_table``
     - string
     - No
     - null
     - Table alias this dimension comes from. Used for join dependency resolution. Must be an alias declared in ``tables`` (any case), also in a single-table view. Write it unquoted: a quoted alias such as ``'"O"'`` is accepted only in a single-table view (a known limitation).
   * - ``output_type``
     - string
     - No
     - null
     - **No longer accepted.** Write the cast into the expression instead --
       ``expr: CAST(o.ordered_at AS DATE)``. No DDL clause can express a member
       type, so :ref:`GET_DDL <ref-get-ddl>` dropped this field and a restored
       view silently lost the cast; a cast written into ``expr`` survives the
       round trip. A type name here is rejected at ``CREATE`` time.
       ``output_type: null`` is accepted and ignored:
       :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>` writes it on every
       dimension, metric, and fact, so exported YAML re-imports unchanged.
   * - ``comment``
     - string
     - No
     - null
     - Human-readable description.
   * - ``synonyms``
     - list of string
     - No
     - ``[]``
     - Alternative names for discoverability.
   * - ``tags``
     - list of `Tag`_
     - No
     - ``[]``
     - Name/value metadata tags.
   * - ``is_filter``
     - boolean
     - No
     - ``false``
     - ``true`` declares the dimension a :ref:`named filter <howto-annotations-filters>`, the YAML form of ``LABELS = (FILTER)``. The expression should be boolean.
   * - ``sample_values``
     - list of string
     - No
     - ``[]``
     - The YAML form of ``SAMPLE_VALUES``. See :ref:`Cortex Analyst metadata <ref-create-cortex-metadata>`.
   * - ``is_enum``
     - boolean
     - No
     - ``false``
     - The YAML form of ``IS_ENUM``.
   * - ``cortex_search_service``
     - mapping
     - No
     - null
     - The YAML form of ``WITH CORTEX SEARCH SERVICE``: ``service`` (required), with optional ``schema``, ``database`` and ``literal_column`` (the ``USING`` column). A ``database`` needs a ``schema``, and no part may be empty.

.. code-block:: yaml

   dimensions:
     - name: region
       expr: o.region
       source_table: o
       comment: Sales territory
     - name: order_month
       expr: CAST(date_trunc('month', o.ordered_at) AS DATE)
       source_table: o
     - name: is_eu
       expr: o.region = 'EU'
       source_table: o
       is_filter: true
     - name: warehouse
       expr: o.warehouse
       source_table: o
       sample_values: [SMALL, LARGE]
       is_enum: true
       cortex_search_service:
         service: warehouses
         schema: search
         literal_column: warehouse_name


.. _ref-yaml-format-metric:

Metric
======

Each entry in the ``metrics`` list declares a named aggregation, derived metric, semi-additive metric, or window metric.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``name``
     - string
     - Yes
     -
     - Metric name.
   * - ``expr``
     - string
     - Yes
     -
     - For base metrics: aggregate expression (``SUM(o.amount)``). For derived metrics: arithmetic over other metric names (``revenue - cost``).
   * - ``source_table``
     - string
     - No
     - null
     - Table alias. Present for base metrics, absent for derived metrics. Must be an alias declared in ``tables`` (any case), also in a single-table view. Write it unquoted: a quoted alias such as ``'"O"'`` is accepted only in a single-table view (a known limitation).
   * - ``output_type``
     - string
     - No
     - null
     - **No longer accepted.** Write the cast into the expression instead
       (see the dimension table above).
   * - ``using_relationships``
     - list of string
     - No
     - ``[]``
     - Named relationships this metric traverses (for role-playing dimensions).
   * - ``comment``
     - string
     - No
     - null
     - Human-readable description.
   * - ``synonyms``
     - list of string
     - No
     - ``[]``
     - Alternative names for discoverability.
   * - ``tags``
     - list of `Tag`_
     - No
     - ``[]``
     - Name/value metadata tags.
   * - ``access``
     - string
     - No
     - ``Public``
     - Access modifier: ``Public`` (queryable) or ``Private`` (usable only in derived expressions).
   * - ``non_additive_by``
     - list of `NonAdditiveDim`_
     - No
     - ``[]``
     - Semi-additive dimensions for snapshot selection. Mutually exclusive with ``window_spec``.
   * - ``window_spec``
     - `WindowSpec`_
     - No
     - null
     - Window function specification. Mutually exclusive with ``non_additive_by``.

**Base metric** (with ``source_table`` and aggregate expression):

.. code-block:: yaml

   metrics:
     - name: revenue
       expr: SUM(o.amount)
       source_table: o

**Derived metric** (no ``source_table``, references other metrics):

.. code-block:: yaml

   metrics:
     - name: profit
       expr: revenue - cost

**Private metric:**

.. code-block:: yaml

   metrics:
     - name: raw_total
       expr: SUM(o.amount)
       source_table: o
       access: Private

**Semi-additive metric:**

.. code-block:: yaml

   metrics:
     - name: total_balance
       expr: SUM(a.balance)
       source_table: a
       non_additive_by:
         - dimension: report_date
           order: Desc
           nulls: First

**Window metric:**

.. code-block:: yaml

   metrics:
     - name: rolling_avg
       expr: AVG(total_qty)
       source_table: s
       window_spec:
         window_function: AVG
         inner_metric: total_qty
         excluding_dims:
           - date
         order_by:
           - expr: date
             order: Asc
             nulls: Last
         frame_clause: "RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW"


.. _ref-yaml-format-fact:

Fact
====

Each entry in the ``facts`` list declares a named row-level expression. Facts can reference other facts (resolved in topological order). Aggregate functions are not allowed.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``name``
     - string
     - Yes
     -
     - Fact name.
   * - ``expr``
     - string
     - Yes
     -
     - Row-level SQL expression. Must not contain aggregate functions.
   * - ``source_table``
     - string
     - No
     - null
     - Table alias this fact is scoped to.
   * - ``output_type``
     - string
     - No
     - null
     - **No longer accepted.** Write the cast into the expression instead
       (see the dimension table above). :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>`
       reports the expression's actual type without it.
   * - ``comment``
     - string
     - No
     - null
     - Human-readable description.
   * - ``synonyms``
     - list of string
     - No
     - ``[]``
     - Alternative names for discoverability.
   * - ``tags``
     - list of `Tag`_
     - No
     - ``[]``
     - Name/value metadata tags.
   * - ``access``
     - string
     - No
     - ``Public``
     - Access modifier: ``Public`` (queryable via ``facts := [...]``) or ``Private`` (usable only in metric expressions).
   * - ``is_filter``
     - boolean
     - No
     - ``false``
     - ``true`` declares the fact a :ref:`named filter <howto-annotations-filters>`, the YAML form of ``LABELS = (FILTER)``. The expression should be boolean.
   * - ``sample_values``
     - list of string
     - No
     - ``[]``
     - The YAML form of ``SAMPLE_VALUES``. See :ref:`Cortex Analyst metadata <ref-create-cortex-metadata>`.

.. code-block:: yaml

   facts:
     - name: net_price
       expr: o.extended_price * (1 - o.discount)
       source_table: o
       comment: Price after discount
       synonyms:
         - discounted_price
     - name: tax_amount
       expr: o.net_price * o.tax_rate
       source_table: o


.. _ref-yaml-format-join:

Join
====

Each entry in the ``joins`` list declares a FK/PK relationship between tables. This maps to the SQL ``RELATIONSHIPS`` clause.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``table``
     - string
     - Yes
     -
     - Target table alias (the table being joined to).
   * - ``from_alias``
     - string
     - Yes
     -
     - Source table alias containing the FK columns.
   * - ``fk_columns``
     - list of string
     - Yes
     -
     - FK column names on the source table. At least one is required; ``CREATE`` rejects a relationship without any.
   * - ``ref_columns``
     - list of string
     - No
     - ``[]``
     - Referenced columns on the target table. Defaults to the target's primary key if omitted.
   * - ``name``
     - string
     - Yes
     -
     - Relationship name, referenced by ``using_relationships`` for role-playing dimensions. Must be unique among the view's relationships, compared the way DuckDB compares identifiers (``r``, ``R`` and ``"r"`` are one name).
   * - ``cardinality``
     - string
     - No
     - ``ManyToOne``
     - ``ManyToOne`` or ``OneToOne``. Used for fan trap detection.

.. code-block:: yaml

   joins:
     - table: c
       from_alias: o
       fk_columns:
         - customer_id
       name: order_customer
       cardinality: ManyToOne
     - table: a
       from_alias: f
       fk_columns:
         - departure_code
       ref_columns:
         - airport_code
       name: dep_airport


.. _ref-yaml-format-materialization:

Materialization
===============

Each entry in the ``materializations`` list maps a pre-aggregated table to the dimensions and metrics it covers.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``name``
     - string
     - Yes
     -
     - Materialization name (unique within the view).
   * - ``table``
     - string
     - Yes
     -
     - Pre-aggregated table name. Supports catalog-qualified names. Not validated for existence at define time.
   * - ``dimensions``
     - list of string
     - No
     - ``[]``
     - Dimension names covered. Must match declared dimension names.
   * - ``metrics``
     - list of string
     - No
     - ``[]``
     - Metric names covered. Must match declared metric names.

At least one of ``dimensions`` or ``metrics`` must be specified.

.. code-block:: yaml

   materializations:
     - name: region_agg
       table: revenue_by_region
       dimensions:
         - region
       metrics:
         - revenue
         - order_count
     - name: global_agg
       table: global_totals
       metrics:
         - revenue


.. _ref-yaml-format-nonadditivedim:

NonAdditiveDim
==============

Used within a metric's ``non_additive_by`` list to specify snapshot selection behavior.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``dimension``
     - string
     - Yes
     -
     - Dimension name for snapshot selection.
   * - ``order``
     - string
     - No
     - ``Asc``
     - Sort direction: ``Asc`` or ``Desc``.
   * - ``nulls``
     - string
     - No
     - ``Last``
     - NULLS placement: ``Last`` or ``First``.

.. code-block:: yaml

   non_additive_by:
     - dimension: report_date
       order: Desc
       nulls: First


.. _ref-yaml-format-windowspec:

WindowSpec
==========

Used within a metric's ``window_spec`` field to declare a window function metric.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``window_function``
     - string
     - Yes
     -
     - Window function name (``AVG``, ``LAG``, ``SUM``, ``RANK``, etc.).
   * - ``inner_metric``
     - string
     - Yes
     -
     - Name of the metric to wrap in the window function.
   * - ``extra_args``
     - list of string
     - No
     - ``[]``
     - Additional function arguments (e.g., ``["30"]`` for ``LAG(metric, 30)``).
   * - ``excluding_dims``
     - list of string
     - No
     - ``[]``
     - Dimensions to exclude from the partition set (``PARTITION BY EXCLUDING`` semantics). Mutually exclusive with ``partition_dims``.
   * - ``partition_dims``
     - list of string
     - No
     - ``[]``
     - Explicit partition dimensions (``PARTITION BY`` semantics). Mutually exclusive with ``excluding_dims``.
   * - ``order_by``
     - list of `WindowOrderBy`_
     - No
     - ``[]``
     - ORDER BY entries for the window frame.
   * - ``frame_clause``
     - string
     - No
     - null
     - Raw SQL frame clause (e.g., ``RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW``).

.. code-block:: yaml

   window_spec:
     window_function: AVG
     inner_metric: total_qty
     excluding_dims:
       - date
     order_by:
       - expr: date
         order: Asc
         nulls: Last
     frame_clause: "RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW"


.. _ref-yaml-format-windoworderby:

WindowOrderBy
=============

Used within a window spec's ``order_by`` list.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``expr``
     - string
     - Yes
     -
     - Dimension name or expression to order by.
   * - ``order``
     - string
     - No
     - ``Asc``
     - Sort direction: ``Asc`` or ``Desc``.
   * - ``nulls``
     - string
     - No
     - ``Last``
     - NULLS placement: ``Last`` or ``First``.


.. _ref-yaml-format-tag:

Tag
===

.. versionadded:: 0.17.0

Used within the ``tags`` list of the view, a table, a fact, a dimension or a metric. The shape follows Snowflake's semantic-view YAML: ``name`` is a mapping with the tag's name parts.

.. list-table::
   :header-rows: 1
   :widths: 22 18 12 10 38

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``name``
     - mapping
     - Yes
     -
     - The tag name: ``tag`` (required), with optional ``schema`` and ``database``. A ``database`` needs a ``schema``. Parts are stored as written and compared case-insensitively.
   * - ``value``
     - string
     - Yes
     -
     - The tag value, at most 256 characters.

.. code-block:: yaml

   tags:
     - name: {tag: S_LAYER_KIND}
       value: metric
     - name: {schema: governance, tag: pii}
       value: none

A tag list may not name the same tag twice. Tags the SQL form could not write (an empty name part, a ``database`` without a ``schema``, a value over 256 characters, a duplicate) are rejected by ``CREATE SEMANTIC VIEW ... FROM YAML``.


.. _ref-yaml-format-verifiedquery:

VerifiedQuery
=============

.. versionadded:: 0.18.0

Used within the top-level ``verified_queries`` list. The field names are Snowflake's semantic-view YAML names. See :ref:`Cortex Analyst metadata <ref-create-cortex-metadata>`.

.. list-table::
   :header-rows: 1
   :widths: 30 12 12 10 36

   * - Field
     - Type
     - Required
     - Default
     - Description
   * - ``name``
     - string
     - Yes
     -
     - Entry name. One identifier, unique in the list (compared case-insensitively).
   * - ``question``
     - string
     - Yes
     -
     - The question the query answers.
   * - ``sql``
     - string
     - Yes
     -
     - The SQL that answers it. Stored, not checked.
   * - ``verified_at``
     - integer
     - No
     - null
     - When the query was verified, in seconds since the Unix epoch.
   * - ``verified_by``
     - string
     - No
     - null
     - Who verified it.
   * - ``use_as_onboarding_question``
     - boolean
     - No
     - ``false``
     - The YAML form of ``ONBOARDING_QUESTION TRUE``.

.. code-block:: yaml

   max_staleness: 3600
   module_custom_instructions:
     sql_generation: Round all numeric columns to 2 decimal places.
   verified_queries:
     - name: revenue_by_region
       question: What is the revenue for each region?
       sql: SELECT region, revenue FROM SEMANTIC_VIEW(sales DIMENSIONS region METRICS revenue)
       use_as_onboarding_question: true


.. _ref-yaml-format-size-limit:

Size Limit
==========

YAML definitions are capped at 1 MiB (1,048,576 bytes). Definitions exceeding this limit are rejected before parsing. Large definitions should be split into multiple semantic views.


.. _ref-yaml-format-related:

Related
=======

- :ref:`ref-create-semantic-view` -- ``FROM YAML`` and ``FROM YAML FILE`` syntax
- :ref:`ref-read-yaml` -- Export a semantic view as YAML
- :ref:`howto-yaml-definitions` -- Step-by-step import and export workflow
