.. meta::
   :description: How transactional DDL works in DuckDB Semantic Views, and the small set of caveats around read visibility, concurrent CREATE, and DDL across multiple connections

.. _explanation-transactional-ddl:

==========================================
Transactional DDL and Known Limitations
==========================================

.. versionadded:: 0.8.0

``CREATE``, ``DROP``, and ``ALTER SEMANTIC VIEW`` are fully transactional: they participate in your surrounding ``BEGIN`` / ``COMMIT`` / ``ROLLBACK`` block the way ordinary DuckDB DDL does. ADBC, dbt-duckdb, and any other transaction-aware client see the same semantics.

This page covers the everyday behavior first, then a short list of edge cases. Most of them surface only in unusual situations -- several connections issuing DDL against the same database at the same time, or scripts that change the parser setting the extension relies on.

For the common workload -- open a database, run DDL at start-up, then query -- the next two sections cover everything that applies.


.. _explanation-txn-ddl-what-changed:

DDL Now Participates in Your Transaction
=========================================

You can wrap DDL in ``BEGIN`` / ``COMMIT`` and rely on the rollback semantics:

.. code-block:: sql

   BEGIN;
   CREATE SEMANTIC VIEW order_metrics AS
       TABLES (o AS orders) DIMENSIONS (o.region AS o.region);
   ROLLBACK;
   -- order_metrics does not exist; the CREATE was discarded.

   BEGIN;
   DROP SEMANTIC VIEW order_metrics;
   ROLLBACK;
   -- order_metrics is still there; the DROP was discarded.

   BEGIN;
   ALTER SEMANTIC VIEW order_metrics RENAME TO sales_metrics;
   ROLLBACK;
   -- the view is still called order_metrics.

This applies to every ``CREATE`` body variant: the ``AS`` keyword body, inline ``FROM YAML $$ ... $$``, ``FROM YAML FILE '<path>'``, and the ``CREATE OR REPLACE`` / ``IF NOT EXISTS`` modifiers. ``ALTER`` covers ``RENAME TO``, ``SET COMMENT``, and ``UNSET COMMENT``.


.. _explanation-txn-ddl-write-visibility:

Reads Inside an Open Transaction See Committed State
=====================================================

The introspection commands -- :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`, :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>` (and the other ``SHOW SEMANTIC *`` variants), :ref:`READ_YAML_FROM_SEMANTIC_VIEW <ref-read-yaml>`, :ref:`GET_DDL <ref-get-ddl>` -- always read what has been **committed**. They do not see the uncommitted changes from your own open transaction.

So this sequence:

.. code-block:: sql

   BEGIN;
   CREATE SEMANTIC VIEW v AS TABLES (o AS orders) DIMENSIONS (o.r AS o.region);
   SHOW SEMANTIC VIEWS;   -- v is NOT in the result yet
   COMMIT;
   SHOW SEMANTIC VIEWS;   -- now v is listed

is expected. The same applies to in-flight ``DROP`` (the row keeps appearing until commit) and ``ALTER ... RENAME TO`` (the row appears under its old name until commit). If you need ``SHOW`` or ``DESCRIBE`` to reflect a change, commit first.

The same rule covers the data itself: when you query a semantic view with :ref:`semantic_view() <ref-semantic-view-function>`, that query also reads committed state from your underlying tables. If you have inserted rows into ``orders`` inside an open transaction and then query a semantic view over ``orders`` in the same transaction, those new rows will not be included. Commit the data writes first, or do the data write and the semantic-view query in separate transactions.

In short: commit before introspecting.


.. _explanation-txn-ddl-create-race:

CREATE IF NOT EXISTS Across Multiple Connections
=================================================

.. note::

   This section is mostly theoretical for typical DuckDB usage. DuckDB runs as an in-process library, and only one process at a time can open a database file for writing, so most deployments run their DDL from a single connection at start-up. In that setting ``CREATE SEMANTIC VIEW IF NOT EXISTS`` behaves as expected every time, and this section can be skipped.

If two connections to the same database (for example, two worker threads in one program, each with its own connection) both run ``CREATE SEMANTIC VIEW IF NOT EXISTS my_view ...`` at the same time, and neither has committed yet when the other checks, both will try to create the view. One will win. The other will see one of these errors, depending on whether the collision is caught as the row is written or when the transaction commits:

.. code-block:: text

   Constraint Error: Duplicate key "schema_name: main, name: my_view" violates primary key constraint.

.. code-block:: text

   TransactionContext Error: Failed to commit: PRIMARY KEY or UNIQUE constraint violation: duplicate key "main, my_view"

This is the same error a plain ``CREATE SEMANTIC VIEW`` would produce in the same race. ``IF NOT EXISTS`` reliably absorbs duplicates on a single connection; it cannot absorb two connections that both found no existing view at check time.

If you do run parallel set-up on several connections -- worker threads that each bootstrap at start-up, or parallel set-up in a test harness -- catch either error on your view name and treat it as success:

.. code-block:: python

   try:
       conn.execute("CREATE SEMANTIC VIEW IF NOT EXISTS my_view ...")
   except (duckdb.ConstraintException, duckdb.TransactionException) as e:
       if "my_view" not in str(e):
           raise
       # Another connection created the view first; treat that as success.

The first writer wins, the second writer sees a clear error rather than silent corruption, and after the catch both connections see the same view.


.. _explanation-txn-ddl-drop-alter-race:

DROP and ALTER Without IF EXISTS: the Existence Check and Its Race Window
=========================================================================

A non-``IF EXISTS`` ``DROP SEMANTIC VIEW my_view`` (or any non-``IF EXISTS`` ``ALTER SEMANTIC VIEW my_view ...`` form) is rewritten into a small existence-check statement followed by the actual ``DELETE`` / ``UPDATE``. If the view is not there when the check runs, you get:

.. code-block:: text

   Invalid Input Error: semantic view 'my_view' does not exist

rather than a silent success: the statement named a specific view, and that view was not there. The ``IF EXISTS`` variants (``DROP SEMANTIC VIEW IF EXISTS my_view``, ``ALTER SEMANTIC VIEW IF EXISTS my_view ...``) keep their silent-no-op contract by design.

When a single connection issues all the DDL there is no race. The rest of this section matters only when several connections issue DDL against the same database at the same time.

The check and the write are separate statements, and **whether they are atomic depends on your transaction**:

- **Under autocommit (the default), they are not atomic.** DuckDB commits after each statement, so the existence check and the ``DELETE`` / ``UPDATE`` run in two separate transactions. A concurrent drop is caught only if it has already committed by the time your check runs. If another connection drops the view in the small window *between* your check and your write, the check passes and then:

  - a plain ``DROP`` deletes 0 rows and reports success having removed nothing (a silent no-op);
  - a plain ``ALTER RENAME`` whose target name was taken in that window surfaces a raw ``Constraint Error: Duplicate key`` from DuckDB rather than the extension's ``already exists`` message.

- **Inside an explicit transaction, they are atomic.** Wrap the DDL in ``BEGIN ... COMMIT`` (or use a connection with ``autocommit = false``) and the check and the write share one snapshot. A conflicting concurrent commit then makes your ``COMMIT`` fail with a transaction-conflict error you can retry, instead of slipping through the window.

So if you run parallel DDL and need the check to be reliable, wrap it in a transaction:

.. code-block:: sql

   BEGIN;
   DROP SEMANTIC VIEW my_view;
   COMMIT;

This is the ``DROP`` / ``ALTER`` counterpart of the ``CREATE IF NOT EXISTS`` race above; both come down to the same rule -- concurrent DDL against one database needs an explicit transaction. Narrowing that window without one is a known limitation of the two-statement check-then-write sequence.


.. _explanation-txn-ddl-readonly:

Read-Only Databases
====================

.. versionadded:: 0.9.0

Loading the extension into a read-only DuckDB database works the same way as a writable one -- ``LOAD semantic_views`` succeeds and you can query any semantic view that was previously defined. The extension detects ``access_mode = 'read_only'`` at load time and skips the catalog-table bootstrap that would otherwise fail with DuckDB's read-only error.

Three behaviors change between writable and read-only databases:

1. **Reads work as usual on a bootstrapped database.** If the database already contains a ``semantic_layer._definitions`` table (because it was opened writable before and one or more semantic views were defined), then ``SHOW SEMANTIC VIEWS``, ``DESCRIBE SEMANTIC VIEW name``, ``FROM semantic_view('name', dimensions := [...], metrics := [...])``, and the rest of the SHOW / DESCRIBE / GET_DDL family all behave identically to writable mode.

2. **A fresh read-only database is treated as having zero views, not as an error.** If the database was never bootstrapped (no ``semantic_layer._definitions`` table exists), ``SHOW SEMANTIC VIEWS`` returns zero rows. Each lookup returns its usual "no such view" error rather than a raw catalog error about a missing table: ``DESCRIBE SEMANTIC VIEW anything`` reports ``semantic view 'anything' does not exist``, and ``FROM semantic_view('anything', ...)`` reports ``Semantic view 'anything' not found. Run SHOW SEMANTIC VIEWS to see all registered views.``

3. **DDL fails with DuckDB's standard read-only error.** ``CREATE``, ``DROP``, and ``ALTER SEMANTIC VIEW`` write to ``semantic_layer._definitions`` on the caller's connection, as an ``INSERT``, ``DELETE`` or ``UPDATE``. On a read-only database those writes fail with:

   .. code-block:: text

      Invalid Input Error: Cannot execute statement of type "INSERT" on database "<name>" which is attached in read-only mode!

   The exact statement-type token (``INSERT`` / ``DELETE`` / ``UPDATE``) varies by DDL form. The extension does not wrap or rephrase the message.

   On a read-only database that was never bootstrapped, ``CREATE SEMANTIC VIEW`` fails one step earlier, because the table it writes to does not exist:

   .. code-block:: text

      Catalog Error: Table with name "semantic_layer._definitions" does not exist because schema "semantic_layer" does not exist.

   Either way, define the views on a writable connection first, as shown below.

Bootstrap-then-Reopen Workflow
------------------------------

The typical pattern for shipping a read-only database with pre-defined semantic views is:

.. code-block:: python

   import duckdb

   # Step 1 -- open writable, define views, close.
   rw = duckdb.connect("analytics.duckdb")
   rw.execute("LOAD semantic_views")
   rw.execute("""
       CREATE SEMANTIC VIEW orders AS
         TABLES (o AS orders_table PRIMARY KEY (id))
         DIMENSIONS (o.region AS o.region)
         METRICS (o.total AS SUM(o.amount))
   """)
   rw.close()

   # Step 2 -- reopen read-only and query.
   ro = duckdb.connect("analytics.duckdb", read_only=True)
   ro.execute("LOAD semantic_views")
   rows = ro.execute(
       "SELECT * FROM semantic_view('orders', dimensions := ['region'], metrics := ['total'])"
   ).fetchall()


.. _explanation-txn-ddl-attach:

Attached Databases (Single-Catalog)
====================================

Semantic-view definitions live in one place: a ``semantic_layer._definitions`` table created in the database you loaded the extension into (the primary database). The extension is **single-catalog** -- it manages semantic views in that one database, not across ``ATTACH``-ed databases.

Concretely, if you ``ATTACH`` another database and ``USE`` it, then run semantic-view DDL:

.. code-block:: sql

   ATTACH 'other.db' AS other;
   USE other;
   CREATE SEMANTIC VIEW v AS ...;   -- error, see below

you get an actionable error rather than a confusing failure or a silently-lost view:

.. code-block:: text

   semantic_views: semantic-view DDL was issued against database 'other', but the
   semantic view catalog lives in a different database. Semantic views are
   single-catalog: manage them from the database the extension was loaded into,
   without USE-ing into an attached database.

Reads (``SHOW SEMANTIC VIEWS``, ``semantic_view(...)``, ``DESCRIBE``) always resolve against the primary catalog regardless of ``USE``, so the rule is: **run semantic-view DDL from the database you loaded the extension into.** You can still reference tables from attached databases inside a view body by qualifying them (``TABLES (o AS other.main.orders ...)``) -- only the ``_definitions`` catalog is single-database.

None of this arises if you do not ``ATTACH`` a second database, or if the session stays on the primary catalog.

.. note::

   Because the ``_definitions`` table is tied to the primary database, the one-time
   v0.1.0 companion-file migration only ever imports the **primary** database's own
   ``<db>.semantic_views`` companion file. An attached database's companion file is
   never read or removed.


.. _explanation-txn-ddl-one-statement:

One Statement per Call
======================

.. versionchanged:: 0.14.0

   The error below is new. This case used to fail with a ``parser_override is
   not active`` message that pointed at a setting that was not the problem.

Send each semantic-view statement to DuckDB on its own. A query string that holds several statements -- for example one Python ``execute()`` call with a ``CREATE TABLE``, an ``INSERT`` and a ``CREATE SEMANTIC VIEW`` separated by semicolons -- is not supported when any of them is semantic-view DDL. It fails with this error, whatever the position of the semantic-view statement in the string:

.. code-block:: text

   Parser Error: semantic_views: semantic-view DDL must be sent as its own statement, one statement per call. This query string also contains other statements; send each statement in a separate call (a single trailing ';' is fine).

Split the string into one call per statement (a single trailing semicolon is fine):

.. code-block:: python

   con.execute("CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE)")
   con.execute("INSERT INTO orders VALUES (1, 'US', 100), (2, 'EU', 200)")
   con.execute("""
       CREATE SEMANTIC VIEW sales AS
       TABLES (o AS orders PRIMARY KEY (id))
       DIMENSIONS (o.region AS o.region)
       METRICS (o.revenue AS SUM(o.amount));
   """)

Strings of ordinary SQL statements are unaffected.


.. _explanation-txn-ddl-peg:

The Parser Override Setting
===========================

.. versionchanged:: 0.15.0

   DuckDB 2.0 parses every statement with its PEG parser and no longer has
   ``enable_peg_parser()`` or ``disable_peg_parser()``. On DuckDB 1.5, calling
   ``disable_peg_parser()`` reset the setting described below as a side effect.
   That can no longer happen.

``CREATE SEMANTIC VIEW`` and the other semantic-view statements are not part of DuckDB's own grammar. The extension handles them through DuckDB's parser-override hook, which DuckDB consults only while the ``allow_parser_override_extension`` setting is ``'FALLBACK'``. The extension sets it to ``'FALLBACK'`` when it loads and leaves it in place.

If a session sets ``allow_parser_override_extension`` back to ``'DEFAULT'``, semantic-view DDL on that connection stops working and fails with:

.. code-block:: text

   Parser Error: semantic_views: parser_override is not active for this connection (allow_parser_override_extension is 'DEFAULT' or 'STRICT'). Re-enable with: SET allow_parser_override_extension='FALLBACK';

Restore the setting with one statement:

.. code-block:: sql

   SET allow_parser_override_extension = 'FALLBACK';

Sessions that never change the setting are unaffected. (Semantic-view DDL inside a multi-statement query string fails with a different message; see :ref:`explanation-txn-ddl-one-statement`.)


.. _explanation-txn-ddl-summary:

Summary
========

For most users, the everyday-visible behavior is:

- ``BEGIN ... ROLLBACK`` rolls back ``CREATE``, ``DROP``, and ``ALTER SEMANTIC VIEW``.

The other items on this page only matter in specific situations:

- Introspection inside an open transaction shows committed state -- commit before ``SHOW`` / ``DESCRIBE`` if you need to see your own pending changes.
- Concurrent ``CREATE IF NOT EXISTS`` from two connections can produce a constraint or commit-conflict error on one of them; catch it and treat as success.
- Semantic views are single-catalog: run their DDL from the database you loaded the extension into, not from a ``USE``-d attached database.
- Send semantic-view DDL one statement per call, not inside a multi-statement string.
- Setting ``allow_parser_override_extension`` back to ``'DEFAULT'`` turns semantic-view DDL off until you set it to ``'FALLBACK'`` again.

See also:

- :ref:`explanation-txn-ddl-readonly` -- read-only database support and the bootstrap-then-reopen workflow.
- :ref:`explanation-txn-ddl-attach` -- single-catalog behavior with ``ATTACH`` / ``USE``.
- :ref:`ref-create-semantic-view` -- syntax for all four ``CREATE`` body forms.
- :ref:`ref-drop-semantic-view` -- ``DROP`` and ``DROP IF EXISTS``.
- :ref:`ref-alter-semantic-view` -- ``ALTER`` variants.
- :ref:`ref-error-messages` -- error catalog.
