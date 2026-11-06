.. meta::
   :description: Change an existing semantic view in place with CREATE OR ALTER, keeping its creation time, tags and still-valid materializations, and check YAML definitions before deploying them

.. _howto-create-or-alter:

==================================================
How to Change a View in Place with CREATE OR ALTER
==================================================

.. versionadded:: 0.18.0

This guide shows how to keep a semantic view in step with a definition you maintain in a script or a YAML file, by running ``CREATE OR ALTER SEMANTIC VIEW`` on every deploy. As in Snowflake, the statement creates the view when it is missing and otherwise alters it to match, so running the same statement twice changes nothing. Unlike ``CREATE OR REPLACE``, it keeps what the statement cannot state: the view's creation time, its tags, and the materializations the change leaves valid.

**Prerequisites:**

- A semantic view you change over time, or a definition you deploy repeatedly
- For the YAML workflow: familiarity with :ref:`howto-yaml-definitions`


.. _howto-coa-choose:

Choose Between OR ALTER and OR REPLACE
======================================

Both statements end with the view matching the statement's definition. They differ in what survives from the view they change:

.. list-table::
   :header-rows: 1
   :widths: 34 33 33

   * - Kept from the existing view
     - ``CREATE OR ALTER``
     - ``CREATE OR REPLACE``
   * - Creation time (``created_on``)
     - Kept
     - Set to now
   * - Tags on the view
     - Kept
     - Only those the statement gives
   * - Tags on tables, facts, dimensions and metrics
     - Kept for each one the statement keeps by name
     - Only those the statement gives
   * - Materializations, when the statement has no ``MATERIALIZATIONS`` clause
     - Kept while the query each one answers is unchanged
     - None
   * - Anything else: comments, synonyms, members, Cortex Analyst metadata
     - As the statement gives it
     - As the statement gives it

Use ``CREATE OR ALTER`` for a deployment that runs on every release. Use ``CREATE OR REPLACE`` when you want to set every tag in one statement, or start the view afresh.


.. _howto-coa-setup:

Set Up an Example View
======================

The examples use a view of orders with a comment, tags and one materialization over a pre-aggregated table:

.. code-block:: sql

   CREATE TABLE orders (id INTEGER, region VARCHAR, status VARCHAR, amount DECIMAL(10,2));
   INSERT INTO orders VALUES
       (1, 'East', 'shipped',  100.00),
       (2, 'West', 'shipped',  200.00),
       (3, 'East', 'refunded', 150.00);

   CREATE TABLE revenue_by_region AS
       SELECT region, SUM(amount) AS revenue FROM orders GROUP BY region;

   CREATE SEMANTIC VIEW sales
   TABLES (
       o AS orders PRIMARY KEY (id) WITH TAG (domain = 'finance')
   )
   FACTS (
       o.net_amount AS o.amount
   )
   DIMENSIONS (
       o.region AS o.region COMMENT = 'Sales region',
       o.status AS o.status
   )
   METRICS (
       o.revenue AS SUM(o.net_amount) WITH TAG (certified = 'yes')
   )
   MATERIALIZATIONS (
       by_region AS (TABLE revenue_by_region, DIMENSIONS (region), METRICS (revenue))
   )
   COMMENT = 'Order revenue';

   ALTER SEMANTIC VIEW sales SET TAG owner = 'finance';


.. _howto-coa-change:

Change the View
===============

Write the whole definition you want, without tags, and run it with ``CREATE OR ALTER``. This one adds an ``order_count`` metric and renames ``status`` to ``order_status``:

.. code-block:: sql
   :emphasize-lines: 1,10,14

   CREATE OR ALTER SEMANTIC VIEW sales
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   FACTS (
       o.net_amount AS o.amount
   )
   DIMENSIONS (
       o.region       AS o.region COMMENT = 'Sales region',
       o.order_status AS o.status
   )
   METRICS (
       o.revenue     AS SUM(o.net_amount),
       o.order_count AS COUNT(*)
   )
   COMMENT = 'Order revenue';

What changed and what was kept:

- ``order_count`` and ``order_status`` are new. ``status`` is gone: a renamed member is removed and added under its new name.
- The table's ``domain`` tag, ``revenue``'s ``certified`` tag and the view's ``owner`` tag are still there, although the statement gives none. :ref:`GET_DDL <ref-get-ddl>` shows them.
- ``created_on`` in :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>` is the time of the first ``CREATE``.
- ``by_region`` is still declared, because ``region`` and ``revenue`` mean what they meant before. :ref:`SHOW SEMANTIC MATERIALIZATIONS <ref-show-semantic-materializations>` lists it:

.. code-block:: sql

   SELECT name FROM (SHOW SEMANTIC MATERIALIZATIONS IN sales);

.. code-block:: text

   ┌───────────┐
   │   name    │
   ├───────────┤
   │ by_region │
   └───────────┘

Run the same statement again and nothing changes. That is what makes it safe to run on every deploy.

.. warning::

   **What the statement leaves out, it unsets.** The statement is the whole definition, not a list of changes. Leave out ``COMMENT = 'Order revenue'`` and the view loses its comment; leave out ``COMMENT = 'Sales region'`` and the dimension loses it; leave out a ``MAX_STALENESS`` or ``WITH SYNONYMS`` and it is removed. A member the statement no longer lists is dropped. Keep the complete definition in your script or YAML file, and change it there.


.. _howto-coa-materializations:

Keep Materializations Correct
=============================

A materialization's table holds numbers computed under the old definition. ``CREATE OR ALTER`` keeps a materialization only while the query it answers is the same under the new definition, so a query is never routed to a table the change made stale. A change to anything a covered member reads -- the member's expression, a fact it uses, its table, a variable's default -- drops the materialization.

Count refunded orders as zero revenue by changing the fact that ``revenue`` sums:

.. code-block:: sql
   :emphasize-lines: 6

   CREATE OR ALTER SEMANTIC VIEW sales
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   FACTS (
       o.net_amount AS CASE WHEN o.status = 'refunded' THEN 0 ELSE o.amount END
   )
   DIMENSIONS (
       o.region       AS o.region COMMENT = 'Sales region',
       o.order_status AS o.status
   )
   METRICS (
       o.revenue     AS SUM(o.net_amount),
       o.order_count AS COUNT(*)
   )
   COMMENT = 'Order revenue';

``revenue_by_region`` still holds East's 250, which counts the refund, so ``by_region`` is dropped and ``SHOW SEMANTIC MATERIALIZATIONS IN sales`` returns no rows. Queries compute from ``orders`` until you declare a materialization again.

To bring it back, rebuild the table under the new rule and add a ``MATERIALIZATIONS`` clause to the statement. A ``MATERIALIZATIONS`` clause replaces the view's materializations with the ones it lists:

.. code-block:: sql
   :emphasize-lines: 1-3,20-22

   CREATE OR REPLACE TABLE revenue_by_region AS
       SELECT region, SUM(CASE WHEN status = 'refunded' THEN 0 ELSE amount END) AS revenue
       FROM orders GROUP BY region;

   CREATE OR ALTER SEMANTIC VIEW sales
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   FACTS (
       o.net_amount AS CASE WHEN o.status = 'refunded' THEN 0 ELSE o.amount END
   )
   DIMENSIONS (
       o.region       AS o.region COMMENT = 'Sales region',
       o.order_status AS o.status
   )
   METRICS (
       o.revenue     AS SUM(o.net_amount),
       o.order_count AS COUNT(*)
   )
   MATERIALIZATIONS (
       by_region AS (TABLE revenue_by_region, DIMENSIONS (region), METRICS (revenue))
   )
   COMMENT = 'Order revenue';

A materialization is also dropped when its table would resolve to another table: run from another schema or database than the view was created from, an unqualified table name means a table there. Run the statement from the same schema each time, or write materialization tables schema-qualified (``main.revenue_by_region``).

.. warning::

   Snowflake suspends a materialization that ``CREATE OR ALTER`` affects; this extension drops it, since it does not build or refresh materializations itself. See :ref:`howto-materializations`.


.. _howto-coa-tags:

Change Tags
===========

``CREATE OR ALTER`` cannot set tags, as in Snowflake: a ``WITH TAG`` anywhere in the statement, or ``tags:`` in its YAML, is refused. Change tags around it instead:

- **The view's own tags:** run :ref:`ALTER SEMANTIC VIEW ... SET TAG / UNSET TAG <ref-alter-tags>` after the statement.

  .. code-block:: sql

     ALTER SEMANTIC VIEW sales SET TAG owner = 'revenue-ops', tier = 'gold';
     ALTER SEMANTIC VIEW sales UNSET TAG tier;

- **Tags on a table, fact, dimension or metric:** ``ALTER`` changes view-level tags only, so recreate the view with ``CREATE OR REPLACE`` giving every tag. That also resets ``created_on`` and the materializations to what the statement declares.

A member renamed by ``CREATE OR ALTER`` is a new member and has no tags.


.. _howto-coa-preview:

Preview a Change Before Committing It
=====================================

Semantic-view DDL runs in your transaction (see :ref:`explanation-transactional-ddl`), so you can run the statement, look at the result, and roll it back. Use this to see which materializations a change keeps. This change would count refunds in ``revenue`` again:

.. code-block:: sql
   :emphasize-lines: 1,8,20,23

   BEGIN;

   CREATE OR ALTER SEMANTIC VIEW sales
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   FACTS (
       o.net_amount AS o.amount
   )
   DIMENSIONS (
       o.region       AS o.region COMMENT = 'Sales region',
       o.order_status AS o.status
   )
   METRICS (
       o.revenue     AS SUM(o.net_amount),
       o.order_count AS COUNT(*)
   )
   COMMENT = 'Order revenue';

   SELECT name FROM (SHOW SEMANTIC MATERIALIZATIONS IN sales);  -- no rows: by_region would be dropped
   SELECT GET_DDL('SEMANTIC_VIEW', 'sales');

   ROLLBACK;  -- or COMMIT to keep it

After the ``ROLLBACK``, ``SHOW SEMANTIC MATERIALIZATIONS IN sales`` lists ``by_region`` again and the view is as it was.

A statement that is refused -- a member that does not bind, a tag in the statement -- leaves the view as it was, in or out of a transaction.


.. _howto-coa-yaml:

Deploy from a YAML File
=======================

``CREATE OR ALTER`` takes ``FROM YAML`` and ``FROM YAML FILE`` bodies too. This is the extension's form of the ``create_or_alter`` argument of Snowflake's ``SYSTEM$CREATE_SEMANTIC_VIEW_FROM_YAML``. Keep the definition in version control and load it on each deploy:

.. code-block:: sql

   CREATE OR ALTER SEMANTIC VIEW sales FROM YAML FILE 'semantic/sales.yaml';

To check a definition first without creating or changing anything, pass the view name and the YAML text to :ref:`verify_semantic_view_yaml() <ref-create-verify-yaml>`. It runs every check ``CREATE SEMANTIC VIEW ... FROM YAML`` runs, including binding each member against its tables, and returns Snowflake's success message, or fails with the error the ``CREATE`` would raise:

.. code-block:: duckdb-sql

   SELECT * FROM verify_semantic_view_yaml('sales', $$
   tables:
     - alias: o
       table: orders
       pk_columns: [id]
   dimensions:
     - name: region
       expr: o.region
       source_table: o
       comment: Sales region
   metrics:
     - name: revenue
       expr: SUM(o.amount)
       source_table: o
   comment: Order revenue
   $$);

.. code-block:: text

   ┌──────────────────────────────────────────────────────────────────────────────────┐
   │                                      status                                      │
   ├──────────────────────────────────────────────────────────────────────────────────┤
   │ YAML file is valid for creating a semantic view. No object has been created yet. │
   └──────────────────────────────────────────────────────────────────────────────────┘

It does not check whether a view of that name exists, and it applies the checks of a plain ``CREATE``, not ``CREATE OR ALTER``'s refusal of tags.

.. warning::

   YAML exported with :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>` from a tagged view contains ``tags:`` entries, and ``CREATE OR ALTER`` refuses them. Delete the ``tags:`` entries from the file you deploy with ``CREATE OR ALTER``: the view keeps the tags it has.


.. _howto-coa-troubleshooting:

Troubleshooting
===============

**CREATE OR ALTER SEMANTIC VIEW can't set tags, as in Snowflake, and dimension 'region' has one**
   Remove the ``WITH TAG`` (or YAML ``tags:``) from the statement. Set the view's tags with ``ALTER SEMANTIC VIEW ... SET TAG``, or use ``CREATE OR REPLACE`` to set tags on members. See :ref:`howto-coa-tags`.

**IF NOT EXISTS can't be used with CREATE OR ALTER SEMANTIC VIEW**
   ``CREATE OR ALTER`` already creates a missing view. Remove ``IF NOT EXISTS``.

**COPY GRANTS can't be used with CREATE OR ALTER SEMANTIC VIEW**
   Remove ``COPY GRANTS``. An altered view keeps what it has.

**The comment, synonyms or another property disappeared**
   The statement did not state it, so it was unset. Add it back to the definition you deploy. See the warning in :ref:`howto-coa-change`.

**A materialization disappeared**
   The change reached a member it covers, or the statement ran from another schema than before. Rebuild the table if its numbers changed, and declare it again in a ``MATERIALIZATIONS`` clause. See :ref:`howto-coa-materializations`.

**A dimension or metric lost its tags**
   It was renamed, which removes the old member and adds a new one. Recreate the view with ``CREATE OR REPLACE`` to tag it.

**A view in another schema was not changed**
   ``CREATE OR ALTER SEMANTIC VIEW staging.sales`` and ``main.sales`` are two views, and the statement takes nothing from the other. Qualify the name with the schema you mean.

See :ref:`ref-err-create-or-alter` for the messages.


.. _howto-coa-related:

Related
=======

- :ref:`ref-create-or-alter` -- What ``CREATE OR ALTER`` keeps and unsets
- :ref:`verify_semantic_view_yaml() <ref-create-verify-yaml>` -- Checking a YAML definition without creating it
- :ref:`ref-alter-semantic-view` -- ``ALTER SEMANTIC VIEW`` for renames, comments and view-level tags
- :ref:`howto-yaml-definitions` -- Import and export YAML definitions
- :ref:`howto-materializations` -- Declaring materializations and keeping their tables current
