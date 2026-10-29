.. meta::
   :description: Join the same table under multiple relationship aliases and use USING on metrics to resolve which path a query should traverse

.. _howto-role-playing:

============================================
How to Model Role-Playing Dimensions
============================================

This guide shows how to handle the role-playing dimension pattern, where the same table is joined via multiple relationships. A common example is an ``airports`` table joined to ``flights`` as both the departure airport and the arrival airport. The ``USING`` keyword on metrics tells the extension which join path to use.

**Prerequisites:**

- A working multi-table semantic view with relationships (see :ref:`tutorial-multi-table`)
- Understanding of how ``RELATIONSHIPS`` maps to JOIN clauses


.. _howto-rp-problem:

The Problem
===========

When the same table appears as the target of multiple relationships, dimensions from that table are ambiguous. The extension cannot determine which join path to use unless every metric in the query names one with a ``USING`` clause.

Consider flights with departure and arrival airports:

.. code-block:: sql

   CREATE TABLE airports (airport_code VARCHAR, city VARCHAR, country VARCHAR);
   CREATE TABLE flights (
       flight_id INTEGER,
       departure_code VARCHAR,
       arrival_code VARCHAR,
       carrier VARCHAR
   );

Both ``departure_code`` and ``arrival_code`` point to ``airports``. A dimension like ``city`` from ``airports`` is ambiguous: is it the departure city or the arrival city?


.. _howto-rp-define:

Define a Role-Playing View
==========================

Declare two named relationships to the same target table. Then use ``USING`` on metrics to select which relationship path each metric traverses.

.. code-block:: sql
   :emphasize-lines: 8,9,16,17

   CREATE SEMANTIC VIEW flight_analytics AS
   TABLES (
       f AS flights  PRIMARY KEY (flight_id),
       a AS airports PRIMARY KEY (airport_code)
   )
   RELATIONSHIPS (
       dep_airport AS f(departure_code) REFERENCES a,
       arr_airport AS f(arrival_code)   REFERENCES a
   )
   DIMENSIONS (
       a.city    AS a.city,
       a.country AS a.country,
       f.carrier AS f.carrier
   )
   METRICS (
       f.departure_count USING (dep_airport) AS COUNT(*),
       f.arrival_count   USING (arr_airport) AS COUNT(*)
   );

The ``USING (dep_airport)`` clause tells the extension: when this metric is queried alongside a dimension from the ``airports`` table, use the ``dep_airport`` relationship to resolve the join path.

Snowflake's documentation writes the same view with one plain count and a
metric per role over it. A metric can name another metric on its own table,
and the referenced aggregate is computed through the referencing metric's
``USING`` path:

.. code-block:: sql

   METRICS (
       f.flight_count    AS COUNT(*),
       f.departure_count USING (dep_airport) AS f.flight_count,
       f.arrival_count   USING (arr_airport) AS f.flight_count
   );

Both forms give the same numbers. See :ref:`ref-create-metrics` for the rules.


.. _howto-rp-query:

Query with USING Context
========================

**Departures by city:** the ``departure_count`` metric's USING context resolves ``city`` via ``dep_airport``:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(flight_analytics
       DIMENSIONS city
       METRICS departure_count
   ) ORDER BY city;

**Arrivals by city:** the ``arrival_count`` metric's USING context resolves ``city`` via ``arr_airport``:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(flight_analytics
       DIMENSIONS city
       METRICS arrival_count
   ) ORDER BY city;

**Departures and arrivals by city in one query:** each metric is grouped by
``city`` through its own relationship, and the two results are merged on
``city``. A city that only one role reaches gets ``NULL`` for the other
metric:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(flight_analytics
       DIMENSIONS city
       METRICS departure_count, arrival_count
   ) ORDER BY city;

.. versionadded:: 0.18.0

   Metrics with different ``USING`` paths to the same table can be queried
   together with a dimension on that table. Before, the query was refused as
   ambiguous.

**Every metric needs a path:** a metric without ``USING`` cannot say which
airport a city is, so grouping it by ``city`` is refused, also when another
metric in the query names a path:

.. code-block:: sql

   -- Refused: flight_count names no relationship to airports.
   SELECT * FROM SEMANTIC_VIEW(flight_analytics
       DIMENSIONS city
       METRICS departure_count, flight_count
   );

.. versionchanged:: 0.18.0

   A metric without ``USING`` used to borrow the path of a co-queried metric
   that had one, so its numbers changed with what else was in the query. It is
   now refused, as in Snowflake.

A dimension one relationship further on follows the same path. If
``airports`` references a ``regions`` table, ``departure_count`` by a region
dimension groups by the departure airport's region, and ``departure_count,
arrival_count`` by region merges the two per region, as by city.

A window metric names its path through its inner metric: a window over
``departure_count`` is grouped by departure city. Two window metrics over
different paths cannot be grouped by ``city`` together, and neither can metrics
on different paths beside a semi-additive metric whose ``NON ADDITIVE BY``
dimension is not queried; query those metrics separately.

A metric on a table that has no relationship to ``airports``, such as a count
of flight legs on a ``legs`` table under ``flights``, cannot name a path,
because a ``USING`` relationship must start at the metric's own table. Grouping
it by ``city`` is refused, as in Snowflake.

**Non-ambiguous dimensions:** the ``carrier`` dimension comes from the ``flights`` table (not ``airports``), so it works with any metric without ambiguity:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(flight_analytics
       DIMENSIONS carrier
       METRICS departure_count, arrival_count
   ) ORDER BY carrier;


.. _howto-rp-derived:

Derived Metrics with Role-Playing
=================================

A derived metric cannot have ``USING`` itself, but it can combine metrics that do:

.. code-block:: sql

   METRICS (
       f.departure_count USING (dep_airport) AS COUNT(*),
       f.arrival_count   USING (arr_airport) AS COUNT(*),
       total_flights     AS departure_count + arrival_count
   );

``total_flights`` depends on both ``dep_airport`` and ``arr_airport``. Querying ``total_flights`` with a non-ambiguous dimension like ``carrier`` works:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(flight_analytics
       DIMENSIONS carrier
       METRICS total_flights
   ) ORDER BY carrier;

Querying ``total_flights`` by ``city`` works too: each component is grouped by
``city`` through its own path, and the derived metric is computed from the
merged components. Where a city is reached by only one path, the other
component is ``NULL`` and so is the sum.

.. versionchanged:: 0.18.0

   A derived metric over different ``USING`` paths used to be refused with a
   role-played dimension.


.. _howto-rp-facts:

Aggregating a Fact on the Role-Playing Table
============================================

.. versionchanged:: 0.14.0

   A ``USING`` metric can now aggregate a fact declared on the role-playing
   table. Such a metric used to fail with ``Referenced table "a" not found``.

A metric can aggregate a fact declared on the role-playing table itself. Its
``USING`` picks which instance of the table the fact is read from, so the same
fact gives a different number per role. Recreate the two tables with an
elevation and some flights:

.. code-block:: sql

   CREATE OR REPLACE TABLE airports (
       airport_code VARCHAR, city VARCHAR, country VARCHAR, elevation INTEGER
   );
   INSERT INTO airports VALUES
       ('DEN', 'Denver',        'US', 5434),
       ('SFO', 'San Francisco', 'US',   13),
       ('LHR', 'London',        'UK',   83);

   CREATE OR REPLACE TABLE flights (
       flight_id INTEGER,
       departure_code VARCHAR,
       arrival_code VARCHAR,
       carrier VARCHAR
   );
   INSERT INTO flights VALUES
       (1, 'SFO', 'DEN', 'UA'),
       (2, 'DEN', 'LHR', 'UA'),
       (3, 'LHR', 'SFO', 'BA');

   CREATE SEMANTIC VIEW flight_elevation AS
   TABLES (
       f AS flights  PRIMARY KEY (flight_id),
       a AS airports PRIMARY KEY (airport_code)
   )
   RELATIONSHIPS (
       dep_airport AS f(departure_code) REFERENCES a,
       arr_airport AS f(arrival_code)   REFERENCES a
   )
   FACTS (
       a.elev AS a.elevation
   )
   DIMENSIONS (
       f.carrier AS f.carrier
   )
   METRICS (
       f.dep_elev USING (dep_airport) AS SUM(a.elev),
       f.arr_elev USING (arr_airport) AS SUM(a.elev),
       elev_climb AS arr_elev - dep_elev
   );

``dep_elev`` sums the departure airports' elevations and ``arr_elev`` the
arrival airports'. ``elev_climb`` composes both roles and keeps them apart:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(flight_elevation
       DIMENSIONS carrier
       METRICS dep_elev, arr_elev, elev_climb
   ) ORDER BY carrier;

.. code-block:: text

   ┌─────────┬──────────┬──────────┬────────────┐
   │ carrier │ dep_elev │ arr_elev │ elev_climb │
   ├─────────┼──────────┼──────────┼────────────┤
   │ BA      │       83 │       13 │        -70 │
   │ UA      │     5447 │     5517 │         70 │
   └─────────┴──────────┴──────────┴────────────┘

The metric's ``USING`` must name exactly one relationship to the table the fact
lives on (spelling that relationship twice, in any case, still names one). The
role carries through to a window metric over such a metric
(``f.dep_elev_by_flight AS SUM(dep_elev) OVER (PARTITION BY flight_id)``) and
to a semi-additive one.

A member that reaches such a fact with no single role to pick is rejected as
ambiguous at query time (see :ref:`howto-rp-errors`). That covers a metric
without ``USING`` (including one used as a window metric's inner aggregate), a
dimension, and a fact on a table reached only *through* the role-playing table.
Earlier versions answered these with the first-declared relationship's numbers.

Declare such metrics on the *referencing* table, as above. A metric declared on
the role-playing table itself (``a.total_elev AS SUM(a.elevation)``) cannot
name a role, because a ``USING`` relationship must start at the metric's own
table. It is rejected as ambiguous too.

Relationship names may be quoted identifiers, including names with spaces or
embedded quotes: declare ``"Dep Airport" AS f(departure_code) REFERENCES a`` and
pick it with ``USING ("Dep Airport")``. Names match the way DuckDB matches
identifiers, so ``USING (dep_airport)`` also finds a relationship declared as
``"Dep_Airport"``. Before v0.14.0 a quoted name on a role-playing relationship
made every query through it fail with a parser error.


.. _howto-rp-inspect:

Inspect the Scoped Aliases
==========================

Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to see how the extension creates scoped aliases for role-playing joins:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('flight_analytics',
       dimensions := ['city'],
       metrics := ['departure_count']
   );

The expanded SQL shows the airports table joined with a scoped alias (e.g., ``a__dep_airport``) that reflects which relationship path was used.


.. _howto-rp-diamond:

Role-Playing vs an Ambiguous Diamond
====================================

.. versionchanged:: 0.11.0

   A table reached from two *different* source tables -- a join "diamond" -- is
   rejected at ``CREATE`` time. Earlier versions accepted it and joined the
   shared table through whichever relationship was declared first, which gave
   wrong numbers when the two paths lead to different rows.

Role-playing and a diamond both reach one table by two routes, but only one of
them tells the extension which route a query means:

- **Role-playing (supported):** several named relationships from **one** source
  table to one target, like ``dep_airport`` and ``arr_airport`` from
  ``flights`` to ``airports`` above. ``USING`` on a metric picks the route.
- **Diamond (rejected):** one target reached from two **different** source
  tables. In the view below, ``regions`` is reached from both ``customers`` and
  ``stores``, so a ``region`` dimension could mean the customer's region or the
  store's:

.. code-block:: sql

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       o AS orders    PRIMARY KEY (order_id),
       c AS customers PRIMARY KEY (customer_id),
       s AS stores    PRIMARY KEY (store_id),
       r AS regions   PRIMARY KEY (region_id)
   )
   RELATIONSHIPS (
       order_customer  AS o(customer_id) REFERENCES c,
       order_store     AS o(store_id)    REFERENCES s,
       customer_region AS c(region_id)   REFERENCES r,
       store_region    AS s(region_id)   REFERENCES r
   )
   DIMENSIONS (
       r.region AS r.name
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

.. code-block:: text

   diamond: 'r' is reachable from multiple tables ('c', 's'); the join path is
   ambiguous. Declare the target under a second table alias so it is joined once
   per path.

Fix it as the message says: declare ``regions`` once per path, under two
aliases, and give each alias its own dimension. Each route then has its own
table and there is nothing left to choose between:

.. code-block:: sql
   :emphasize-lines: 6,7,12,13,16,17

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       o  AS orders    PRIMARY KEY (order_id),
       c  AS customers PRIMARY KEY (customer_id),
       s  AS stores    PRIMARY KEY (store_id),
       cr AS regions   PRIMARY KEY (region_id),
       sr AS regions   PRIMARY KEY (region_id)
   )
   RELATIONSHIPS (
       order_customer  AS o(customer_id) REFERENCES c,
       order_store     AS o(store_id)    REFERENCES s,
       customer_region AS c(region_id)   REFERENCES cr,
       store_region    AS s(region_id)   REFERENCES sr
   )
   DIMENSIONS (
       cr.customer_region AS cr.name,
       sr.store_region    AS sr.name
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

A query can now group by ``customer_region``, ``store_region`` or both.

Two relationships from one source to two *different* targets, like
``orders → customers`` and ``orders → products`` in :ref:`tutorial-multi-table`,
are neither pattern and need nothing extra.


.. _howto-rp-errors:

Troubleshooting
===============

**"dimension is ambiguous" error**
   This occurs when a dimension comes from a role-playing table and some metric in the
   query (or the query itself, with no metrics) names no ``USING`` path to it. Solutions:

   - Give every queried metric a ``USING`` that targets the desired relationship, for
     example by writing ``f.departures USING (dep_airport) AS f.flight_count`` over the
     plain metric.
   - Use a dimension from a non-ambiguous table (like the base table).

**Multiple USING paths for the same table**
   .. versionchanged:: 0.18.0

   Co-queried metrics with different ``USING`` paths to the dimension's table are each
   grouped through their own path and merged (see :ref:`howto-rp-query`). This used to
   raise an ambiguity error.

**Dimension on a descendant of a role-playing table**
   .. versionchanged:: 0.18.0

   A dimension on a table reached *through* a role-playing table -- for
   example ``region_name`` on ``regions`` where ``airports`` references
   ``regions`` -- follows the path each metric's ``USING`` names, as in
   Snowflake: ``departure_count`` by ``region_name`` groups by the departure
   airport's region, and ``departure_count, arrival_count`` by ``region_name``
   merges the two per region. A metric with no ``USING`` is refused as
   ambiguous, as it is for a dimension directly on ``airports``. Since 0.11.0
   and until 0.18.0 every such query was refused; before 0.11.0 it silently
   used the first-declared relationship.

**Facts on a role-playing table**
   .. versionchanged:: 0.11.0

   A fact sourced on a role-playing table (or a descendant of one) is now
   rejected. Facts carry no ``USING`` context, so the role was always
   unresolvable -- previously such a fact silently read the first-declared
   relationship's rows.

**A member references a fact on a role-playing table**
   .. versionchanged:: 0.14.0

   A metric or dimension whose expression reaches a fact on a role-playing table
   (or on a table reached only through one) is rejected as ambiguous unless it
   is a metric whose ``USING`` names exactly one relationship to the fact's own
   table. Previously such a member silently read the first-declared
   relationship's rows. Add ``USING`` to the metric, or declare the fact on a
   non-role-playing table. See :ref:`howto-rp-facts`.

**A metric is declared on a role-playing table**
   .. versionchanged:: 0.14.0

   A metric whose own table is a role-playing table (or is reached only through
   one) is rejected as ambiguous: ``USING`` cannot be attached to it, since a
   ``USING`` relationship must start at the metric's own table. Previously, when
   no fan trap stopped it (one-to-one relationships), it silently read the
   first-declared relationship's rows. Declare the metric on the referencing
   table with ``USING`` instead. See :ref:`howto-rp-facts`.

   The one exception is the base table, the first table in ``TABLES``. A metric
   declared on it is accepted when it is queried without dimensions, alongside
   only other base-table metrics, and with a filter (if any) that names only
   base-table members. Nothing is joined then, so there is no relationship to
   choose. Grouping it by a dimension, even one on the base table, or giving it
   ``NON ADDITIVE BY`` is still rejected.

**Semi-additive metric snapshotting on a role-playing dimension**
   .. versionchanged:: 0.11.0

   A metric written ``m USING (<relationship>) NON ADDITIVE BY (<dim on the
   role-played table>)`` now selects the snapshot for the role its ``USING``
   names, instead of an arbitrary one. A semi-additive metric that snapshots on
   a role-playing dimension but has no ``USING`` to disambiguate the role is
   now rejected as ambiguous at query time -- the same error a directly-queried
   role-playing dimension raises. See :ref:`howto-semi-additive`.


.. _howto-rp-related:

Related
=======

- :ref:`ref-create-relationships` -- ``RELATIONSHIPS`` syntax and validation rules
- :ref:`ref-create-metrics` -- ``USING`` in the metric grammar
- :ref:`howto-fan-traps` -- What a role-played dimension does to multi-grain queries
- :ref:`howto-derived-metrics` -- Composing metrics that carry ``USING`` context
