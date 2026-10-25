.. meta::
   :description: How transactional DDL works in DuckDB Semantic Views, what reads inside an open transaction see, and the small set of caveats around concurrent CREATE and DDL across multiple connections

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

Reads Inside an Open Transaction See Your Changes
==================================================

.. versionchanged:: 0.16.0

   Earlier releases read every semantic view's definition as last committed,
   so a view created, dropped or renamed in an open transaction did not show
   up that way until you committed.

Everything that reads a semantic view runs in your transaction: :ref:`semantic_view() <ref-semantic-view-function>`, ``explain_semantic_view()``, :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`, :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>` and the other ``SHOW SEMANTIC *`` commands, :ref:`READ_YAML_FROM_SEMANTIC_VIEW <ref-read-yaml>` and :ref:`GET_DDL <ref-get-ddl>`. They see your own uncommitted changes, as a query of an ordinary table does:

.. code-block:: sql

   BEGIN;
   CREATE SEMANTIC VIEW v AS TABLES (o AS orders) DIMENSIONS (o.r AS o.region);
   SHOW SEMANTIC VIEWS;   -- v is listed
   SELECT * FROM semantic_view('v', dimensions := ['r']);   -- and can be queried
   ROLLBACK;
   SHOW SEMANTIC VIEWS;   -- v is gone

An in-flight ``DROP`` hides the view, and an in-flight ``ALTER ... RENAME TO`` shows it under its new name, in your transaction only. Other connections see none of it until you commit.

The member types that ``SHOW`` and ``DESCRIBE`` report, and the plan ``explain_semantic_view()`` shows, come from your transaction too. A source table you alter in an open transaction, or a temporary table of your session, is typed and planned the way ``semantic_view()`` reads it.


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

Several Statements in One Call
==============================

.. versionchanged:: 0.16.0

   Semantic-view DDL can share a query string with other statements. Earlier
   releases refused such a string with ``semantic-view DDL must be sent as its
   own statement, one statement per call``.

A query string can hold semantic-view statements alongside other SQL, separated by semicolons. DuckDB splits the string and runs each statement in order, so one Python ``execute()`` call can set up a table and a view on it:

.. code-block:: python

   con.execute("""
       CREATE TABLE orders (id INTEGER, region VARCHAR, amount DOUBLE);
       INSERT INTO orders VALUES (1, 'US', 100), (2, 'EU', 200);
       CREATE SEMANTIC VIEW sales AS
       TABLES (o AS orders PRIMARY KEY (id))
       DIMENSIONS (o.region AS o.region)
       METRICS (o.revenue AS SUM(o.amount));
   """)

DuckDB 2.0 runs a string's statements one at a time and parses each one just before it runs. So when one statement fails, whether on a syntax error or while it runs, the statements before it have already run and their effects stay, and nothing after it runs. The exception is a string that opens its own transaction with ``BEGIN``: the error aborts that transaction, so the statements before it are rolled back with it, and the connection needs a ``ROLLBACK`` before it runs anything else.

A view named with a DuckDB reserved word, such as ``select``, has to be quoted in every statement, as DuckDB requires for its own names: ``CREATE SEMANTIC VIEW "select" AS ...``. Unquoted, the statement fails with a :ref:`syntax error <ref-err-syntax>`.


.. _explanation-txn-ddl-peg:

How the Extension Joins DuckDB's Parser
=======================================

.. versionchanged:: 0.16.0

   The semantic-view statements are part of DuckDB's own grammar on every
   connection. Earlier releases also parsed a statement sent on its own
   through DuckDB's parser-override hook, which the
   ``allow_parser_override_extension`` setting could switch off. That setting
   no longer affects semantic-view statements.

``CREATE SEMANTIC VIEW`` and the other semantic-view statements are not part of DuckDB's built-in grammar. When the extension loads, it adds them to the PEG grammar DuckDB 2.0 parses every statement with, so DuckDB finds them wherever its own statements of the same kind may appear, and reports a malformed one as a :ref:`syntax error <ref-err-syntax>` with a ``LINE 1: ... ^`` pointer, as it does for its own SQL.

Two things follow from DuckDB reading the statement as tokens:

- ``CREATE SEMANTIC VIEW`` stores each expression as DuckDB's tokens spell it: a tab becomes a space, a run of blank lines one line break, and comments are dropped. ``GET_DDL`` and ``DESCRIBE`` show that spelling. The view behaves the same.
- A view name that is a reserved word must be quoted, as above.

DuckDB keeps the grammar a connection parses with per connection. The connection that runs ``LOAD semantic_views``, and every connection opened after it, can use the statements at once. A connection that was **already open** when another connection loaded the extension picks them up only when it next starts or finishes a query. A statement that fails to parse never starts one, so until that connection runs a statement DuckDB parses on its own, such as ``SELECT 1``, every semantic-view statement on it fails as a syntax error, and retrying it fails the same way. To avoid that, run ``LOAD semantic_views`` on the connection first (it is a no-op on a database that has the extension loaded), or open the connection after the ``LOAD``.

.. code-block:: python

   other = con.cursor()                 # opened before the LOAD
   con.execute("LOAD semantic_views")
   other.execute("LOAD semantic_views") # activates the statements on this connection
   other.execute("SHOW SEMANTIC VIEWS")


.. _explanation-txn-ddl-summary:

Summary
========

For most users, the everyday-visible behavior is:

- ``BEGIN ... ROLLBACK`` rolls back ``CREATE``, ``DROP``, and ``ALTER SEMANTIC VIEW``.
- Queries, ``SHOW`` and ``DESCRIBE`` inside an open transaction see its uncommitted semantic-view changes.

The other items on this page only matter in specific situations:

- Concurrent ``CREATE IF NOT EXISTS`` from two connections can produce a constraint or commit-conflict error on one of them; catch it and treat as success.
- Semantic views are single-catalog: run their DDL from the database you loaded the extension into, not from a ``USE``-d attached database.
- A connection that was already open when another one ran ``LOAD semantic_views`` needs one statement DuckDB parses on its own, such as its own ``LOAD semantic_views``, before it parses semantic-view statements. Retrying a failed semantic-view statement does not help.

See also:

- :ref:`explanation-txn-ddl-readonly` -- read-only database support and the bootstrap-then-reopen workflow.
- :ref:`explanation-txn-ddl-attach` -- single-catalog behavior with ``ATTACH`` / ``USE``.
- :ref:`ref-create-semantic-view` -- syntax for all four ``CREATE`` body forms.
- :ref:`ref-drop-semantic-view` -- ``DROP`` and ``DROP IF EXISTS``.
- :ref:`ref-alter-semantic-view` -- ``ALTER`` variants.
- :ref:`ref-error-messages` -- error catalog.
