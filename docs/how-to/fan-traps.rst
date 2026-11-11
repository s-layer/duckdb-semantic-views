.. meta::
   :description: How a metric grouped by a dimension below its grain is counted, when the fan trap error is raised, and how to fix it

.. _howto-fan-traps:

=====================================
How to Diagnose and Fix Fan Traps
=====================================

This guide shows how the extension counts a metric grouped or filtered by a dimension on the "many" side of a relationship, which of those queries still raise the fan trap error, and how to fix the query or the view that raises it.

**Prerequisites:**

- A working multi-table semantic view with relationships (see :ref:`tutorial-multi-table`)
- Understanding of cardinality concepts (one-to-one, many-to-one, one-to-many)


.. _howto-fan-what:

What Is a Fan Trap?
===================

A fan trap occurs when a query aggregates a metric from one table while grouping by a dimension from another table that is on the "many" side of a relationship. A plain join produces duplicate rows, inflating the aggregate result.

For example, consider orders and line items:

- Each order has many line items (one-to-many from orders to line items).
- ``COUNT(*)`` on orders counts one row per order.
- If you join orders to line items to get a line-item dimension, each order row is duplicated per line item.
- ``COUNT(*)`` on orders over that join returns the number of line items, not the number of orders.

The extension never aggregates over such a join. As in Snowflake, it counts **each row of the metric's table once in every group it reaches**: an order with a shipped and a pending line item counts once under ``shipped`` and once under ``pending``, and an order with no line items counts in the ``NULL`` group. Where it cannot count that way, it raises the fan trap error instead of returning an inflated result. For why a metric's grain belongs to its table, see :ref:`explanation-metric-grain`.


.. _howto-fan-detect:

How a Fan-Out Is Counted
========================

.. versionchanged:: 0.19.0

   A metric grouped or filtered by a dimension below its own grain is answered
   once per group, as in Snowflake. It used to be refused with ``fan trap
   detected``.

The extension infers cardinality from the ``PRIMARY KEY`` and ``UNIQUE`` declarations in the ``TABLES`` clause:

- If the FK columns on the "from" side of a relationship match a PK or UNIQUE constraint on that same table, the relationship is **one-to-one**.
- Otherwise, the relationship is **many-to-one** (the default).

Following a relationship many-to-one never duplicates rows, because each row on the "many" side maps to at most one row on the "one" side. Following one in reverse (one-to-many) is a **fan-out**. When a metric reaches a queried dimension, or a member the query's ``WHERE`` names, through a fan-out, the extension finds the distinct pairs of a metric-table row and a group, and aggregates the metric over those. Every one-to-many path is counted this way:

- straight down, such as orders by a line-item status, or customers by the status of their orders' line items;
- to a sibling after a hop up, such as line items by the mode of their order's shipments, when line items and shipments both reference orders;
- through several fan-outs, such as line items by the tag of their order's notes;
- across a many-to-many bridge (see :ref:`Many-to-many relationships through a bridge <ref-create-bridge-relationships>`).

A filter on such a dimension keeps a metric-table row when any row it reaches matches, and counts it once.

The pairs are told apart by the metric table's ``PRIMARY KEY``, so the metric's table needs one. Without it the fan-out is refused, as Snowflake refuses it.


.. _howto-fan-example:

Example: A Metric Grouped Below Its Grain
=========================================

.. code-block:: sql

   CREATE TABLE orders (id INTEGER, region VARCHAR);
   INSERT INTO orders VALUES (1, 'East'), (2, 'West');

   CREATE TABLE line_items (
       id INTEGER, order_id INTEGER,
       extended_price DOUBLE, status VARCHAR
   );
   INSERT INTO line_items VALUES
       (1, 1, 100.00, 'shipped'),
       (2, 1, 200.00, 'pending'),
       (3, 2, 150.00, 'shipped');

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       o  AS orders     PRIMARY KEY (id),
       li AS line_items PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       li_to_order AS li(order_id) REFERENCES o
   )
   DIMENSIONS (
       o.region     AS o.region,
       li.status    AS li.status
   )
   METRICS (
       li.revenue     AS SUM(li.extended_price),
       o.order_count  AS COUNT(*)
   );

``li.revenue`` grouped by ``o.region`` follows ``li_to_order`` many-to-one, so nothing fans out:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS region
       METRICS revenue
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬─────────┐
   │ region │ revenue │
   ├────────┼─────────┤
   │ East   │   300.0 │
   │ West   │   150.0 │
   └────────┴─────────┘

``o.order_count`` grouped by ``li.status`` follows it in reverse. Order 1 has a shipped and a pending line item and counts once in each group; order 2 has one shipped line item:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS status
       METRICS order_count
   ) ORDER BY status;

.. code-block:: text

   ┌─────────┬─────────────┐
   │ status  │ order_count │
   ├─────────┼─────────────┤
   │ pending │           1 │
   │ shipped │           2 │
   └─────────┴─────────────┘

The groups overlap: the counts add up to 3, but there are only two orders. Summing a fanned-out metric across its groups counts a row once for every group it is in. Query the metric without the dimension (``METRICS order_count`` alone gives 2) when you need the total.


.. _howto-fan-errors:

When the Extension Raises a Fan Trap Error
==========================================

The fan-out is refused with ``fan trap detected``, rather than counted once per group, when:

- the metric's table has no ``PRIMARY KEY``;
- the metric is semi-additive and its ``NON ADDITIVE BY`` dimension is not in the query;
- the fan-out crosses an ``ASOF`` or range relationship in reverse;
- the path to the dimension runs through a role-played table, or the query has a dimension or filter whose expression names no table.

For example, in a view where ``line_items`` (``li``, declared without a ``PRIMARY KEY``) and ``shipments`` (``sh``) both reference ``orders``, ``revenue`` grouped by the shipments' ``mode`` raises:

.. code-block:: text

   semantic view 'sales': fan trap detected -- metric 'revenue' (table 'li')
   would be duplicated when joined to dimension 'mode' (table 'sh') via
   relationship 'sh_to_order' (many-to-one cardinality, inferred: FK is not
   PK/UNIQUE). This would inflate aggregation results. Remove the dimension,
   use a metric from the same table, or restructure the relationship. A
   fan-out like this one is counted once per group only for ordinary metrics
   on a table with a PRIMARY KEY: not for a semi-additive metric whose NON
   ADDITIVE BY dimension is not queried, a role-played table, a query with a
   dimension or filter that names no table, or a metric table with no PRIMARY
   KEY.

Metrics that merely sit at *different grains from each other* are not an error either: they are computed one grain at a time and joined, see :ref:`howto-fan-per-grain`.


.. _howto-fan-fix:

How to Fix Fan Trap Errors
==========================

There are four approaches:

**1. Declare the metric table's primary key**

If the error comes from a metric table with no ``PRIMARY KEY``, declare one in ``TABLES`` (``li AS line_items PRIMARY KEY (id)``). The fan-out is then counted once per group.

**2. Remove the problematic dimension**

Query the metric with a dimension from the same table, or from a table it reaches many-to-one:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS region
       METRICS order_count
   );

**3. Use a metric from the same table as the dimension**

Instead of ``o.order_count`` with ``li.status``, use ``li.revenue`` with ``li.status``:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS status
       METRICS revenue
   );

**4. Restructure the view**

If you want each order counted in exactly one group, decide which group an order with lines in several statuses belongs to, then model that answer. For example, add an order-level status column to ``orders`` and declare the dimension there, or pre-aggregate the line items to one row per order and declare that table instead of ``line_items``.


.. _howto-fan-per-grain:

Query Metrics at Different Grains
=================================

.. versionchanged:: 0.12.0

   Queries whose metrics sit at different grains are answered instead of
   rejected with ``fan trap detected``.

Metrics from different tables in one query are not a fan trap on their own.
Each metric is aggregated over its own table, and the results are joined on
the queried dimensions. In the ``sales`` view above, ``order_count`` (on
``orders``) and ``revenue`` (on ``line_items``) can be queried together:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS region
       METRICS order_count, revenue
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬─────────────┬─────────┐
   │ region │ order_count │ revenue │
   ├────────┼─────────────┼─────────┤
   │ East   │           1 │   300.0 │
   │ West   │           1 │   150.0 │
   └────────┴─────────────┴─────────┘

The same applies to these shapes:

- A metric on a parent table, such as ``SUM(customers.balance)`` in a view
  built around ``orders``, queried alone or with dimensions at or above the
  customer grain.
- Metrics on two child tables of one parent (a *chasm trap*).
- A derived metric that combines two grains, such as
  ``order_total / item_count``.
- A window metric over any of these. The window is computed over its inner
  metric's result, so it sees the numbers that metric gives on its own (see
  :ref:`howto-window-relationships`).
- An active semi-additive metric (``NON ADDITIVE BY`` with its snapshot
  dimension left out of the query).
- A role-played table, when every metric's ``USING`` names a relationship to
  it (metrics naming different ones are grouped separately and merged).
- A dimension or filter across a many-to-many bridge table, such as a book
  metric grouped by author country through ``book_authors``. Each book counts
  once per group (see :ref:`Many-to-many relationships through a bridge <ref-create-bridge-relationships>`).

A dimension group that exists at one grain but not another is kept, with
``NULL`` for the metrics that have no rows there. For example, a region with
customers but no orders reports its balance and a ``NULL`` order count.

These multi-grain queries still raise the fan trap error. Query each part at a
single grain instead:

- A role-played dimension together with an active semi-additive metric.
- A role-played table reached by a metric with no ``USING`` to say which
  relationship is meant.
- An active semi-additive metric grouped or filtered across a many-to-many
  bridge.

:ref:`explanation-grain-multi` explains how the per-grain results are joined,
and :ref:`explanation-grain-refused` explains why these three shapes are
refused.


.. _howto-fan-other-shapes:

Shapes That Were Silently Inflated Before v0.11.0
=================================================

.. versionchanged:: 0.11.0

   Query shapes that inflate the same way as the classic fan trap previously
   slipped past the fan trap check and returned silently wrong numbers. They
   were refused from v0.11.0, and since v0.19.0 they are counted once per
   group like any other fan-out (see :ref:`howto-fan-detect`).

**A dimension below a metric's own grain, several levels down.**
   ``SUM(customers.balance)`` grouped by an order-grain or line-item-grain
   dimension: each customer counts once in every group its orders or line
   items reach.

**A dimension on a sibling table.**
   When two tables both reference a third -- ``line_items`` and ``shipments``
   both referencing ``orders`` -- a metric on one grouped by a dimension on the
   other used to be aggregated over the join of both children, multiplying each
   one's rows by the other's (an order with 2 line items and 2 shipments
   produced 4 rows). Each line item now counts once per shipment mode its order
   has.

**An active semi-additive metric queried alongside a fanning child dimension.**
   A ``NON ADDITIVE BY`` metric queried together with a dimension on a fanning
   child table ran its snapshot (``RANK``) query over the already-multiplied
   join, where ties across the fanned duplicates of one source row are
   indistinguishable from ties across distinct rows, so it could
   double-count. With its ``NON ADDITIVE BY`` dimension in the query it is an
   ordinary metric and is counted once per group; without it, it is still
   refused. Fix: snapshot only on dimensions the metric reaches many-to-one, or
   query the semi-additive metric without the fanning child dimension.

.. note::

   A semantic view whose ``RELATIONSHIPS`` form a **cycle** (``a`` references
   ``b`` and ``b`` references ``a``) parses successfully but such a definition
   is degenerate. Since v0.11.0 a query against it terminates with an error
   instead of hanging.


.. _howto-fan-onetoone:

One-to-One Relationships
========================

If the FK columns match a PK or UNIQUE constraint on the "from" side, the extension infers one-to-one cardinality. One-to-one relationships can be traversed in either direction without fan-out.

.. code-block:: sql

   CREATE TABLE order_details (order_id INTEGER, gift_wrap BOOLEAN, shipping_cost DOUBLE);
   INSERT INTO order_details VALUES (1, true, 5.0), (2, false, 7.5);

   CREATE SEMANTIC VIEW order_details_sv AS
   TABLES (
       o  AS orders        PRIMARY KEY (id),
       od AS order_details PRIMARY KEY (order_id) -- order_id is both PK and FK
   )
   RELATIONSHIPS (
       detail_to_order AS od(order_id) REFERENCES o
   )
   DIMENSIONS (
       o.region     AS o.region,
       od.gift_wrap AS od.gift_wrap
   )
   METRICS (
       o.order_count AS COUNT(*),
       od.shipping   AS SUM(od.shipping_cost)
   );

Because ``order_id`` is the PK of ``order_details``, the relationship is one-to-one. Metrics from either table can be grouped by dimensions from the other without any fan-out. Here ``order_count`` (on ``orders``) is grouped by ``gift_wrap`` (on ``order_details``):

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(order_details_sv
       DIMENSIONS gift_wrap
       METRICS order_count
   ) ORDER BY gift_wrap;

.. code-block:: text

   ┌───────────┬─────────────┐
   │ gift_wrap │ order_count │
   ├───────────┼─────────────┤
   │ false     │           1 │
   │ true      │           1 │
   └───────────┴─────────────┘

.. tip::

   Before writing a query, you can ask the extension which dimensions a specific metric can be grouped by. :ref:`SHOW SEMANTIC DIMENSIONS … FOR METRIC <ref-show-dims-for-metric>` lists them, those the metric is counted once per group by included; a dimension it leaves out is one the query refuses:

   .. code-block:: sql

      SHOW SEMANTIC DIMENSIONS IN sales FOR METRIC order_count;


.. _howto-fan-troubleshooting:

Troubleshooting
===============

**fan trap detected -- metric '...' would be duplicated when joined to dimension '...'**
   The dimension sits below the metric's grain, and the query is one of those
   listed in :ref:`howto-fan-errors`. Apply one of the fixes in
   :ref:`howto-fan-fix`, or run ``SHOW SEMANTIC DIMENSIONS IN <view> FOR METRIC
   <metric>`` to list the dimensions the metric can be grouped by.

**A metric's groups add up to more than its total**
   The query groups the metric by a dimension below its grain, so a row that
   reaches several groups counts in each (see :ref:`howto-fan-example`). Query
   the metric without that dimension for the total.

**The error says "inferred: FK is not PK/UNIQUE", but each row really has one match**
   The extension infers a relationship as one-to-one only when the foreign-key
   columns are declared ``PRIMARY KEY`` or ``UNIQUE`` on their own table in
   ``TABLES``. Declare the key, as in :ref:`howto-fan-onetoone`, and the
   relationship can be traversed in both directions.

**The fan trap error names a member you only filtered on**
   Tables that a ``WHERE`` predicate inside the query reaches are checked like a queried
   dimension's. See the troubleshooting section of :ref:`howto-filtering`.

**Some groups show NULL for one metric**
   In a multi-grain query, ``NULL`` means that group has no rows at that
   metric's grain -- for example, a region with customers but no orders. It is
   not zero. Wrap the column in ``COALESCE`` in the outer query if you want
   zeros.

**A child-table metric has a NULL group, or a parent is missing**
   .. versionchanged:: 0.14.0

   A metric on a table below the base table, such as a line-item count in a
   view built around ``orders``, is aggregated from its own table even when it
   is the only metric in the query. A child row whose foreign key is ``NULL``
   is counted under a ``NULL`` dimension value, and a parent with no children
   gets no row unless a metric on the parent's own table is queried alongside.
   Earlier versions computed such a metric from the base table, so these
   numbers can differ after an upgrade. To leave orphan rows out, declare a
   dimension on the parent's key, such as ``o.order_key AS o.id``, and add
   ``WHERE order_key IS NOT NULL`` to the query, which filters them before
   aggregation. It drops a line item whose ``order_id`` is ``NULL`` and one
   whose ``order_id`` names an order that does not exist, since neither finds
   an order to look up. Testing ``li.order_id IS NOT NULL`` instead would keep
   the second kind. See :ref:`explanation-grain-orphans`.


.. _howto-fan-related:

Related
=======

- :ref:`explanation-metric-grain` -- What grain is and why the extension refuses some shapes
- :ref:`ref-show-dims-for-metric` -- List the dimensions a metric can be grouped by
- :ref:`ref-create-relationships` -- How relationship cardinality is inferred
- :ref:`howto-role-playing` -- Relationships that reach one table by more than one route
