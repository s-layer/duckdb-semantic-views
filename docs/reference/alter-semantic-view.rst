.. meta::
   :description: Syntax reference for ALTER SEMANTIC VIEW, covering RENAME TO, SET COMMENT, and UNSET COMMENT operations

.. _ref-alter-semantic-view:

======================
ALTER SEMANTIC VIEW
======================

Modifies an existing semantic view. Supports renaming and setting or removing the view-level comment. The view definition (tables, relationships, dimensions, metrics, facts) is preserved.


.. _ref-alter-syntax:

Syntax
======

.. code-block:: sqlgrammar

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> RENAME TO <new_name>

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> SET COMMENT = '<text>'

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> UNSET COMMENT


.. _ref-alter-variants:

Statement Variants
==================

``ALTER SEMANTIC VIEW <name> RENAME TO <new_name>``
   Renames the semantic view from ``<name>`` to ``<new_name>``. Returns an error if ``<name>`` does not exist or if ``<new_name>`` already exists.

``ALTER SEMANTIC VIEW IF EXISTS <name> RENAME TO <new_name>``
   Renames the semantic view if it exists. If ``<name>`` does not exist, the statement succeeds silently without modifying anything. Returns an error if ``<new_name>`` already exists.

``ALTER SEMANTIC VIEW <name> SET COMMENT = '<text>'``
   Sets the view-level comment on the semantic view. Replaces any existing comment. Returns an error if the view does not exist.

``ALTER SEMANTIC VIEW IF EXISTS <name> SET COMMENT = '<text>'``
   Sets the view-level comment if the view exists. If the view does not exist, the statement succeeds silently.

``ALTER SEMANTIC VIEW <name> UNSET COMMENT``
   Removes the view-level comment from the semantic view. Returns an error if the view does not exist.

``ALTER SEMANTIC VIEW IF EXISTS <name> UNSET COMMENT``
   Removes the view-level comment if the view exists. If the view does not exist, the statement succeeds silently.

.. note::

   **Schema qualifiers.** ``<name>`` may carry a ``<schema>.`` qualifier and it decides which view is altered; an unqualified name resolves through the session's ``search_path``, and the first schema on the path holding a view of that name wins (see :ref:`ref-create-semantic-view`). When several schemas hold a view of the same name, qualify ``<name>`` so the statement alters the view you mean. A qualifier on ``<new_name>`` **moves** the view: ``ALTER SEMANTIC VIEW analytics.sales RENAME TO staging.sales`` relocates it to ``staging``. An unqualified ``<new_name>`` leaves the view in its current schema, so the ordinary rename never moves anything. The "already exists" check applies to the destination schema.

.. note::

   ``ALTER`` participates in your surrounding transaction (``BEGIN ... ROLLBACK`` restores the previous name and comment). The non-``IF EXISTS`` forms raise ``semantic view '<name>' does not exist`` when the view is absent at check time (and ``RENAME`` raises ``semantic view '<new_name>' already exists`` if the target name is taken); ``IF EXISTS`` keeps its silent no-op behavior. The existence/collision check and the update are atomic only inside an explicit transaction -- under autocommit a concurrent commit in the window between them is not detected (a concurrent drop leaves the update affecting 0 rows; a concurrently taken rename target surfaces a raw key-constraint error). See :ref:`explanation-transactional-ddl` for the guard window and how to close it.

.. note::

   Requires a writable database. On a read-only database this statement fails with DuckDB's standard ``Cannot execute statement of type "..." on database "<name>" which is attached in read-only mode!`` error. See :ref:`explanation-txn-ddl-readonly`.


.. _ref-alter-params:

Parameters
==========

``<name>``
   The name of the semantic view to modify. Matched case-insensitively (folded to lowercase per DuckDB identifier semantics), quoted or not.

``<new_name>``
   The new name for the semantic view (RENAME TO only). Folded to lowercase before it is stored, quoted or not. Must not match the name of an existing semantic view.

``<text>``
   The comment text (SET COMMENT only). Must be enclosed in single quotes. Use ``''`` to escape single quotes within the text.


.. _ref-alter-output:

Output Columns
==============

**RENAME TO** returns a single row with 2 columns:

.. list-table::
   :header-rows: 1
   :widths: 20 15 65

   * - Column
     - Type
     - Description
   * - ``old_name``
     - VARCHAR
     - The original semantic view name before the rename.
   * - ``new_name``
     - VARCHAR
     - The new semantic view name after the rename.

**SET COMMENT and UNSET COMMENT** return a single row with 2 columns:

.. list-table::
   :header-rows: 1
   :widths: 20 15 65

   * - Column
     - Type
     - Description
   * - ``name``
     - VARCHAR
     - The semantic view name.
   * - ``status``
     - VARCHAR
     - The operation result: ``comment set`` or ``comment unset``.


.. _ref-alter-examples:

Examples
========

**Rename a semantic view:**

.. code-block:: sql

   ALTER SEMANTIC VIEW sales_view RENAME TO revenue_view;

After the rename, queries must use the new name:

.. code-block:: sql

   -- This works
   SELECT * FROM SEMANTIC_VIEW(revenue_view
       DIMENSIONS region
       METRICS total_amount
   );

   -- This fails: "Semantic view 'sales_view' not found.
   -- Run SHOW SEMANTIC VIEWS to see all registered views."
   SELECT * FROM SEMANTIC_VIEW(sales_view
       DIMENSIONS region
       METRICS total_amount
   );

**Rename with IF EXISTS (safe no-op):**

.. code-block:: sql

   -- Succeeds silently if 'old_reports' does not exist
   ALTER SEMANTIC VIEW IF EXISTS old_reports RENAME TO new_reports;

**Set a view-level comment:**

.. code-block:: sql

   ALTER SEMANTIC VIEW sales SET COMMENT = 'Revenue analytics for North America';

.. code-block:: text

   ┌───────┬─────────────┐
   │ name  │ status      │
   ├───────┼─────────────┤
   │ sales │ comment set │
   └───────┴─────────────┘

The comment appears in :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>` and :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`.

**Remove a view-level comment:**

.. code-block:: sql

   ALTER SEMANTIC VIEW sales UNSET COMMENT;

.. code-block:: text

   ┌───────┬───────────────┐
   │ name  │ status        │
   ├───────┼───────────────┤
   │ sales │ comment unset │
   └───────┴───────────────┘

**Error: target name already exists:**

.. code-block:: sql

   -- Assuming both 'sales' and 'inventory' exist
   ALTER SEMANTIC VIEW sales RENAME TO inventory;

.. code-block:: text

   Error: semantic view 'inventory' already exists

**The statement is case-insensitive:**

.. code-block:: sql

   alter semantic view sales_view rename to revenue_view;
