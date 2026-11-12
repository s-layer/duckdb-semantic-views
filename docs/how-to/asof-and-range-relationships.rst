.. meta::
   :description: Join each row to the record in effect at its date with an ASOF relationship, or to the period that holds its date with a range relationship

.. _howto-asof-range:

======================================================
How to Join on Dates with ASOF and Range Relationships
======================================================

.. versionadded:: 0.19.0

This guide shows how to relate a table to another by a date or a number rather than by an equal key: each order to the address its customer had on the order date (an **ASOF relationship**), or each signup to the campaign running on the signup date (a **range relationship**). Both are written as in Snowflake, and both give the answers Snowflake gives.

**Prerequisites:**

- A working multi-table semantic view (see :ref:`tutorial-multi-table`)
- Familiarity with the ``TABLES`` and ``RELATIONSHIPS`` clauses (see :ref:`ref-create-relationships`)


.. _howto-asof-range-choose:

Choose the Relationship
=======================

Both kinds join each row of the referencing table to at most one row of the target, so the target is the "one" side of a many-to-one relationship, as with an ordinary foreign key. They differ in what the target stores:

- **ASOF:** the target holds a history of changes, each row with the date it took effect, such as an address history or a price list. Each referencing row takes the latest target row that took effect on or before its own date. Nothing marks where a row stops applying: the next row replaces it.
- **Range:** the target holds periods with a start and an end, such as campaigns, fiscal quarters or tariff periods. Each referencing row takes the period that holds its date. Gaps between periods are allowed, and a row that falls in a gap matches nothing.

In both, a row that matches nothing keeps its place in the answer, with ``NULL`` for the target's dimensions, as a ``LEFT JOIN`` would give.


.. _howto-asof:

Join to the Latest Row with ASOF
================================

The example tracks customers who move. Customer 1 lived in Oslo, then Berlin from June; customer 2 moved to Rome in March; customer 3 has no address on file; and the Lisbon address has no orders:

.. code-block:: sql

   CREATE TABLE customer_addresses (cust_id INTEGER, start_date DATE, city VARCHAR);
   INSERT INTO customer_addresses VALUES
       (1, '2024-01-01', 'Oslo'),
       (1, '2024-06-01', 'Berlin'),
       (2, '2024-03-01', 'Rome'),
       (4, '2024-01-01', 'Lisbon');

   CREATE TABLE orders (order_id INTEGER, cust_id INTEGER, order_date DATE, amount DECIMAL(10,2));
   INSERT INTO orders VALUES
       (1, 1, '2024-02-10', 100.00),
       (2, 1, '2024-06-01', 200.00),
       (3, 1, '2024-09-15',  50.00),
       (4, 2, '2024-02-01',  80.00),
       (5, 2, '2024-04-01', 120.00),
       (6, 3, '2024-05-01',  40.00);

Write ``ASOF`` before the target's date column in the ``REFERENCES`` list. The other column pairs are matched for equality, and the ``ASOF`` pair matches each order to the address with the greatest ``start_date`` on or before its ``order_date``:

.. code-block:: sql
   :emphasize-lines: 4,7

   CREATE SEMANTIC VIEW order_cities
   TABLES (
       o AS orders             PRIMARY KEY (order_id),
       a AS customer_addresses PRIMARY KEY (cust_id, start_date)
   )
   RELATIONSHIPS (
       order_address AS o(cust_id, order_date) REFERENCES a(cust_id, ASOF start_date)
   )
   DIMENSIONS (
       o.order_id AS o.order_id,
       a.city     AS a.city
   )
   METRICS (
       o.revenue       AS SUM(o.amount),
       o.order_count   AS COUNT(*),
       a.address_count AS COUNT(*)
   );

The referenced columns, ``ASOF`` column included, must be the target's ``PRIMARY KEY`` or a ``UNIQUE`` key, as for any relationship: an address history has one row per customer and start date. Only one column of a relationship can be ``ASOF``, and the two ``ASOF`` columns must be numbers, dates, times or timestamps that compare.

List each order with the city it was matched to:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(order_cities
       DIMENSIONS order_id, city
   ) ORDER BY order_id;

.. code-block:: text

   ┌──────────┬────────┐
   │ order_id │  city  │
   ├──────────┼────────┤
   │        1 │ Oslo   │
   │        2 │ Berlin │
   │        3 │ Berlin │
   │        4 │ NULL   │
   │        5 │ Rome   │
   │        6 │ NULL   │
   └──────────┴────────┘

Order 2 falls on the day the Berlin address starts, and the start day counts, as in Snowflake. Order 4 was placed before customer 2's first address, and customer 3 has none, so both have a ``NULL`` city. An order with a ``NULL`` date matches no address, and is kept with a ``NULL`` city too.

Group revenue by the city at the time of each order:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(order_cities
       DIMENSIONS city
       METRICS revenue, order_count
   ) ORDER BY city;

.. code-block:: text

   ┌────────┬─────────┬─────────────┐
   │  city  │ revenue │ order_count │
   ├────────┼─────────┼─────────────┤
   │ Berlin │  250.00 │           2 │
   │ Oslo   │  100.00 │           1 │
   │ Rome   │  120.00 │           1 │
   │ NULL   │  120.00 │           2 │
   └────────┴─────────┴─────────────┘

Each order counts once, and the unmatched orders form the ``NULL`` group, so the groups add up to the total: ``METRICS revenue, order_count`` alone gives 590.00 and 6. A metric on the address table is counted over the address table's own rows, the Lisbon address included:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(order_cities
       DIMENSIONS city
       METRICS revenue, address_count
   ) ORDER BY city;

.. code-block:: text

   ┌────────┬─────────┬───────────────┐
   │  city  │ revenue │ address_count │
   ├────────┼─────────┼───────────────┤
   │ Berlin │  250.00 │             1 │
   │ Lisbon │    NULL │             1 │
   │ Oslo   │  100.00 │             1 │
   │ Rome   │  120.00 │             1 │
   │ NULL   │  120.00 │          NULL │
   └────────┴─────────┴───────────────┘

.. tip::

   Give the history table a ``PRIMARY KEY`` on the equality columns and the ``ASOF`` column together, and keep it true. When two rows share a customer and a start date, an order that matches them is joined to one of the two, and which one is not defined. Snowflake also picks one.


.. _howto-range:

Join to a Period with a Range Relationship
==========================================

The example counts signups by the marketing campaign running when each signup happened. The spring campaign ends on the day the always-on campaign starts, and the always-on campaign has no end date yet:

.. code-block:: sql

   CREATE TABLE campaigns (campaign_id INTEGER, name VARCHAR, start_date DATE, end_date DATE);
   INSERT INTO campaigns VALUES
       (1, 'Winter sale',   '2024-01-01', '2024-02-01'),
       (2, 'Spring launch', '2024-03-01', '2024-04-01'),
       (3, 'Always on',     '2024-04-01', NULL);

   CREATE TABLE signups (signup_id INTEGER, signup_date DATE, plan VARCHAR);
   INSERT INTO signups VALUES
       (1, '2024-01-05', 'pro'),
       (2, '2024-01-31', 'free'),
       (3, '2024-02-01', 'pro'),
       (4, '2024-03-10', 'pro'),
       (5, '2024-04-01', 'free'),
       (6, '2024-08-20', 'pro'),
       (7, NULL,         'free');

Declare the period on the target with a ``DISTINCT RANGE`` constraint, then write the relationship's ``REFERENCES`` list as the same ``BETWEEN ... AND ... EXCLUSIVE``:

.. code-block:: sql
   :emphasize-lines: 5,8

   CREATE SEMANTIC VIEW campaign_signups
   TABLES (
       s AS signups   PRIMARY KEY (signup_id),
       c AS campaigns PRIMARY KEY (campaign_id)
           CONSTRAINT campaign_dates DISTINCT RANGE BETWEEN start_date AND end_date EXCLUSIVE
   )
   RELATIONSHIPS (
       signup_campaign AS s(signup_date) REFERENCES c(BETWEEN start_date AND end_date EXCLUSIVE)
   )
   DIMENSIONS (
       s.signup_id AS s.signup_id,
       s.plan      AS s.plan,
       c.campaign  AS c.name
   )
   METRICS (
       s.signup_count   AS COUNT(*),
       c.campaign_count AS COUNT(*)
   );

Each signup takes the campaign with ``start_date <= signup_date`` and ``signup_date < end_date``: the start is included and the end excluded, which is what ``EXCLUSIVE`` says. A ``NULL`` bound leaves that side of the period open. The relationship has one foreign-key column, and the constraint must name the same two columns in the same order. The target also needs a ``PRIMARY KEY`` or ``UNIQUE`` key, as any relationship target does.

List each signup with its campaign:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(campaign_signups
       DIMENSIONS signup_id, campaign
   ) ORDER BY signup_id;

.. code-block:: text

   ┌───────────┬───────────────┐
   │ signup_id │   campaign    │
   ├───────────┼───────────────┤
   │         1 │ Winter sale   │
   │         2 │ Winter sale   │
   │         3 │ NULL          │
   │         4 │ Spring launch │
   │         5 │ Always on     │
   │         6 │ Always on     │
   │         7 │ NULL          │
   └───────────┴───────────────┘

Signup 3 falls on the winter sale's end date, which is excluded, and in the gap before the spring launch. Signup 5 falls on the day the spring launch ends and the always-on campaign starts, so it belongs to the always-on campaign. Signup 6 is inside the always-on campaign's open end, and signup 7 has no date.

Count signups per campaign:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(campaign_signups
       DIMENSIONS campaign
       METRICS signup_count
   ) ORDER BY campaign;

.. code-block:: text

   ┌───────────────┬──────────────┐
   │   campaign    │ signup_count │
   ├───────────────┼──────────────┤
   │ Always on     │            2 │
   │ Spring launch │            1 │
   │ Winter sale   │            2 │
   │ NULL          │            2 │
   └───────────────┴──────────────┘

``METRICS signup_count, campaign_count`` with no dimension gives 7 and 3: each metric is counted over its own table.

.. warning::

   The constraint is not checked against the data, as in Snowflake. If two periods overlap, a signup that falls in both counts under each of them when grouped by campaign, and once in a total that does not use the campaign table. Make the periods meet end to start, as above, if each row must belong to one period.


.. _howto-asof-range-grain:

What You Can Group By
=====================

The referencing table is the "many" side: many orders share one address, and many signups one campaign. So, as with any many-to-one relationship:

- A metric on the referencing table can be grouped and filtered by the target's dimensions, as above. ``METRICS revenue WHERE city = 'Berlin'`` gives 250.00.
- A metric on the target can be grouped by its own dimensions, or by those of a table the target references.
- A metric on the target cannot be grouped or filtered by a dimension of the referencing table, such as an address count by order, or a campaign count by plan. That would cross the relationship from the "one" side, which an ordinary relationship answers by counting each row once per group, but an ASOF or range relationship does not: the query is refused as a fan trap, as Snowflake refuses it for ASOF.

.. code-block:: sql

   -- Refused: campaign_count is on c, plan is on s.
   SELECT * FROM SEMANTIC_VIEW(campaign_signups
       DIMENSIONS plan
       METRICS campaign_count
   );

.. code-block:: text

   semantic view 'campaign_signups': fan trap detected -- metric 'campaign_count'
   (table 'c') would be duplicated when joined to dimension 'plan' (table 's') via
   relationship 'signup_campaign' (many-to-one cardinality, inferred: FK is not
   PK/UNIQUE). This would inflate aggregation results. Remove the dimension, use
   a metric from the same table, or restructure the relationship.

:ref:`SHOW SEMANTIC DIMENSIONS ... FOR METRIC <ref-show-dims-for-metric>` leaves such dimensions out: for ``address_count`` it lists only ``city``.

An ASOF or range relationship is many-to-one even when its foreign-key columns are the referencing table's own key, since several rows can match the same target row.


.. _howto-asof-range-inspect:

Inspect the Relationship
========================

:ref:`explain_semantic_view() <ref-explain-semantic-view>` shows the join. An ASOF relationship becomes DuckDB's ``ASOF LEFT JOIN``:

.. code-block:: duckdb-sql

   SELECT * FROM explain_semantic_view('order_cities',
       dimensions := ['city'],
       metrics := ['revenue']
   );

.. code-block:: text

   -- Expanded SQL:
   SELECT
       a.city AS "city",
       SUM(o.amount) AS "revenue"
   FROM "memory"."main"."orders" AS "o"
   ASOF LEFT JOIN "memory"."main"."customer_addresses" AS "a" ON "o"."cust_id" = "a"."cust_id" AND "o"."order_date" >= "a"."start_date"
   GROUP BY
       1

A range relationship becomes a ``LEFT JOIN`` whose ``ON`` condition compares the signup date with both bounds, a ``NULL`` bound comparing as open.

:ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` writes the special column in the relationship's ``REF_KEY`` row as Snowflake does, and shows a ``DISTINCT RANGE`` constraint as ``CONSTRAINT`` rows:

.. code-block:: sql

   SELECT object_kind, object_name, property, property_value
   FROM (DESCRIBE SEMANTIC VIEW campaign_signups)
   WHERE object_kind IN ('CONSTRAINT', 'RELATIONSHIP');

.. code-block:: text

   ┌──────────────┬─────────────────┬─────────────────┬────────────────────────────────────────────────────────────────────────┐
   │ object_kind  │   object_name   │    property     │                             property_value                             │
   ├──────────────┼─────────────────┼─────────────────┼────────────────────────────────────────────────────────────────────────┤
   │ CONSTRAINT   │ campaign_dates  │ CONSTRAINT_TYPE │ DISTINCT_RANGE                                                         │
   │ CONSTRAINT   │ campaign_dates  │ START_COLUMN    │ start_date                                                             │
   │ CONSTRAINT   │ campaign_dates  │ END_COLUMN      │ end_date                                                               │
   │ RELATIONSHIP │ signup_campaign │ TABLE           │ signups                                                                │
   │ RELATIONSHIP │ signup_campaign │ REF_TABLE       │ campaigns                                                              │
   │ RELATIONSHIP │ signup_campaign │ FOREIGN_KEY     │ ["signup_date"]                                                        │
   │ RELATIONSHIP │ signup_campaign │ REF_KEY         │ [{"start_column":"start_date","end_column":"end_date","type":"RANGE"}] │
   └──────────────┴─────────────────┴─────────────────┴────────────────────────────────────────────────────────────────────────┘

For ``order_cities`` the ``REF_KEY`` row reads ``["cust_id",{"type":"ASOF","column":"start_date"}]``. :ref:`GET_DDL <ref-get-ddl>` writes both relationships back as you wrote them, with the keywords in upper case where Snowflake's ``GET_DDL`` writes ``asof`` and ``between ... exclusive``. The YAML export writes the ``ASOF`` column's position as ``asof:``, the range's end column as ``range_end:`` and the constraint under ``distinct_ranges:`` (see :ref:`ref-yaml-format`).

.. warning::

   A release of the extension older than 0.19.0 cannot read a view with an ASOF or range relationship, or with a ``DISTINCT RANGE`` constraint. Upgrade every DuckDB process that opens the database file before you create one.


.. _howto-asof-range-troubleshooting:

Troubleshooting
===============

``CREATE`` checks each of these, so a mistake refuses the statement rather than a later query.

**Relationship 'r' has more than one ASOF column; at most one referenced column can be ASOF.**
   Mark only the date column. The other columns are matched for equality.

**ASOF column o.city has type VARCHAR; an ASOF column must be numeric, a date, a time or a timestamp**
   The ``ASOF`` pair is compared with ``>=``, so it needs an ordered type, as in Snowflake. Match on a date or a number, casting in a :ref:`table defined by a query <howto-query-tables>` if the column is stored as text.

**FK (cust_id, start_date) in relationship 'order_address' does not match any PRIMARY KEY or UNIQUE constraint on table 'a'**
   The referenced columns, the ``ASOF`` one included, must be the target's key. Declare ``PRIMARY KEY (cust_id, start_date)`` on the history table.

**range relationship 'signup_campaign' references BETWEEN start_date AND end_date on table 'c', which has no matching DISTINCT RANGE constraint**
   Declare ``CONSTRAINT <name> DISTINCT RANGE BETWEEN start_date AND end_date EXCLUSIVE`` on the target. When the table declares one on other columns, or in the other order, the message names it.

**The range in relationship 'signup_campaign' must read 'BETWEEN start_column AND end_column EXCLUSIVE'**
   Only half-open periods are supported, as in Snowflake. Write ``EXCLUSIVE``, and store the day after the last day as the end.

**range relationship 'signup_campaign' does not bind: Binder Error: Cannot compare values of type VARCHAR and type DATE**
   The foreign-key column and the two bounds must compare. Cast the text column in a table defined by a query.

**fan trap detected -- metric 'campaign_count' (table 'c') would be duplicated when joined to dimension 'plan' (table 's')**
   A metric on the target is grouped or filtered by the referencing table. See :ref:`howto-asof-range-grain`.

See :ref:`ref-err-asof` and :ref:`ref-err-range` for every message.


.. _howto-asof-range-related:

Related
=======

- :ref:`ASOF relationships <ref-create-relationships>` and :ref:`range relationships <ref-create-range-relationships>` -- Syntax and rules in the ``CREATE SEMANTIC VIEW`` reference
- :ref:`howto-many-to-many` -- Relationships through a bridge table
- :ref:`howto-fan-traps` -- Grouping a metric by a dimension below its grain
- :ref:`explanation-snowflake` -- How these relationships compare with Snowflake's
