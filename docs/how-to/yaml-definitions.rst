.. meta::
   :description: Import semantic view definitions from inline YAML or YAML files, export existing views to YAML, and round-trip definitions for version control and migration

.. _howto-yaml-definitions:

==============================================
How to Import and Export YAML Definitions
==============================================

.. versionadded:: 0.7.0

This guide shows how to create a semantic view from a YAML definition (inline or file), export an existing view to YAML, and round-trip definitions between environments. These features enable version-controlled definitions, cross-environment migration, and sharing semantic view configurations outside of SQL.

**Prerequisites:**

- A working DuckDB installation with the ``semantic_views`` extension loaded
- For file import: a YAML definition file accessible to DuckDB
- For export: an existing semantic view to export


.. _howto-yaml-import-inline:

Import from Inline YAML
========================

Use ``FROM YAML`` with a dollar-quoted string to create a semantic view from an inline YAML definition:

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

The YAML body is enclosed in ``$$`` dollar-quote delimiters. Tagged dollar-quoting is also supported for clarity:

.. code-block:: duckdb-sql

   CREATE SEMANTIC VIEW order_metrics FROM YAML $yaml$
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
   $yaml$;

Both ``CREATE OR REPLACE`` and ``CREATE IF NOT EXISTS`` variants work with ``FROM YAML``:

.. code-block:: duckdb-sql

   CREATE OR REPLACE SEMANTIC VIEW order_metrics FROM YAML $$
   ...
   $$;

   CREATE SEMANTIC VIEW IF NOT EXISTS order_metrics FROM YAML $$
   ...
   $$;


.. _howto-yaml-import-file:

Import from a YAML File
========================

Use ``FROM YAML FILE`` with a single-quoted file path to create a semantic view from a YAML file:

.. code-block:: sql

   CREATE SEMANTIC VIEW order_metrics FROM YAML FILE '/path/to/order_metrics.yaml';

The file path must be single-quoted. DuckDB reads the file and parses its contents as a YAML semantic view definition.

.. code-block:: sql

   CREATE OR REPLACE SEMANTIC VIEW order_metrics
   FROM YAML FILE '/path/to/order_metrics.yaml';

.. note::

   ``CREATE SEMANTIC VIEW ... FROM YAML FILE`` participates in the caller's transaction along with the inline ``AS`` and ``FROM YAML $$ ... $$`` forms. ``BEGIN ... ROLLBACK`` discards the loaded view as expected. See :ref:`explanation-transactional-ddl`.


.. _howto-yaml-export:

Export with READ_YAML_FROM_SEMANTIC_VIEW
========================================

Use the :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>` scalar function to export an existing semantic view as a YAML string:

.. code-block:: sql

   SELECT READ_YAML_FROM_SEMANTIC_VIEW('order_metrics');

The function returns a single VARCHAR value containing the YAML representation of the view definition. The output includes all clauses (tables, relationships, facts, dimensions, metrics, materializations) that were declared when the view was created.

To save the output to a file, use DuckDB's ``COPY`` statement:

.. code-block:: sql

   COPY (SELECT READ_YAML_FROM_SEMANTIC_VIEW('order_metrics'))
   TO '/path/to/order_metrics.yaml' (FORMAT CSV, HEADER FALSE, QUOTE '');

The function supports schema-qualified and catalog-qualified view names:

.. code-block:: sql

   SELECT READ_YAML_FROM_SEMANTIC_VIEW('main.order_metrics');
   SELECT READ_YAML_FROM_SEMANTIC_VIEW('memory.main.order_metrics');

See :ref:`ref-read-yaml` for the full function reference.


.. _howto-yaml-roundtrip:

Round-Trip Workflow
===================

Export and import together enable a full round-trip workflow for migrating semantic views between environments:

**1. Export from the source environment:**

.. code-block:: sql

   COPY (SELECT READ_YAML_FROM_SEMANTIC_VIEW('analytics'))
   TO '/shared/analytics.yaml' (FORMAT CSV, HEADER FALSE, QUOTE '');

**2. Import into the target environment:**

.. code-block:: sql

   CREATE SEMANTIC VIEW analytics FROM YAML FILE '/shared/analytics.yaml';

**3. Verify the import:**

.. code-block:: sql

   -- Compare DDL output to confirm the definition round-tripped correctly
   SELECT GET_DDL('SEMANTIC_VIEW', 'analytics');

The exported YAML produces the same semantic view definition when imported, including all materializations, metadata annotations, and access modifiers.

.. tip::

   Store YAML definitions in version control alongside your data model. This provides a history of semantic view changes and enables code review for model updates.


.. _howto-yaml-troubleshooting:

Troubleshooting
===============

**Error: Expected 'AS', 'TABLES' or 'FROM YAML' after view name**
   The DDL body must start with ``AS`` or ``TABLES`` (keyword body) or ``FROM YAML`` (YAML body). Check that the ``FROM YAML`` keywords appear directly after the view name.

**Error: Expected '$' to begin dollar-quoted string**
   The inline YAML body must be enclosed in dollar-quote delimiters (``$$`` or ``$tag$``). Ensure the YAML content starts with ``$$`` immediately after ``FROM YAML``.

**Error: Unterminated dollar-quoted string**
   The closing delimiter was not found. Ensure the closing ``$$`` (or ``$tag$``) matches the opening delimiter exactly.

**Error: Unexpected content after closing dollar-quote**
   Extra text appears after the closing ``$$``. Remove any trailing content after the closing delimiter (semicolons are allowed at the statement level but not inside the dollar-quote).

**Error: File path cannot be empty**
   The ``FROM YAML FILE`` variant requires a non-empty single-quoted file path: ``FROM YAML FILE '/path/to/file.yaml'``.

**Error: YAML definition exceeds size limit**
   YAML definitions are capped at 1 MiB. Large definitions should be split into multiple semantic views.

**Error: semantic view 'name' does not exist (on export)**
   :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>` requires the view to exist. Check the view name with :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>`.

See :ref:`ref-error-messages` for the full list of YAML-related error messages.


.. _howto-yaml-related:

Related
=======

- :ref:`ref-yaml-format` -- YAML definition format specification
- :ref:`ref-read-yaml` -- ``READ_YAML_FROM_SEMANTIC_VIEW()`` function reference
- :ref:`ref-create-semantic-view` -- ``FROM YAML`` and ``FROM YAML FILE`` syntax
- :ref:`ref-get-ddl` -- Export as SQL DDL (alternative to YAML export)
