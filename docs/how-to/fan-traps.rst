.. meta::
   :description: Diagnose the fan trap error, fix the query or the view, and know which multi-grain queries the extension answers

.. _howto-fan-traps:

=====================================
How to Diagnose and Fix Fan Traps
=====================================

This guide shows how to recognize the fan trap error, fix the query or the view that triggers it, and tell which queries across several tables the extension answers rather than rejects.

**Prerequisites:**

- A working multi-table semantic view with relationships (see :ref:`tutorial-multi-table`)
- Understanding of cardinality concepts (one-to-one, many-to-one, one-to-many)


.. _howto-fan-what:

What Is a Fan Trap?
===================

A fan trap occurs when a query aggregates a metric from one table while grouping by a dimension from another table that is on the "many" side of a relationship. The join produces duplicate rows, inflating the aggregate result.

For example, consider orders and line items:

- Each order has many line items (one-to-many from orders to line items).
- ``COUNT(*)`` on orders counts one row per order.
- If you join orders to line items to get a line-item dimension, each order row is duplicated per line item.
- ``COUNT(*)`` on orders now returns the number of line items, not the number of orders.

The extension detects this pattern and raises an error instead of returning incorrect results. For why a metric's grain belongs to its table, see :ref:`explanation-metric-grain`.


.. _howto-fan-detect:

When the Extension Raises a Fan Trap Error
==========================================

The extension infers cardinality from the ``PRIMARY KEY`` and ``UNIQUE`` declarations in the ``TABLES`` clause:

- If the FK columns on the "from" side of a relationship match a PK or UNIQUE constraint on that same table, the relationship is **one-to-one**.
- Otherwise, the relationship is **many-to-one** (the default).

A fan trap error is raised when a metric's source table must traverse a relationship in the reverse direction (one-to-many) to reach a queried **dimension**. Traversing many-to-one is always safe, because each row on the "many" side maps to at most one row on the "one" side.

Metrics that merely sit at *different grains from each other* are not an error: since v0.12.0 they are computed one grain at a time and joined -- see :ref:`howto-fan-per-grain`.


.. _howto-fan-example:

Example: Fan Trap Detection in Action
======================================

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

**Safe query,** ``li.revenue`` grouped by ``o.region``:

The relationship ``li_to_order`` is many-to-one from ``li`` to ``o``. Traversing this direction is safe because each line item maps to one order.

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

**Blocked query,** ``o.order_count`` grouped by ``li.status``:

To reach ``li.status``, the extension must traverse from ``o`` to ``li``, the reverse of many-to-one, which is one-to-many. This would duplicate order rows, inflating the count.

.. code-block:: sql

   -- This query is blocked with a fan trap error:
   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS status
       METRICS order_count
   );

The error message identifies the metric, dimension, and relationship involved:

.. code-block:: text

   semantic view 'sales': fan trap detected -- metric 'order_count' (table 'o')
   would be duplicated when joined to dimension 'status' (table 'li') via
   relationship 'li_to_order' (many-to-one cardinality, inferred: FK is not
   PK/UNIQUE). This would inflate aggregation results. Remove the dimension,
   use a metric from the same table, or restructure the relationship.


.. _howto-fan-fix:

How to Fix Fan Trap Errors
==========================

There are three approaches:

**1. Remove the problematic dimension**

Query ``order_count`` with a dimension from the same table (``o``) or from a table reachable in the safe direction:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS region
       METRICS order_count
   );

**2. Use a metric from the same table as the dimension**

Instead of ``o.order_count`` with ``li.status``, use ``li.revenue`` with ``li.status``:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS status
       METRICS revenue
   );

**3. Restructure the view**

If you need the order count broken down by ``status``, decide what an order with lines in several statuses should count as, then model that answer. For example, add an order-level status column to ``orders`` and declare the dimension there, or pre-aggregate the line items to one row per order and declare that table instead of ``line_items``.


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
- A window metric whose inner aggregate is on a non-base table.
- An active semi-additive metric (``NON ADDITIVE BY`` with its snapshot
  dimension left out of the query).
- A role-played table, when a metric's ``USING`` names the relationship.

A dimension group that exists at one grain but not another is kept, with
``NULL`` for the metrics that have no rows there. For example, a region with
customers but no orders reports its balance and a ``NULL`` order count.

These multi-grain queries still raise the fan trap error. Query each part at a
single grain instead:

- Two window metrics whose inner aggregates sit at different grains.
- A role-played dimension together with an active semi-additive metric.
- A role-played table reached with no ``USING`` to say which relationship is
  meant.

:ref:`explanation-grain-multi` explains how the per-grain results are joined,
and :ref:`explanation-grain-refused` explains why these three shapes are
refused.


.. _howto-fan-other-shapes:

Other Shapes That Raise the Fan Trap Error
==========================================

.. versionchanged:: 0.11.0

   Query shapes that inflate the same way as the classic fan trap previously
   slipped past the fan trap check and returned silently wrong numbers. They
   now raise the same ``fan trap detected`` error.

**A dimension below a metric's own grain.**
   The classic case above (``order_count`` by ``li.status``), and its
   longer-range forms: ``SUM(customers.balance)`` grouped by an order-grain or
   line-item-grain dimension. Per-grain aggregation does not make these
   answerable -- each customer genuinely fans across their orders' statuses, so
   there is no single correct value per group. Snowflake rejects the same shape:
   its rule is that `the logical table for the dimension must be related to the
   logical table for the metric
   <https://docs.snowflake.com/en/user-guide/views-semantic/querying>`_ and must
   have "an equal or lower level of granularity than the logical table for the
   metric". Fix: use the fixes listed above.

**A dimension on a sibling table.**
   .. versionchanged:: 0.12.0

   When two tables both reference a third -- ``line_items`` and ``shipments``
   both referencing ``orders`` -- a metric on one grouped by a dimension on the
   other is a fan trap: joining both children multiplies each one's rows by the
   other's (an order with 2 line items and 2 shipments produces 4 rows). Neither
   sibling is an ancestor of the other, so this pair used to be skipped by the
   check and the query returned inflated numbers. It is now rejected. Fix: query
   the two sides separately -- their *metrics* together are fine, and are
   computed per grain.

**An active semi-additive metric queried alongside a fanning child dimension.**
   A ``NON ADDITIVE BY`` metric queried together with a dimension on a fanning
   child table ran its snapshot (``RANK``) query over the already-multiplied
   join, where ties across the fanned duplicates of one source row are
   indistinguishable from ties across distinct rows -- so it could
   double-count. Such metrics previously skipped the fan trap check on the
   assumption that the snapshot neutralized the fan; they now get the same
   check. Fix: snapshot only on safe, root-ward dimensions, or query the
   semi-additive metric without the fanning child dimension.

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

Because ``order_id`` is the PK of ``order_details``, the relationship is one-to-one. Metrics from either table can be grouped by dimensions from the other without triggering a fan trap. Here ``order_count`` (on ``orders``) is grouped by ``gift_wrap`` (on ``order_details``):

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

   Before writing a query, you can ask the extension which dimensions are safe to combine with a specific metric. :ref:`SHOW SEMANTIC DIMENSIONS … FOR METRIC <ref-show-dims-for-metric>` applies the same reachability rules at inspection time and returns only the dimensions that will not trigger a fan trap:

   .. code-block:: sql

      SHOW SEMANTIC DIMENSIONS IN sales FOR METRIC order_count;


.. _howto-fan-troubleshooting:

Troubleshooting
===============

**fan trap detected -- metric '...' would be duplicated when joined to dimension '...'**
   The dimension sits below the metric's grain. Apply one of the fixes in
   :ref:`howto-fan-fix`, or run ``SHOW SEMANTIC DIMENSIONS IN <view> FOR METRIC
   <metric>`` to list the dimensions that combine with it safely.

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
