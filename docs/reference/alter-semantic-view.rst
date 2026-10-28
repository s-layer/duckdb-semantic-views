.. meta::
   :description: Syntax reference for ALTER SEMANTIC VIEW, covering RENAME TO, SET and UNSET of COMMENT and MAX_STALENESS, SET TAG, and UNSET TAG operations

.. _ref-alter-semantic-view:

======================
ALTER SEMANTIC VIEW
======================

Modifies an existing semantic view. Supports renaming, setting or removing the view-level comment and ``MAX_STALENESS``, and setting or removing view-level tags. The view definition (tables, relationships, dimensions, metrics, facts) is preserved.


.. _ref-alter-syntax:

Syntax
======

.. code-block:: sqlgrammar

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> RENAME TO <new_name>

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> SET COMMENT = '<text>'

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> SET MAX_STALENESS = { <seconds> | '<interval>' }

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> UNSET { COMMENT | MAX_STALENESS } [ , ... ]

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> SET TAG <tag_name> = '<tag_value>' [ , ... ]

   ALTER SEMANTIC VIEW [ IF EXISTS ] <name> UNSET TAG <tag_name> [ , ... ]


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

.. _ref-alter-max-staleness:

``ALTER SEMANTIC VIEW <name> SET MAX_STALENESS = { <seconds> | '<interval>' }``
   Sets the view's :ref:`MAX_STALENESS <ref-create-cortex-metadata>`, replacing any existing value. The value follows the ``CREATE`` rules: whole seconds or a single-quoted interval such as ``'2 hours'``, at least 120 seconds either way. Returns an error if the view does not exist.

``ALTER SEMANTIC VIEW <name> UNSET COMMENT, MAX_STALENESS``
   ``UNSET`` takes a list of view properties, as in Snowflake, each named once. Removing a property the view does not have is not an error.

With ``IF EXISTS``, ``SET MAX_STALENESS`` and ``UNSET`` succeed silently when the view does not exist.

.. versionadded:: 0.18.0

   ``SET MAX_STALENESS``, ``UNSET MAX_STALENESS``, and several properties in one ``UNSET``.

.. _ref-alter-tags:

``ALTER SEMANTIC VIEW <name> SET TAG <tag_name> = '<tag_value>' [ , ... ]``
   Sets one or more view-level :ref:`tags <ref-create-tags>`. A tag the view does not have is added. A tag it already has gets the new value and moves to the end of the list, spelled as in this statement. Returns an error if the view does not exist.

``ALTER SEMANTIC VIEW <name> UNSET TAG <tag_name> [ , ... ]``
   Removes the named view-level tags. A name the view does not carry is ignored. Removing the last tag leaves the view as if it had been created without ``WITH TAG``. Returns an error if the view does not exist.

With ``IF EXISTS``, ``SET TAG`` and ``UNSET TAG`` succeed silently when the view does not exist.

.. versionadded:: 0.18.0

   ``SET TAG`` and ``UNSET TAG``.

.. note::

   As in Snowflake, ``ALTER`` changes the tags of the view itself, never those of its tables or members, and the tag list takes no parentheses. To change a member's tags, re-create the view. Tag names match case-insensitively, so ``SET TAG OWNER = 'x'`` replaces a tag written ``owner``. ``SET TAG`` follows the rules of the ``WITH TAG`` clause: a qualified name (``governance.pii``) is allowed, values are single-quoted strings, and naming the same tag twice in one list is an error. The change shows in the ``TAGS`` row of :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` and in :ref:`GET_DDL <ref-get-ddl>`.

.. note::

   **Schema qualifiers.** ``<name>`` may carry a ``<schema>.`` qualifier and it decides which view is altered; an unqualified name resolves through the session's ``search_path``, and the first schema on the path holding a view of that name wins (see :ref:`ref-create-semantic-view`). When several schemas hold a view of the same name, qualify ``<name>`` so the statement alters the view you mean. A qualifier on ``<new_name>`` **moves** the view: ``ALTER SEMANTIC VIEW analytics.sales RENAME TO staging.sales`` relocates it to ``staging``. An unqualified ``<new_name>`` leaves the view in its current schema, so the ordinary rename never moves anything. The "already exists" check applies to the destination schema.

.. note::

   ``ALTER`` participates in your surrounding transaction (``BEGIN ... ROLLBACK`` restores the previous name, comment, MAX_STALENESS and tags). The non-``IF EXISTS`` forms raise ``semantic view '<name>' does not exist`` when the view is absent at check time (and ``RENAME`` raises ``semantic view '<new_name>' already exists`` if the target name is taken); ``IF EXISTS`` keeps its silent no-op behavior. The existence/collision check and the update are atomic only inside an explicit transaction -- under autocommit a concurrent commit in the window between them is not detected (a concurrent drop leaves the update affecting 0 rows; a concurrently taken rename target surfaces a raw key-constraint error). See :ref:`explanation-transactional-ddl` for the guard window and how to close it.

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

``<seconds>`` / ``'<interval>'``
   The staleness limit (SET MAX_STALENESS only): a whole number of seconds, or a single-quoted count and unit (``second``, ``minute``, ``hour`` or ``day``, singular or plural, any case). At least 120 seconds.

``<tag_name>``
   A tag name (SET TAG and UNSET TAG), optionally qualified (``governance.pii``). Matched case-insensitively against the view's tags.

``<tag_value>``
   The tag value (SET TAG only). A single-quoted string of up to 256 characters.


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

**SET MAX_STALENESS** returns the same two columns with ``status`` set to ``max_staleness set``. ``UNSET`` names what it removed: ``comment unset``, ``max_staleness unset``, or ``comment, max_staleness unset``.

**SET TAG and UNSET TAG** return the same two columns, with ``status`` set to ``tags set`` or ``tags unset``.


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

**Set and remove MAX_STALENESS:**

.. code-block:: sql

   ALTER SEMANTIC VIEW sales SET MAX_STALENESS = '2 hours';

.. code-block:: text

   ┌───────┬───────────────────┐
   │ name  │ status            │
   ├───────┼───────────────────┤
   │ sales │ max_staleness set │
   └───────┴───────────────────┘

``DESCRIBE`` now shows a ``MAX_STALENESS`` row reading ``2 hours``. Remove it and the comment in one statement:

.. code-block:: sql

   ALTER SEMANTIC VIEW sales UNSET COMMENT, MAX_STALENESS;

.. code-block:: text

   ┌───────┬──────────────────────────────┐
   │ name  │ status                       │
   ├───────┼──────────────────────────────┤
   │ sales │ comment, max_staleness unset │
   └───────┴──────────────────────────────┘

**Set and remove view-level tags:**

Given a view created with ``WITH TAG (owner = 'analytics')``:

.. code-block:: sql

   ALTER SEMANTIC VIEW sales SET TAG owner = 'finance', governance.tier = 'gold';

.. code-block:: text

   ┌───────┬──────────┐
   │ name  │ status   │
   ├───────┼──────────┤
   │ sales │ tags set │
   └───────┴──────────┘

The view's ``TAGS`` row in ``DESCRIBE`` now reads ``{"owner":"finance","governance.tier":"gold"}``. Remove one tag:

.. code-block:: sql

   ALTER SEMANTIC VIEW sales UNSET TAG governance.tier;

.. code-block:: text

   ┌───────┬────────────┐
   │ name  │ status     │
   ├───────┼────────────┤
   │ sales │ tags unset │
   └───────┴────────────┘

``GET_DDL`` now ends with ``WITH TAG (owner = 'finance')``.

**Error: target name already exists:**

.. code-block:: sql

   -- Assuming both 'sales' and 'inventory' exist
   ALTER SEMANTIC VIEW sales RENAME TO inventory;

.. code-block:: text

   Error: semantic view 'inventory' already exists

**The statement is case-insensitive:**

.. code-block:: sql

   alter semantic view sales_view rename to revenue_view;
