.. meta::
   :description: Why each metric is computed at the grain of its own table, how multi-grain queries are assembled from one aggregate per grain, and which shapes are still refused

.. _explanation-metric-grain:

==========================================
Metric Grain and How Queries Are Assembled
==========================================

Grain is the idea that decides which numbers a semantic view can return. It is
worth meeting while you are designing a model, rather than the first time a
query comes back with a value you did not expect. This page explains what grain
is, why a metric's grain belongs to its table rather than to the query you
wrote, how the extension assembles a query whose metrics sit at different
grains, and which shapes it still refuses.

If you are here because a query raised ``fan trap detected``, the diagnostic
route is :ref:`howto-fan-traps`. This page is the modeling route.


.. _explanation-grain-what:

What Grain Means
================

The **grain** of a table is the level of detail one row represents. A
``customers`` table has one row per customer, so its grain is "one customer". An
``orders`` table has one row per order. A ``line_items`` table has one row per
item within an order. Reading down that list, the grain gets finer: each order
belongs to exactly one customer, and each line item belongs to exactly one
order.

Grain is what makes a sum meaningful. ``SUM(c.balance)`` over the ``customers``
table adds each customer's balance exactly once, because there is exactly one
row per customer. The same expression evaluated over a join of ``customers`` to
``orders`` adds each balance once per order that customer placed, which is not a
balance total -- it is a balance total weighted by order volume. Nothing in the
SQL looks wrong. Only the grain of the rows being summed changed.

.. tip::

   If you have modeled a star schema before, grain is the discipline you
   already apply when you decide what one row of a fact table means. Semantic
   views make that discipline explicit and enforce it at query time.


.. _explanation-grain-property:

Grain Belongs to the Metric's Table, Not to the Query
=====================================================

In a semantic view, every metric is declared against a table alias:

.. code-block:: sql

   METRICS (
       o.order_count   AS COUNT(*),
       c.total_balance AS SUM(c.balance)
   )

That prefix is not decoration. ``o.order_count`` counts rows of ``orders``;
``c.total_balance`` sums rows of ``customers``. The metric carries its grain
with it wherever it is queried, because the grain is a fact about where the
numbers live -- not about which dimensions a particular caller asked for.

This is the same rule Snowflake applies, and it is what lets one semantic view
serve queries that a single ``CREATE VIEW`` could not. A regular view has to
commit to one join shape and one ``GROUP BY``; a semantic view keeps each metric
attached to its own grain and works out the join shape per query. See
:ref:`explanation-sv-vs-views` for that contrast in full.

The practical consequence for modeling: **declare each metric on the table
whose rows it aggregates.** A customer-level metric belongs on the customer
table even if most of your queries start from orders. Declaring it on the orders
table to keep the definition tidy does not move the numbers to the order grain;
it only describes them incorrectly.


.. _explanation-grain-single:

A Single-Grain Query Is One SELECT
==================================

When every metric in a query sits at the same grain, the extension generates one
statement anchored at that grain's table -- the metrics' **home table** -- with a
``LEFT JOIN`` out to each table a queried dimension needs, and one ``GROUP BY``.

Take the three-table shop model from :ref:`tutorial-multi-table`: both
``revenue`` and ``order_count`` are declared on ``o`` (orders), so grouping
either of them by a customer or product dimension is a single-grain query,
anchored at ``orders``. Joining outward from orders to customers is safe because
each order has exactly one customer -- traversing from the "many" side to the
"one" side never duplicates a row.

.. versionchanged:: 0.14.0

   A metric on a table *below* the base table -- a line-item metric in a view
   whose base table is ``orders`` -- used to be computed from the base table,
   joined down to its rows. It is now computed from its own table like every
   other metric. This changes the numbers in two edge cases; see
   :ref:`explanation-grain-orphans`.


.. _explanation-grain-orphans:

Orphan Rows and Parents With No Children
========================================

Every metric is aggregated from its own home table, joining only *outward* --
from the "many" side to the "one" side -- to reach the dimensions the query asks
for. Two consequences are worth knowing, because they are where this differs
from the answer you would get by starting at a parent table and joining down.
Snowflake gives the same answers in both cases.

**A row whose foreign key is NULL is still counted.**
   A line item that belongs to no order (its ``order_id`` is ``NULL``, or names
   an order that does not exist) is a real line item. Counting line items by
   customer region counts it, under a ``NULL`` region -- the same ``NULL`` an
   outer join gives any attribute that cannot be looked up. The grand total of
   ``total_qty`` includes it too.

**A parent with no children has no row for a child metric.**
   An order with no line items has nothing for ``item_count`` to count, so
   querying ``item_count`` by order returns no row for it -- not a row reading
   ``0``. When a metric of the order's own grain is queried alongside, the row
   appears because *that* metric has a value for it, and ``item_count`` there is
   ``NULL``: "no rows at this grain", exactly as in the multi-grain example
   below.

Here is a small shop with both edge cases in its data. Customer 1 (EU) has
two orders with line items; customer 2 (US) has one order with none; and line
item 4 belongs to no order at all:

.. code-block:: sql

   CREATE TABLE customers (id INTEGER, region VARCHAR);
   INSERT INTO customers VALUES (1, 'EU'), (2, 'US');

   CREATE TABLE orders (id INTEGER, customer_id INTEGER);
   INSERT INTO orders VALUES (1, 1), (2, 1), (3, 2);

   CREATE TABLE line_items (id INTEGER, order_id INTEGER, qty INTEGER);
   INSERT INTO line_items VALUES
       (1, 1,    1),
       (2, 1,    2),
       (3, 2,    3),
       (4, NULL, 40);

   CREATE SEMANTIC VIEW shop_items AS
   TABLES (
       o  AS orders     PRIMARY KEY (id),
       c  AS customers  PRIMARY KEY (id),
       li AS line_items PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       item_order     AS li(order_id)   REFERENCES o
   )
   DIMENSIONS (
       c.cust_region AS c.region
   )
   METRICS (
       o.order_count AS COUNT(*),
       li.n_items    AS COUNT(*),
       li.total_qty  AS SUM(li.qty)
   );

The base table is ``orders``, but ``n_items`` and ``total_qty`` live on
``line_items``, so that is where they are aggregated:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop_items
       DIMENSIONS cust_region
       METRICS n_items, total_qty
   ) ORDER BY cust_region;

.. code-block:: text

   ┌─────────────┬─────────┬───────────┐
   │ cust_region │ n_items │ total_qty │
   ├─────────────┼─────────┼───────────┤
   │ EU          │       3 │         6 │
   │ NULL        │       1 │        40 │
   └─────────────┴─────────┴───────────┘

The orphan line item is counted under a ``NULL`` region, and US has no row,
because none of its orders has an item. Add the order-grain ``order_count`` and
US comes back, carried by the metric that has rows for it:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop_items
       DIMENSIONS cust_region
       METRICS n_items, total_qty, order_count
   ) ORDER BY cust_region;

.. code-block:: text

   ┌─────────────┬─────────┬───────────┬─────────────┐
   │ cust_region │ n_items │ total_qty │ order_count │
   ├─────────────┼─────────┼───────────┼─────────────┤
   │ EU          │       3 │         6 │           2 │
   │ US          │    NULL │      NULL │           1 │
   │ NULL        │       1 │        40 │        NULL │
   └─────────────┴─────────┴───────────┴─────────────┘

The most useful property follows from the rule rather than being added to it:
**a metric's value never depends on what else is in the query.** ``n_items`` for
EU is 3 whether it is queried alone, next to ``order_count``, or inside a
derived metric. Before v0.14.0 that was not guaranteed: the same metric could
read differently on its own than beside a metric from another table.

.. note::

   Only metrics are anchored this way. A dimensions-only query and a ``FACTS``
   query are row-level, and keep their earlier behavior: they start at the base
   table and drop the row a parent with no children would otherwise contribute.
   A row-level query on a child fact therefore still omits a child row whose
   foreign key is ``NULL``.


.. _explanation-grain-multi:

A Multi-Grain Query Is One Aggregate per Grain
==============================================

.. versionchanged:: 0.12.0

   Queries whose metrics sit at different grains are computed per grain and
   joined. Before, the generated SQL was always anchored at the base table, so
   these queries were rejected with a fan trap error rather than answered.

Consider a view whose base table is ``orders`` but which also carries a
customer-level metric:

.. code-block:: sql

   CREATE OR REPLACE TABLE customers (id INTEGER, region VARCHAR, balance DECIMAL(10,2));
   INSERT INTO customers VALUES
       (1, 'East', 500.00),
       (2, 'East', 300.00),
       (3, 'West', 900.00);

   CREATE OR REPLACE TABLE orders (id INTEGER, customer_id INTEGER, amount DECIMAL(10,2));
   INSERT INTO orders VALUES
       (1, 1, 25.00),
       (2, 1, 50.00),
       (3, 2, 75.00);

   CREATE SEMANTIC VIEW accounts AS
   TABLES (
       o AS orders    PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (
       c.region AS c.region
   )
   METRICS (
       o.order_count   AS COUNT(*),
       c.total_balance AS SUM(c.balance)
   );

Note the data: customer 1 placed two orders, customer 2 placed one, and customer
3 -- the only customer in the West region -- placed none.

Asking for both metrics by region is a two-grain query:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(accounts
       DIMENSIONS region
       METRICS order_count, total_balance
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬─────────────┬───────────────┐
   │ region │ order_count │ total_balance │
   ├────────┼─────────────┼───────────────┤
   │ East   │           3 │        800.00 │
   │ West   │        NULL │        900.00 │
   └────────┴─────────────┴───────────────┘

Both numbers are what a careful analyst would compute by hand: East has three
orders and two customers holding 800.00 between them, and West holds 900.00
while having placed nothing yet.

Anchoring the query at ``orders`` -- the only option before v0.12.0 -- would have
produced neither. Customer 1's balance would have been added once per order, so
East's total would read 1300.00, and West would have vanished entirely, because
no order row exists to carry it. That is why the base-anchored path rejected this
query with a fan trap error instead of answering it.


.. _explanation-grain-shapes:

The Shapes This Covers
----------------------

Three query shapes are answered per grain:

**A metric on a parent table the base table references.**
   ``c.total_balance`` above -- queried alone, or alongside dimensions at or
   above its own grain.

**Metrics at two different grains, queried together.**
   An order-grain metric with a line-item-grain metric (a *fan trap* shape), or
   metrics on two different children of the same parent (a *chasm trap* shape).

**A single derived metric that fuses two grains.**
   ``avg_items AS item_count / order_count``, where the numerator lives on
   ``line_items`` and the denominator on ``orders``. Each component is aggregated
   at its own grain and the arithmetic is evaluated over the two pre-aggregates,
   so the denominator is the true order count rather than a fanned one. See
   :ref:`howto-derived-metrics` for how derived metrics are declared.


.. _explanation-grain-assembly:

How the Grains Are Recombined
-----------------------------

Each grain becomes its own aggregate over its own table, and the results are
joined on the dimensions the query asked for:

.. code-block:: text

   grain 1:  aggregate the orders metrics     GROUP BY the queried dimensions
   grain 2:  aggregate the customers metrics  GROUP BY the queried dimensions
             └── combined on the queried dimensions
                 with a NULL-safe FULL OUTER JOIN

Two properties of that recombination matter when you read results:

- The join is a **NULL-safe** ``FULL OUTER JOIN``. A dimension group present at
  one grain but not another survives, carrying ``NULL`` for the metrics it has no
  rows for. That is the ``NULL`` in the West row above: West has a balance but no
  orders. Nothing is silently dropped, and ``NULL`` here means "no rows at this
  grain", not "zero".
- A query with **no dimensions** has nothing to join on, so the grains are
  combined with a ``CROSS JOIN``: each grain contributes its single grand-total
  row and the result is one row of columns.

This describes the shape of the generated SQL, not its literal text. To see
exactly what was generated for a given query, use
:ref:`explain_semantic_view() <ref-explain-semantic-view>`.

.. note::

   A single-grain query is the one-grain case of this: one aggregate, anchored at
   its own table, with nothing to join it to. Adding a metric at another grain
   adds a second aggregate; it does not change the first one.

One requirement relaxes on this path. A ``COUNT(*)`` metric used to need its
table to declare a ``PRIMARY KEY`` whenever that table was reached through a
``LEFT JOIN`` from the base table, whose NULL-extended rows ``COUNT(*)`` would
count. A table that anchors its own grain produces no such rows, so the key is
not required there. Since every metric is now anchored at its own table, a
``COUNT(*)`` on a child table without a key is answered too. Only the few shapes
that still fall back to the base table, such as the one-to-one window pair
described under :ref:`explanation-grain-refused`, need the key.


.. _explanation-grain-refused:

What Is Still Refused, and Why
==============================

Per-grain assembly widens what a semantic view can answer; it does not remove the
guard rails. Five shapes still raise an error, and the reason behind each one is
useful to keep in mind while you model.

**A dimension below a metric's grain.**
   Grouping ``c.total_balance`` by an order-grain or line-item-grain dimension is
   rejected, and no amount of per-grain machinery makes it answerable: a customer
   with orders in three statuses genuinely fans across those statuses, so there is
   no single correct balance to report per group. Snowflake refuses the same
   shape by the same rule -- `the logical table for the dimension must be related
   to the logical table for the metric
   <https://docs.snowflake.com/en/user-guide/views-semantic/querying>`_ and must
   have "an equal or lower level of granularity". Group by a dimension at or above
   the metric's grain instead, or split the query in two.

**Window metrics whose inner aggregates sit at different grains.**
   A window metric wraps an inner aggregate, and the window function runs over the
   already-grouped rows, so the window itself is not grain-sensitive -- the inner
   aggregate is. One window metric whose inner aggregate lives on a non-base table
   is computed at that table's grain. Two of them at *different* grains are not,
   because those grains would have to be joined before the window could run.
   Where joining the two tables would repeat one side's rows, the query is
   refused with a fan trap error. Where the relationship between them is
   one-to-one, nothing fans out and the pair is answered, but it is computed
   from the base table rather than per grain: an inner aggregate on a child of
   the base table then leaves out orphan child rows (a known limitation). See
   :ref:`howto-window-metrics`.

**A role-played table reached without** ``USING``.
   When one table is reachable through two named relationships -- ``flights``
   referencing ``airports`` once as departure and once as arrival -- a dimension
   on it means nothing until something says which role is intended. A co-queried
   metric's ``USING`` clause supplies that, and per-grain assembly honors it:
   each grain joins the named relationship under its own scoped alias. Without
   ``USING`` the query keeps the error rather than picking a relationship by
   declaration order, because picking silently is how a departure number ends up
   labeled as an arrival one. Declaring role-playing somewhere in a view does not
   cost the rest of that view its multi-grain queries -- the test is what the
   query reaches, not what the definition contains. See :ref:`howto-role-playing`.

**Anything other than a dimension sitting on a role-played table.**
   Only a queried dimension's expression is rewritten to a scoped alias, so a
   pre-aggregation filter member on a role-played table, a metric aggregated at
   one, or a table reachable only by passing through one all remain errors. An
   error is the right outcome here; the alternative is a plausible-looking wrong
   number.

**A role-played dimension queried together with an active semi-additive metric.**
   A snapshot group cannot carry a role, so the two grains would bind different
   instances of the same dimension -- one under the alias ``USING`` named, the
   other under whichever relationship was declared first -- and the outer join
   would silently compare them. Query that pair at a single grain instead.

Semi-additive metrics -- ``NON ADDITIVE BY`` snapshots such as account balances --
are otherwise computed at their own grain and can appear alongside metrics at other
grains. The snapshot ranking runs over the metric's own table rather than over a
join that has already duplicated its rows, so a latest balance is no longer added
once per order. See :ref:`howto-semi-additive`.


.. _explanation-grain-modelling:

What This Means for a Model
===========================

Three habits keep a model on the right side of these rules:

- **Declare each metric on the table whose rows it aggregates.** The alias prefix
  is the grain declaration; treat it as load-bearing.
- **Declare** ``PRIMARY KEY`` **and** ``UNIQUE`` **accurately in** ``TABLES``.
  Cardinality is inferred from them, and cardinality is what tells the extension
  which direction of a relationship is safe to traverse. A missing key makes a
  one-to-one relationship look many-to-one, so traversing it in reverse is treated
  as a fan-out, and that narrows what the view can answer.
- **Check before you query.**
  :ref:`SHOW SEMANTIC DIMENSIONS … FOR METRIC <ref-show-dims-for-metric>` applies
  the same grain rules at inspection time and lists only the dimensions that can
  legitimately be combined with a given metric.

.. tip::

   Grain also decides what a filter means. A predicate applied *before*
   aggregation changes which rows each grain aggregates over, while an outer
   ``WHERE`` filters only the assembled result. :ref:`howto-filtering` covers the
   difference.


.. _explanation-grain-further:

Further Reading
===============

- :ref:`howto-fan-traps` -- the diagnostic view of the same subject: what the
  ``fan trap detected`` error is telling you, and how to restructure around it.
- :ref:`explanation-snowflake` -- how this grain model lines up with Snowflake's,
  including the granularity rule quoted above.
- :ref:`tutorial-multi-table` -- the star-schema model these examples build on.
- :ref:`ref-semantic-view-function` -- the query function's parameters and the
  emission paths a predicate is injected into.
