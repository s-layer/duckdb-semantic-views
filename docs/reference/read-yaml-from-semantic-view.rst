.. meta::
   :description: Syntax reference for READ_YAML_FROM_SEMANTIC_VIEW(), which exports a stored semantic view definition as a YAML string

.. _ref-read-yaml:

================================
READ_YAML_FROM_SEMANTIC_VIEW
================================

Scalar function that returns the YAML representation of a stored semantic view definition. The output is suitable for round-trip import via :ref:`CREATE SEMANTIC VIEW ... FROM YAML <ref-create-from-yaml>`.

.. versionadded:: 0.7.0


.. _ref-read-yaml-syntax:

Syntax
======

.. code-block:: sqlgrammar

   SELECT READ_YAML_FROM_SEMANTIC_VIEW('<view_name>')


.. _ref-read-yaml-params:

Parameters
==========

.. list-table::
   :header-rows: 1
   :widths: 20 15 65

   * - Parameter
     - Type
     - Description
   * - ``<view_name>``
     - VARCHAR
     - The name of the semantic view to export. Supports unqualified (``my_view``), schema-qualified (``main.my_view``), and catalog-qualified (``memory.main.my_view``) names. A schema qualifier pins the schema. An unqualified name resolves through the session's ``search_path``: the first schema on the path holding a view of that name wins. This is the same rule :ref:`GET_DDL <ref-get-ddl-resolution>` uses.


.. _ref-read-yaml-output:

Output
======

Returns a single VARCHAR value containing the YAML representation of the semantic view definition. The YAML includes all user-declared clauses: tables, relationships, facts, dimensions, metrics, and materializations with their full configuration (comments, synonyms, access modifiers, named-filter labels, NON ADDITIVE BY, window specs).

The output always carries a ``joins`` and a ``facts`` key (``[]`` when the view has none), and an ``output_type: null`` line on every dimension, metric, and fact. ``output_type`` is a withdrawn field: a type name there is rejected on import, but ``null`` is accepted and ignored, so the exported YAML re-imports unchanged. See :ref:`ref-yaml-format`.


.. _ref-read-yaml-stripping:

Field Stripping
===============

Fields that record where and when the view was created are left out of the YAML, because they describe the source environment rather than the definition. ``CREATE SEMANTIC VIEW ... FROM YAML`` sets them again from the session that runs the import:

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - Stripped Field
     - Reason
   * - ``created_on``
     - Creation timestamp. Set to the import time on re-creation.
   * - ``database_name``
     - Database the view lives in. Set from the importing session.
   * - ``schema_name``
     - Schema the view lives in. Set from the view name in the ``CREATE`` statement, or from the importing session's current schema when the name is unqualified.

Unqualified table names in ``table:`` entries resolve in the importing session's current schema, the same as in a ``CREATE SEMANTIC VIEW ... AS`` body.


.. _ref-read-yaml-examples:

Examples
========

**Export a semantic view to YAML:**

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

   SELECT READ_YAML_FROM_SEMANTIC_VIEW('order_metrics');

Output:

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
     output_type: null
   metrics:
   - name: revenue
     expr: SUM(o.amount)
     source_table: o
     output_type: null
   joins: []
   facts: []

**Save YAML to a file:**

.. code-block:: sql

   COPY (SELECT READ_YAML_FROM_SEMANTIC_VIEW('order_metrics'))
   TO '/path/to/order_metrics.yaml' (FORMAT CSV, HEADER FALSE, QUOTE '');

**Schema-qualified view name:**

.. code-block:: sql

   SELECT READ_YAML_FROM_SEMANTIC_VIEW('main.order_metrics');

**Round-trip (export then import):**

.. code-block:: sql

   -- Export
   COPY (SELECT READ_YAML_FROM_SEMANTIC_VIEW('analytics'))
   TO '/tmp/analytics.yaml' (FORMAT CSV, HEADER FALSE, QUOTE '');

   -- Import into a new view
   CREATE SEMANTIC VIEW analytics_copy FROM YAML FILE '/tmp/analytics.yaml';


.. _ref-read-yaml-errors:

Error Cases
===========

**View does not exist:**

.. code-block:: sql

   SELECT READ_YAML_FROM_SEMANTIC_VIEW('nonexistent');

.. code-block:: text

   Error: read_yaml_from_semantic_view: semantic view 'nonexistent' does not exist

**Name held by several schemas, none of them on the path:**

.. code-block:: sql

   -- With both analytics.order_metrics and staging.order_metrics present,
   -- and the default search path (main)
   SELECT READ_YAML_FROM_SEMANTIC_VIEW('order_metrics');

.. code-block:: text

   Error: read_yaml_from_semantic_view: semantic view 'order_metrics' does not exist on the search path. It exists in schemas analytics, staging, none of which are on the current search path (main). Qualify the reference as <schema>.order_metrics, or add the schema to search_path.
