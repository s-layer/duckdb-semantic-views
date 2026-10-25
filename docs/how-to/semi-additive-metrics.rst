.. meta::
   :description: Define semi-additive metrics with NON ADDITIVE BY for snapshot aggregation patterns like account balances and inventory levels

.. _howto-semi-additive:

===================================
How to Use Semi-Additive Metrics
===================================

This guide shows how to define metrics with ``NON ADDITIVE BY`` to handle snapshot data -- values that should not be summed across certain dimensions like time, but can be summed across others like customer or region.

**Prerequisites:**

- A working semantic view with ``TABLES``, ``DIMENSIONS``, and ``METRICS`` (see :ref:`tutorial-multi-table`)


.. _howto-semi-additive-snapshot:

Snapshot Data
=============

Semi-additive metrics solve a specific problem with snapshot data -- tables where each row records a point-in-time measurement rather than an event. For example, an ``accounts`` table might record daily balances:

.. code-block:: text

   ┌─────────────┬─────────────┬─────────┐
   │ report_date │ customer_id │ balance │
   ├─────────────┼─────────────┼─────────┤
   │ 2026-04-10  │ ACME        │     500 │
   │ 2026-04-10  │ Globex      │     300 │
   │ 2026-04-11  │ ACME        │     550 │
   │ 2026-04-11  │ Globex      │     280 │
   └─────────────┴─────────────┴─────────┘

If you query ``SUM(balance)`` grouped by ``customer_id`` across both dates, you get 1050 for ACME (500 + 550) -- but that is double-counting. The real current balance is 550. Summing across customers makes sense (ACME + Globex = 830 on April 11), but summing the same customer across dates does not.

``NON ADDITIVE BY`` tells the extension to pick one snapshot row per group (e.g., the latest ``report_date``) before aggregating, so you get correct totals without manual filtering.


.. _howto-semi-additive-define:

Define a Semi-Additive Metric
=============================

Add ``NON ADDITIVE BY (<dimension>)`` to a metric to declare which dimensions it should not be summed across. The clause sits between the metric name and the ``AS``. The extension selects the most recent (or earliest) snapshot row before aggregating.

.. code-block:: sql
   :emphasize-lines: 10

   CREATE SEMANTIC VIEW account_metrics AS
   TABLES (
       a AS accounts PRIMARY KEY (id)
   )
   DIMENSIONS (
       a.customer_id AS a.customer_id,
       a.report_date AS a.report_date
   )
   METRICS (
       a.total_balance NON ADDITIVE BY (report_date) AS SUM(a.balance)
   );

This declares that ``total_balance`` is non-additive by ``report_date``. When a query requests ``total_balance`` grouped by ``customer_id`` (without ``report_date``), the extension selects the latest snapshot row per customer before summing. The default direction (ascending) selects the **latest** snapshot, matching Snowflake -- no ``DESC`` is needed.


.. _howto-semi-additive-clause-order:

Clause Order: NON ADDITIVE BY Comes Before AS
=============================================

.. warning::

   ``NON ADDITIVE BY (...)`` is part of the metric's *declaration*, not part of
   its expression, so it goes **before** the ``AS``. The full metric form is:

   .. code-block:: sqlgrammar

      [ PRIVATE ] <alias>.<metric_name>
          [ USING ( <rel_name> [, ...] ) ]
          [ NON ADDITIVE BY ( <dim_name> [ ASC | DESC ] [ NULLS FIRST | NULLS LAST ] [, ...] ) ]
          AS <aggregate_expression>

   Writing the clause after the ``AS`` is a define-time error. ``CREATE
   SEMANTIC VIEW`` reports:

   .. code-block:: text

      'NON ADDITIVE BY (...)' must come BEFORE 'AS' in a metric entry.
      Form: 'alias.name [USING (...)] [NON ADDITIVE BY (...)] AS expr'.

   The same rule and the same error apply to the ``USING (...)`` clause used by
   :ref:`role-playing dimensions <howto-role-playing>`. Only ``OVER (...)``
   legitimately follows the ``AS`` -- see :ref:`howto-window-metrics`.

.. versionchanged:: 0.12.0

   The after-``AS`` form used to be *accepted*. The clause was absorbed into
   the metric expression, so the metric was stored as an ordinary additive
   metric with its snapshot semantics silently dropped: ``DESCRIBE SEMANTIC
   VIEW`` reported no non-additive dimensions, :ref:`GET_DDL <ref-get-ddl>` round-tripped the
   malformed text, and the only complaint arrived at query time as a parser
   error pointing inside generated SQL. If you have definitions written that
   way, move the clause ahead of the ``AS`` -- they will no longer create.


.. _howto-semi-additive-sort:

Sort Order and NULLS Placement
==============================

Each dimension in ``NON ADDITIVE BY`` accepts an optional sort order and NULLS placement. The rows are sorted by the non-additive dimensions, and the rows sharing the **last value** in that order are aggregated. Rows tied at that value all count. So:

- ``ASC`` (default) -- selects the **latest** snapshot row (matches Snowflake)
- ``DESC`` -- selects the **earliest** snapshot row
- ``NULLS FIRST`` -- a NULL dimension value wins (outranks every real snapshot)
- ``NULLS LAST`` -- a NULL dimension value never wins; the latest (or earliest) real snapshot is chosen

The default NULLS placement follows the sort direction, matching DuckDB and Snowflake: ``ASC`` defaults to ``NULLS LAST`` and ``DESC`` defaults to ``NULLS FIRST``. So a bare ``NON ADDITIVE BY (report_date)`` (latest, ``NULLS LAST``) never lets a NULL date win, whereas ``NON ADDITIVE BY (report_date DESC)`` (earliest, ``NULLS FIRST``) *does* -- add an explicit ``NULLS LAST`` if you want to exclude NULL keys regardless of direction.

.. code-block:: sql

   -- Latest balance (most recent report_date wins) -- the default
   a.total_balance NON ADDITIVE BY (report_date) AS SUM(a.balance)

   -- Earliest balance (oldest report_date wins)
   a.opening_balance NON ADDITIVE BY (report_date DESC) AS SUM(a.balance)

.. versionchanged:: 0.11.0

   The polarity of ``NON ADDITIVE BY`` was corrected to match Snowflake: the
   default (ascending) direction now selects the **latest** snapshot and
   ``DESC`` selects the **earliest**. Before this change the mapping was
   inverted.

   .. warning::

      This is a **breaking** change that silently inverts results rather than
      raising an error, so review every existing semi-additive metric when you
      upgrade:

      - A view that wrote ``NON ADDITIVE BY (d DESC)`` to get the **latest**
        snapshot should now drop the ``DESC`` (write ``NON ADDITIVE BY (d)``).
      - A view that wrote no direction to get the **earliest** snapshot should
        now add ``DESC`` (write ``NON ADDITIVE BY (d DESC)``).

      ``NULLS`` placement is unchanged and kept as declared -- only the sort
      direction is reversed internally.


.. _howto-semi-additive-multiple:

Multiple Non-Additive Dimensions
================================

A metric can be non-additive by more than one dimension. Each gets its own sort specification, and the whole list still precedes the ``AS``:

.. code-block:: sql

   a.snapshot_balance NON ADDITIVE BY (report_date, fiscal_period)
       AS SUM(a.balance)


.. _howto-semi-additive-behavior:

Snapshot Behavior
=================

Snapshot selection depends on whether the non-additive dimensions are in the query. The examples below use the ``accounts`` rows from :ref:`howto-semi-additive-snapshot`.

**Non-additive dimension not in the query (active):**
   The extension picks the snapshot rows for each group of the queried
   dimensions -- the latest ``report_date`` per ``customer_id`` here -- and
   aggregates only those. Every row tied at that date counts, so several
   accounts sharing the latest date within a group are all included.

.. code-block:: sql

   -- report_date not in query -> snapshot selection activated
   SELECT * FROM SEMANTIC_VIEW(account_metrics
       DIMENSIONS customer_id
       METRICS total_balance
   ) ORDER BY customer_id;

.. code-block:: text

   ┌─────────────┬───────────────┐
   │ customer_id │ total_balance │
   ├─────────────┼───────────────┤
   │ ACME        │           550 │
   │ Globex      │           280 │
   └─────────────┴───────────────┘

**Non-additive dimension in the query (effectively regular):**
   When all non-additive dimensions are included in the query, the metric behaves as a standard additive metric, with no snapshot selection. This matches Snowflake's behavior: "When the non-additive dimension is included in the query, the metric is calculated as a standard additive metric."

.. code-block:: sql

   -- report_date in query -> standard aggregation
   SELECT * FROM SEMANTIC_VIEW(account_metrics
       DIMENSIONS customer_id, report_date
       METRICS total_balance
   ) ORDER BY customer_id, report_date;

.. code-block:: text

   ┌─────────────┬─────────────┬───────────────┐
   │ customer_id │ report_date │ total_balance │
   ├─────────────┼─────────────┼───────────────┤
   │ ACME        │ 2026-04-10  │           500 │
   │ ACME        │ 2026-04-11  │           550 │
   │ Globex      │ 2026-04-10  │           300 │
   │ Globex      │ 2026-04-11  │           280 │
   └─────────────┴─────────────┴───────────────┘

**Mixed regular and semi-additive metrics:**
   Regular metrics and semi-additive metrics can be queried together. Only the
   semi-additive metrics are limited to the snapshot rows; regular metrics
   aggregate over all rows.

   A regular metric declared on the **same table** as the semi-additive metric
   must be a single aggregate call ``SUM/COUNT/AVG/MIN/MAX(<expression>)``.
   Shapes that do not fit (``COUNT(*)``, ``DISTINCT`` aggregates, arithmetic
   around the aggregate like ``SUM(x) * 0.1``, ``COALESCE``-wrapped aggregates,
   derived metrics) produce an error that tells you to query them separately
   from the semi-additive metric. A metric on a **different table**, such as a
   ``COUNT(*)`` of customers next to a balance on ``accounts``, has no such limit:
   it is computed at its own grain (see :ref:`explanation-grain-multi`).


.. _howto-semi-additive-verify:

Verify the Generated SQL
=========================

Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to check that snapshot selection is active:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('account_metrics',
       dimensions := ['customer_id'],
       metrics := ['total_balance']
   );

The function returns one ``explain_output`` column, one line per row. Its ``-- Expanded SQL:`` section shows the generated query:

.. code-block:: sql

   WITH __sv_snapshot AS (
       SELECT
           a.customer_id AS "customer_id",
           a.balance AS "__sv_semi_0",
           RANK() OVER (PARTITION BY a.customer_id ORDER BY a.report_date DESC NULLS LAST) AS "__sv_rn"
       FROM "memory"."main"."accounts" AS "a"
   )
   SELECT
       "customer_id" AS "customer_id",
       SUM(CASE WHEN "__sv_rn" = 1 THEN "__sv_semi_0" END) AS "total_balance"
   FROM __sv_snapshot
   GROUP BY
       1

A ``WITH __sv_snapshot`` step means snapshot selection is active. If the query includes every non-additive dimension, the step is absent and the metric is a plain ``SUM``. The output continues with a ``-- DuckDB Plan:`` section.

.. dropdown:: Why the generated ORDER BY is DESC

   The snapshot step ranks rows within each group and keeps rank 1, which is
   the *first* row in the window's ``ORDER BY``. To make rank 1 the **last**
   value in the declared order, the extension emits the reverse of the declared
   direction: the default (ascending) ``NON ADDITIVE BY (report_date)`` becomes
   ``ORDER BY a.report_date DESC`` above, so rank 1 is the latest snapshot,
   along with every row tied at that date. The declared ``NULLS`` placement is
   kept as written. When metrics in one query declare different ``NON ADDITIVE
   BY`` lists, each list gets its own ranking column.


.. _howto-semi-additive-restrictions:

Restrictions
============

.. warning::

   ``NON ADDITIVE BY`` and ``OVER`` (window function) cannot be combined on the same metric. A metric is either semi-additive or a window metric, not both. Attempting to use both produces a define-time error.


.. _howto-semi-additive-troubleshoot:

Troubleshooting
===============

**NON ADDITIVE BY must come BEFORE AS**
   The clause belongs between the metric name and the ``AS``, not after the aggregate
   expression. Rewrite ``a.balance AS SUM(a.v) NON ADDITIVE BY (d)`` as
   ``a.balance NON ADDITIVE BY (d) AS SUM(a.v)``. Parenthesizing the expression does
   not change this -- the clause is still rejected.

**NON ADDITIVE BY dimension not found**
   The dimension name in ``NON ADDITIVE BY`` must match a declared dimension in the view. The error message identifies which dimension name is unrecognized: ``NON ADDITIVE BY dimension 'X' on metric 'Y' does not match any declared dimension``.

**Unexpected aggregation results**
   Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to check for the ``WITH __sv_snapshot`` step (see :ref:`howto-semi-additive-verify`). If all non-additive dimensions are in the query, the metric behaves as a regular additive metric and the step is absent. Remove the non-additive dimension from the query to activate snapshot selection.

**Slower queries with several non-additive dimension sets**
   When semi-additive metrics in one query declare different ``NON ADDITIVE BY`` dimensions, the extension ranks the rows once per distinct list. The results are correct, but each extra list adds a window function to the query. Query those metrics separately if that matters.

**A co-queried metric is rejected: cannot be co-queried with semi-additive metric**
   A regular metric on the same table as the semi-additive metric is not a single ``SUM/COUNT/AVG/MIN/MAX(<expression>)`` call -- ``COUNT(*)``, for example. Query the two metrics separately, or rewrite the regular metric as a single aggregate call over a column, such as ``COUNT(a.id)``.


.. _howto-semi-additive-related:

Related
=======

- :ref:`ref-create-metrics` -- Full metric grammar, including ``NON ADDITIVE BY``
- :ref:`explanation-metric-grain` -- How semi-additive metrics combine with metrics at other grains
- :ref:`howto-window-metrics` -- The other kind of metric that depends on the queried dimensions
- :ref:`howto-materializations` -- Why semi-additive metrics are never routed to a materialization
