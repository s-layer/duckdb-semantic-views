.. meta::
   :description: Syntax reference for DESCRIBE SEMANTIC VIEW, which returns the full definition of a view in a property-per-row format showing each object and its properties

.. _ref-describe-semantic-view:

========================
DESCRIBE SEMANTIC VIEW
========================

Returns the definition of a semantic view as a multi-row result set in property-per-row format. Each row represents one property of one object (semantic view, table, relationship, variable, fact, dimension, metric, derived metric, or materialization) in the view definition.


.. _ref-describe-syntax:

Syntax
======

.. code-block:: sqlgrammar

   { DESCRIBE | DESC } SEMANTIC VIEW <name>;

``DESC`` is accepted as an abbreviation of ``DESCRIBE``, matching Snowflake.


.. _ref-describe-params:

Parameters
==========

``<name>``
   The name of the semantic view to describe. Returns an error if the view does not exist.

.. note::

   ``DESCRIBE SEMANTIC VIEW`` runs in your transaction, so a ``CREATE`` / ``ALTER`` / ``DROP`` issued earlier in it is reflected here before you commit. See :ref:`explanation-txn-ddl-write-visibility`.


.. _ref-describe-output:

Output Columns
==============

The result contains multiple rows with 5 VARCHAR columns:

.. list-table::
   :header-rows: 1
   :widths: 18 10 72

   * - Column
     - Type
     - Description
   * - ``object_kind``
     - VARCHAR
     - The type of object: ``TABLE``, ``RELATIONSHIP``, ``VARIABLE``, ``FACT``, ``DIMENSION``, ``METRIC``, ``DERIVED_METRIC``, ``MATERIALIZATION``, ``CUSTOM_INSTRUCTIONS``, or ``AI_VERIFIED_QUERY``. Empty string on the view-level rows.
   * - ``object_name``
     - VARCHAR
     - The name of the object (table name, relationship name, variable name, fact/dimension/metric name, materialization name, verified query name). Empty string on the view-level rows and the ``CUSTOM_INSTRUCTIONS`` rows.
   * - ``parent_entity``
     - VARCHAR
     - The parent table for this object. Empty string for the view-level rows and for ``TABLE``, ``VARIABLE``, ``DERIVED_METRIC``, ``MATERIALIZATION``, ``CUSTOM_INSTRUCTIONS``, and ``AI_VERIFIED_QUERY`` objects.
   * - ``property``
     - VARCHAR
     - The property name being described.
   * - ``property_value``
     - VARCHAR
     - The property value.


.. _ref-describe-object-kinds:

Object Kinds and Properties
===========================

Rows appear in definition order: the view-level comment, ``MAX_STALENESS`` and tags (when set), then ``TABLE`` objects, then ``RELATIONSHIP``, ``VARIABLE``, ``FACT``, ``DIMENSION``, ``METRIC``, ``DERIVED_METRIC``, ``MATERIALIZATION``, ``CUSTOM_INSTRUCTIONS``, and ``AI_VERIFIED_QUERY``.

.. versionchanged:: 0.14.0

   The ``DATA_TYPE`` property rows carry each member's type again. They had been
   empty for every view created since v0.10.0. The type is now worked out when
   the command runs, so views created in that window report it too, with no
   need to re-create them.

**View-level comment, MAX_STALENESS and tags**
   Emitted only when a view-level comment (in ``CREATE`` or via :ref:`ALTER SEMANTIC VIEW SET COMMENT <ref-alter-semantic-view>`), ``MAX_STALENESS`` or view-level tags are set. The rows describe the view itself, so ``object_kind``, ``object_name``, and ``parent_entity`` are all empty strings. Produces up to three property rows:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``COMMENT``
        - The view-level comment text.
      * - ``MAX_STALENESS``
        - The view's :ref:`MAX_STALENESS <ref-create-cortex-metadata>` as written: a number of seconds (``3600``) or an interval (``1 hour``).

          .. versionadded:: 0.18.0
      * - ``TAGS``
        - The view's tags as a JSON object. See ``TAGS`` below.

**TABLE**
   One block per table declared in the ``TABLES`` clause, named by its physical table, or by its alias for a table defined by a query. Each table produces 1-7 property rows:

   .. list-table::
      :header-rows: 1
      :widths: 35 65

      * - Property
        - Description
      * - ``BASE_TABLE_DATABASE_NAME``
        - The DuckDB database containing the physical table (e.g., ``memory``).
      * - ``BASE_TABLE_SCHEMA_NAME``
        - The DuckDB schema containing the physical table (e.g., ``main``).
      * - ``BASE_TABLE_NAME``
        - The physical table name.
      * - ``DEFINITION``
        - For a table :ref:`defined by a query <ref-create-inline-query>`, the query, without its parentheses. Such a table has no ``BASE_TABLE_*`` rows, as in Snowflake.

          .. versionadded:: 0.18.0
      * - ``PRIMARY_KEY``
        - JSON array of primary key column names (e.g., ``["id"]``). Only emitted when a primary key is declared.
      * - ``COMMENT``
        - The table comment text. Only emitted when a comment is set.
      * - ``SYNONYMS``
        - JSON array of synonym strings (e.g., ``["transactions","purchases"]``). Only emitted when synonyms are set.
      * - ``TAGS``
        - JSON object of the object's tags, in declaration order (e.g., ``{"S_LAYER_KIND":"dimension","governance.pii":"none"}``). Each key is the tag name as ``GET_DDL`` writes it. Only emitted when tags are set.

          .. versionadded:: 0.17.0

**RELATIONSHIP**
   One block per relationship declared in the ``RELATIONSHIPS`` clause:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``TABLE``
        - The physical table name on the foreign key side.
      * - ``REF_TABLE``
        - The physical table name on the referenced (primary key) side.
      * - ``FOREIGN_KEY``
        - JSON array of foreign key column names (e.g., ``["customer_id"]``).
      * - ``REF_KEY``
        - JSON array of referenced key column names (e.g., ``["id"]``).

**VARIABLE**
   .. versionadded:: 0.18.0

   One block per variable declared in the ``VARIABLES`` clause (see :ref:`ref-create-variables`), in declaration order, named by ``object_name``. Variables have an empty ``parent_entity``. Each property is emitted only when the variable has it; a variable with no type, default or comment produces one ``DATA_TYPE`` row with an empty value:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``DATA_TYPE``
        - The variable's type as declared (e.g., ``INTEGER``).
      * - ``DEFAULT_VALUE``
        - The default expression as written (e.g., ``1000`` or ``current_date``). A default declared with ``:=`` is shown the same way.
      * - ``COMMENT``
        - The variable's comment text.

   A member that reads a variable reports its ``DATA_TYPE`` as if the variable were its default, or a ``NULL`` of the variable's type when it has no default.

**FACT**
   One block per fact declared in the ``FACTS`` clause:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``TABLE``
        - The physical table name the fact is scoped to.
      * - ``EXPRESSION``
        - The row-level SQL expression defining the fact.
      * - ``DATA_TYPE``
        - The type the member's column has in a :ref:`semantic-view query <ref-semantic-view-function>`, in DuckDB's spelling (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``). Determined when the command runs, so it follows the current source columns. Empty when the member's source no longer binds (for example, its table was dropped), or when the member cannot be queried on its own because of a fan trap. See :ref:`Reported Data Types <explanation-sf-data-types>`.
      * - ``COMMENT``
        - The fact comment text. Only emitted when a comment is set.
      * - ``SYNONYMS``
        - JSON array of synonym strings. Only emitted when synonyms are set.
      * - ``TAGS``
        - JSON object of the object's tags, in declaration order (e.g., ``{"S_LAYER_KIND":"dimension","governance.pii":"none"}``). Each key is the tag name as ``GET_DDL`` writes it. Only emitted when tags are set.

          .. versionadded:: 0.17.0
      * - ``SAMPLE_VALUES``
        - JSON array of the fact's :ref:`sample values <ref-create-cortex-metadata>` (e.g., ``["100","250"]``). Only emitted when set.

          .. versionadded:: 0.18.0
      * - ``LABELS``
        - ``["FILTER"]`` when the fact is declared a :ref:`named filter <howto-annotations-filters>` with ``LABELS = (FILTER)``. Only emitted when labeled.
      * - ``ACCESS_MODIFIER``
        - ``PUBLIC`` or ``PRIVATE``. Always emitted.

**DIMENSION**
   One block per dimension declared in the ``DIMENSIONS`` clause:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``TABLE``
        - The physical table name the dimension is scoped to.
      * - ``EXPRESSION``
        - The SQL expression defining the dimension.
      * - ``DATA_TYPE``
        - The type the member's column has in a :ref:`semantic-view query <ref-semantic-view-function>`, in DuckDB's spelling (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``). Determined when the command runs, so it follows the current source columns. Empty when the member's source no longer binds (for example, its table was dropped), or when the member cannot be queried on its own because of a fan trap. See :ref:`Reported Data Types <explanation-sf-data-types>`.
      * - ``COMMENT``
        - The dimension comment text. Only emitted when a comment is set.
      * - ``SYNONYMS``
        - JSON array of synonym strings. Only emitted when synonyms are set.
      * - ``TAGS``
        - JSON object of the object's tags, in declaration order (e.g., ``{"S_LAYER_KIND":"dimension","governance.pii":"none"}``). Each key is the tag name as ``GET_DDL`` writes it. Only emitted when tags are set.

          .. versionadded:: 0.17.0
      * - ``SAMPLE_VALUES``
        - JSON array of the dimension's :ref:`sample values <ref-create-cortex-metadata>` (e.g., ``["East","West"]``). Only emitted when set.

          .. versionadded:: 0.18.0
      * - ``IS_ENUM``
        - ``true`` when the dimension is declared ``IS_ENUM``. Only emitted when set.

          .. versionadded:: 0.18.0
      * - ``CORTEX_SEARCH_SERVICE_COLUMN_NAME``, ``CORTEX_SEARCH_SERVICE_DATABASE_NAME``, ``CORTEX_SEARCH_SERVICE_SCHEMA_NAME``, ``CORTEX_SEARCH_SERVICE_NAME``
        - The parts of the dimension's ``WITH CORTEX SEARCH SERVICE`` clause, as Snowflake names them: the ``USING`` column, the database, the schema and the service. Only the parts written are emitted; ``CORTEX_SEARCH_SERVICE_NAME`` always is when the clause is present.

          .. versionadded:: 0.18.0
      * - ``LABELS``
        - ``["FILTER"]`` when the dimension is declared a :ref:`named filter <howto-annotations-filters>` with ``LABELS = (FILTER)``. Only emitted when labeled.

**METRIC**
   One block per base metric (those scoped to a table) declared in the ``METRICS`` clause:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``TABLE``
        - The physical table name the metric is scoped to.
      * - ``EXPRESSION``
        - The aggregate SQL expression defining the metric.
      * - ``DATA_TYPE``
        - The type the member's column has in a :ref:`semantic-view query <ref-semantic-view-function>`, in DuckDB's spelling (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``). Determined when the command runs, so it follows the current source columns. Empty when the member's source no longer binds (for example, its table was dropped), or when the member cannot be queried on its own because of a fan trap. See :ref:`Reported Data Types <explanation-sf-data-types>`.
      * - ``COMMENT``
        - The metric comment text. Only emitted when a comment is set.
      * - ``SYNONYMS``
        - JSON array of synonym strings. Only emitted when synonyms are set.
      * - ``TAGS``
        - JSON object of the object's tags, in declaration order (e.g., ``{"S_LAYER_KIND":"dimension","governance.pii":"none"}``). Each key is the tag name as ``GET_DDL`` writes it. Only emitted when tags are set.

          .. versionadded:: 0.17.0
      * - ``ACCESS_MODIFIER``
        - ``PUBLIC`` or ``PRIVATE``. Always emitted.
      * - ``NON_ADDITIVE_BY``
        - Comma-separated list of non-additive dimensions with optional sort/nulls (e.g., ``report_date DESC NULLS FIRST``). Only emitted for semi-additive metrics.
      * - ``WINDOW_SPEC``
        - Reconstructed OVER clause string (e.g., ``AVG(total_qty) OVER (PARTITION BY EXCLUDING date ORDER BY date)``). Only emitted for window metrics.

**DERIVED_METRIC**
   One block per derived metric (those referencing other metrics rather than a table). Derived metrics have an empty ``parent_entity``:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``EXPRESSION``
        - The expression composing other metrics.
      * - ``DATA_TYPE``
        - The type the member's column has in a :ref:`semantic-view query <ref-semantic-view-function>`, in DuckDB's spelling (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``). Determined when the command runs, so it follows the current source columns. Empty when the member's source no longer binds (for example, its table was dropped), or when the member cannot be queried on its own because of a fan trap. See :ref:`Reported Data Types <explanation-sf-data-types>`.
      * - ``COMMENT``
        - The derived metric comment text. Only emitted when a comment is set.
      * - ``SYNONYMS``
        - JSON array of synonym strings. Only emitted when synonyms are set.
      * - ``TAGS``
        - JSON object of the object's tags, in declaration order (e.g., ``{"S_LAYER_KIND":"dimension","governance.pii":"none"}``). Each key is the tag name as ``GET_DDL`` writes it. Only emitted when tags are set.

          .. versionadded:: 0.17.0
      * - ``ACCESS_MODIFIER``
        - ``PUBLIC`` or ``PRIVATE``. Always emitted.

.. note::

   Derived metrics never carry ``NON_ADDITIVE_BY`` or ``WINDOW_SPEC``
   properties: the parser rejects ``NON ADDITIVE BY`` and ``OVER`` on a derived
   metric, so a semi-additive or window derived metric cannot be defined. Those
   properties appear only under ``METRIC`` (base metrics scoped to a table).

**MATERIALIZATION**
   .. versionadded:: 0.7.0

   One block per materialization declared in the ``MATERIALIZATIONS`` clause. Each materialization produces 3 property rows. Materializations have an empty ``parent_entity``:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``TABLE``
        - The physical table name that the materialization points to.
      * - ``DIMENSIONS``
        - JSON array of dimension names covered by this materialization (e.g., ``["region"]``). Empty array ``[]`` when no dimensions are declared.
      * - ``METRICS``
        - JSON array of metric names covered by this materialization (e.g., ``["revenue","order_count"]``). Empty array ``[]`` when no metrics are declared.

**CUSTOM_INSTRUCTIONS**
   .. versionadded:: 0.18.0

   One row per Cortex Analyst instruction the view carries (see :ref:`Cortex Analyst metadata <ref-create-cortex-metadata>`), with empty ``object_name`` and ``parent_entity``, as in Snowflake:

   .. list-table::
      :header-rows: 1
      :widths: 30 70

      * - Property
        - Description
      * - ``AI_SQL_GENERATION``
        - The ``AI_SQL_GENERATION`` text.
      * - ``AI_QUESTION_CATEGORIZATION``
        - The ``AI_QUESTION_CATEGORIZATION`` text.

**AI_VERIFIED_QUERY**
   .. versionadded:: 0.18.0

   One block per entry of ``AI_VERIFIED_QUERIES``, named by ``object_name``:

   .. list-table::
      :header-rows: 1
      :widths: 25 75

      * - Property
        - Description
      * - ``QUESTION``
        - The question.
      * - ``VERIFIED_AT``
        - Seconds since the Unix epoch. Only emitted when set.
      * - ``ONBOARDING_QUESTION``
        - ``true`` or ``false``. Always emitted.
      * - ``VERIFIED_BY``
        - Who verified the query, as written. Only emitted when set.
      * - ``SQL``
        - The SQL text.


.. _ref-describe-examples:

Examples
========

**Simple single-table view:**

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region AS o.region
   )
   METRICS (
       o.total AS SUM(o.amount)
   );

   DESCRIBE SEMANTIC VIEW order_metrics;

.. code-block:: text

   ┌─────────────┬─────────────┬───────────────┬──────────────────────────┬────────────────┐
   │ object_kind │ object_name │ parent_entity │ property                 │ property_value │
   ├─────────────┼─────────────┼───────────────┼──────────────────────────┼────────────────┤
   │ TABLE       │ orders      │               │ BASE_TABLE_DATABASE_NAME │ memory         │
   │ TABLE       │ orders      │               │ BASE_TABLE_SCHEMA_NAME   │ main           │
   │ TABLE       │ orders      │               │ BASE_TABLE_NAME          │ orders         │
   │ TABLE       │ orders      │               │ PRIMARY_KEY              │ ["id"]         │
   │ DIMENSION   │ region      │ orders        │ TABLE                    │ orders         │
   │ DIMENSION   │ region      │ orders        │ EXPRESSION               │ o.region       │
   │ DIMENSION   │ region      │ orders        │ DATA_TYPE                │ VARCHAR        │
   │ METRIC      │ total       │ orders        │ TABLE                    │ orders         │
   │ METRIC      │ total       │ orders        │ EXPRESSION               │ SUM(o.amount)  │
   │ METRIC      │ total       │ orders        │ DATA_TYPE                │ DECIMAL(38,2)  │
   │ METRIC      │ total       │ orders        │ ACCESS_MODIFIER          │ PUBLIC         │
   └─────────────┴─────────────┴───────────────┴──────────────────────────┴────────────────┘

**View with metadata annotations:**

.. code-block:: sql

   CREATE SEMANTIC VIEW annotated AS
   TABLES (
       o AS orders PRIMARY KEY (id) COMMENT = 'Order data'
   )
   DIMENSIONS (
       o.region AS o.region COMMENT = 'Sales region' WITH SYNONYMS = ('territory')
   )
   METRICS (
       o.revenue AS SUM(o.amount) COMMENT = 'Total revenue'
   );

   ALTER SEMANTIC VIEW annotated SET COMMENT = 'Revenue analytics';

   DESCRIBE SEMANTIC VIEW annotated;

.. code-block:: text

   ┌─────────────┬─────────────┬───────────────┬──────────────────────────┬───────────────────┐
   │ object_kind │ object_name │ parent_entity │ property                 │ property_value    │
   ├─────────────┼─────────────┼───────────────┼──────────────────────────┼───────────────────┤
   │             │             │               │ COMMENT                  │ Revenue analytics │
   │ TABLE       │ orders      │               │ BASE_TABLE_DATABASE_NAME │ memory            │
   │ TABLE       │ orders      │               │ BASE_TABLE_SCHEMA_NAME   │ main              │
   │ TABLE       │ orders      │               │ BASE_TABLE_NAME          │ orders            │
   │ TABLE       │ orders      │               │ PRIMARY_KEY              │ ["id"]            │
   │ TABLE       │ orders      │               │ COMMENT                  │ Order data        │
   │ DIMENSION   │ region      │ orders        │ TABLE                    │ orders            │
   │ DIMENSION   │ region      │ orders        │ EXPRESSION               │ o.region          │
   │ DIMENSION   │ region      │ orders        │ DATA_TYPE                │ VARCHAR           │
   │ DIMENSION   │ region      │ orders        │ COMMENT                  │ Sales region      │
   │ DIMENSION   │ region      │ orders        │ SYNONYMS                 │ ["territory"]     │
   │ METRIC      │ revenue     │ orders        │ TABLE                    │ orders            │
   │ METRIC      │ revenue     │ orders        │ EXPRESSION               │ SUM(o.amount)     │
   │ METRIC      │ revenue     │ orders        │ DATA_TYPE                │ DECIMAL(38,2)     │
   │ METRIC      │ revenue     │ orders        │ COMMENT                  │ Total revenue     │
   │ METRIC      │ revenue     │ orders        │ ACCESS_MODIFIER          │ PUBLIC            │
   └─────────────┴─────────────┴───────────────┴──────────────────────────┴───────────────────┘

The first row is the view-level comment, so its ``object_kind`` and ``object_name`` are empty.

**View with materializations:**

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region AS o.region
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
       )
   );

   DESCRIBE SEMANTIC VIEW order_metrics;

.. code-block:: text

   ┌─────────────────┬─────────────┬───────────────┬──────────────────────────┬───────────────────────────┐
   │ object_kind     │ object_name │ parent_entity │ property                 │ property_value            │
   ├─────────────────┼─────────────┼───────────────┼──────────────────────────┼───────────────────────────┤
   │ TABLE           │ orders      │               │ BASE_TABLE_DATABASE_NAME │ memory                    │
   │ TABLE           │ orders      │               │ BASE_TABLE_SCHEMA_NAME   │ main                      │
   │ TABLE           │ orders      │               │ BASE_TABLE_NAME          │ orders                    │
   │ TABLE           │ orders      │               │ PRIMARY_KEY              │ ["id"]                    │
   │ DIMENSION       │ region      │ orders        │ TABLE                    │ orders                    │
   │ DIMENSION       │ region      │ orders        │ EXPRESSION               │ o.region                  │
   │ DIMENSION       │ region      │ orders        │ DATA_TYPE                │ VARCHAR                   │
   │ METRIC          │ revenue     │ orders        │ TABLE                    │ orders                    │
   │ METRIC          │ revenue     │ orders        │ EXPRESSION               │ SUM(o.amount)             │
   │ METRIC          │ revenue     │ orders        │ DATA_TYPE                │ DECIMAL(38,2)             │
   │ METRIC          │ revenue     │ orders        │ ACCESS_MODIFIER          │ PUBLIC                    │
   │ METRIC          │ order_count │ orders        │ TABLE                    │ orders                    │
   │ METRIC          │ order_count │ orders        │ EXPRESSION               │ COUNT(*)                  │
   │ METRIC          │ order_count │ orders        │ DATA_TYPE                │ BIGINT                    │
   │ METRIC          │ order_count │ orders        │ ACCESS_MODIFIER          │ PUBLIC                    │
   │ MATERIALIZATION │ region_agg  │               │ TABLE                    │ revenue_by_region         │
   │ MATERIALIZATION │ region_agg  │               │ DIMENSIONS               │ ["region"]                │
   │ MATERIALIZATION │ region_agg  │               │ METRICS                  │ ["revenue","order_count"] │
   └─────────────────┴─────────────┴───────────────┴──────────────────────────┴───────────────────────────┘

The materialization rows come last, after the tables, dimensions, and metrics.

.. _ref-describe-subquery:

**Filtering the output:**

.. versionadded:: 0.16.0

``DESCRIBE SEMANTIC VIEW`` can be used as a subquery or in a ``WITH`` clause, like DuckDB's own ``DESCRIBE`` (see :ref:`ref-functions-subqueries`). Filter by ``object_kind`` to pick out one part of the definition. With the view above, only the materialization rows:

.. code-block:: sql

   SELECT object_name, property, property_value
   FROM (DESCRIBE SEMANTIC VIEW order_metrics)
   WHERE object_kind = 'MATERIALIZATION';

.. code-block:: text

   ┌─────────────┬────────────┬───────────────────────────┐
   │ object_name │ property   │ property_value            │
   ├─────────────┼────────────┼───────────────────────────┤
   │ region_agg  │ TABLE      │ revenue_by_region         │
   │ region_agg  │ DIMENSIONS │ ["region"]                │
   │ region_agg  │ METRICS    │ ["revenue","order_count"] │
   └─────────────┴────────────┴───────────────────────────┘

Only the dimensions:

.. code-block:: sql

   SELECT object_name, property, property_value
   FROM (DESCRIBE SEMANTIC VIEW order_metrics)
   WHERE object_kind = 'DIMENSION';

.. code-block:: text

   ┌─────────────┬────────────┬────────────────┐
   │ object_name │ property   │ property_value │
   ├─────────────┼────────────┼────────────────┤
   │ region      │ TABLE      │ orders         │
   │ region      │ EXPRESSION │ o.region       │
   │ region      │ DATA_TYPE  │ VARCHAR        │
   └─────────────┴────────────┴────────────────┘

How many objects of each kind the view declares:

.. code-block:: sql

   SELECT object_kind, count(DISTINCT object_name) AS objects
   FROM (DESCRIBE SEMANTIC VIEW order_metrics)
   GROUP BY object_kind
   ORDER BY object_kind;

.. code-block:: text

   ┌─────────────────┬─────────┐
   │ object_kind     │ objects │
   ├─────────────────┼─────────┤
   │ DIMENSION       │ 1       │
   │ MATERIALIZATION │ 1       │
   │ METRIC          │ 2       │
   │ TABLE           │ 1       │
   └─────────────────┴─────────┘

The relationships of a view with more than one table:

.. code-block:: sql

   CREATE SEMANTIC VIEW shop AS
   TABLES (
       o AS orders PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (
       c.name AS c.name
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

   SELECT object_name, property, property_value
   FROM (DESCRIBE SEMANTIC VIEW shop)
   WHERE object_kind = 'RELATIONSHIP';

.. code-block:: text

   ┌────────────────┬─────────────┬─────────────────┐
   │ object_name    │ property    │ property_value  │
   ├────────────────┼─────────────┼─────────────────┤
   │ order_customer │ TABLE       │ orders          │
   │ order_customer │ REF_TABLE   │ customers       │
   │ order_customer │ FOREIGN_KEY │ ["customer_id"] │
   │ order_customer │ REF_KEY     │ ["id"]          │
   └────────────────┴─────────────┴─────────────────┘

The variables of the ``sales_analysis`` view from :ref:`ref-create-variables`:

.. code-block:: sql

   SELECT object_name, property, property_value
   FROM (DESCRIBE SEMANTIC VIEW sales_analysis)
   WHERE object_kind = 'VARIABLE';

.. code-block:: text

   ┌───────────────┬───────────────┬─────────────────────────┐
   │ object_name   │ property      │ property_value          │
   ├───────────────┼───────────────┼─────────────────────────┤
   │ min_amount    │ DATA_TYPE     │ INTEGER                 │
   │ min_amount    │ DEFAULT_VALUE │ 1000                    │
   │ multiplier    │ DATA_TYPE     │ DOUBLE                  │
   │ multiplier    │ DEFAULT_VALUE │ 1.0                     │
   │ target_region │ DATA_TYPE     │ VARCHAR                 │
   │ target_region │ COMMENT       │ the region to report on │
   └───────────────┴───────────────┴─────────────────────────┘

**Error: view does not exist:**

.. code-block:: sql

   DESCRIBE SEMANTIC VIEW nonexistent_view;

.. code-block:: text

   Error: semantic view 'nonexistent_view' does not exist

**The statement is case-insensitive:**

.. code-block:: sql

   describe semantic view order_metrics;
