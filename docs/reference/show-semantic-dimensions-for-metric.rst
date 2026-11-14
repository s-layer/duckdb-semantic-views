.. meta::
   :description: Syntax reference for SHOW SEMANTIC DIMENSIONS IN ... FOR METRIC, which returns the dimensions a given metric can be grouped by

.. _ref-show-dims-for-metric:

=========================================
SHOW SEMANTIC DIMENSIONS FOR METRIC
=========================================

Returns the dimensions a specific metric can be grouped by, as in Snowflake. In single-table views, all dimensions are returned. In multi-table views, the extension follows the relationship graph from the metric's source table. A dimension the metric's rows fan out to (one reached across a one-to-many relationship) is listed when a query grouping the metric by it is answered, counting each row of the metric's table once per group, and left out when that query is refused with a fan trap error (see :ref:`howto-fan-detect`).

For window function metrics, the ``required`` column indicates which dimensions must be included in the query because they appear in the metric's ``PARTITION BY EXCLUDING``, ``PARTITION BY``, or ``ORDER BY`` clauses.

For background on what fan traps are and how to resolve them, see :ref:`howto-fan-traps`.


.. _ref-show-dims-for-metric-syntax:

Syntax
======

.. code-block:: sqlgrammar

   SHOW SEMANTIC DIMENSIONS
       [ LIKE '<pattern>' ]
       IN <name>
       FOR METRIC <metric_name>
       [ STARTS WITH '<prefix>' ]
       [ LIMIT <rows> ]

The ``IN`` and ``FOR METRIC`` clauses are required. The remaining clauses are optional. When multiple clauses appear, they must follow the order shown above.


.. _ref-show-dims-for-metric-params:

Parameters
==========

``<name>``
   The name of the semantic view. Returns an error if the view does not exist.

``<metric_name>``
   The name of a metric defined in the semantic view. Returns an error if the metric does not exist. Matching is case-insensitive, and quoting does not affect it: ``revenue``, ``REVENUE`` and ``"Revenue"`` all resolve to the same metric. Write the name quoted when it contains whitespace or other characters that would otherwise end it -- ``FOR METRIC "total amount"``.

.. tip::

   Both the view name and metric name support fuzzy matching in error messages. If a name is close to an existing name, the error suggests the closest match: ``metric 'order_totl' not found in semantic view 'star_sv'. Did you mean 'order_total'?``


.. _ref-show-dims-for-metric-filtering-clauses:

Optional Filtering Clauses
===========================

``LIKE '<pattern>'``
   Filters the returned dimensions to those whose name matches the pattern. Uses SQL ``LIKE`` pattern syntax: ``%`` matches any sequence of characters, ``_`` matches a single character. Matching is **case-insensitive** (the extension maps ``LIKE`` to DuckDB's ``ILIKE``). The pattern must be enclosed in single quotes. ``LIKE`` must appear before ``IN``.

``STARTS WITH '<prefix>'``
   Filters the returned dimensions to those whose name begins with the prefix. Matching is **case-sensitive**. The prefix must be enclosed in single quotes. ``STARTS WITH`` must appear after ``FOR METRIC``.

``LIMIT <rows>``
   Restricts the output to the first *rows* results. Must be a non-negative integer; ``LIMIT 0`` is accepted and returns no rows. ``LIMIT`` must appear last.

When ``LIKE`` and ``STARTS WITH`` are both present, a dimension must satisfy both conditions (they are combined with ``AND``). These filtering clauses are applied after the metric's dimensions are worked out, so only dimensions the metric can be grouped by are candidates for name matching.

.. warning::

   Clause order is enforced. The full order is: ``LIKE``, ``IN``, ``FOR METRIC``, ``STARTS WITH``, ``LIMIT``. Placing clauses out of order produces a syntax error.


.. _ref-show-dims-for-metric-output:

Output Columns
==============

Returns one row per reachable dimension with 6 columns:

.. list-table::
   :header-rows: 1
   :widths: 18 12 70

   * - Column
     - Type
     - Description
   * - ``table_name``
     - VARCHAR
     - The physical table name the dimension is scoped to.
   * - ``name``
     - VARCHAR
     - The dimension name.
   * - ``data_type``
     - VARCHAR
     - The type the member's column has in a :ref:`semantic-view query <ref-semantic-view-function>`, in DuckDB's spelling (``VARCHAR``, ``BIGINT``, ``DECIMAL(38,2)``). Determined when the command runs, so it follows the current source columns. Empty when the member's source no longer binds (for example, its table was dropped), or when the member cannot be queried on its own because of a fan trap. See :ref:`Reported Data Types <explanation-sf-data-types>`.
   * - ``required``
     - BOOLEAN
     - ``TRUE`` if the dimension is referenced in a window metric's ``PARTITION BY EXCLUDING``, ``PARTITION BY``, or ``ORDER BY`` clause (meaning it must be included in the query for that metric). ``FALSE`` for all other dimensions and for non-window metrics.
   * - ``synonyms``
     - VARCHAR
     - JSON array of the dimension's synonyms (e.g., ``["territory"]``). ``[]`` when no synonyms are set.
   * - ``comment``
     - VARCHAR
     - The dimension comment text. Empty string if no comment is set.


.. versionchanged:: 0.14.0

   ``data_type`` is populated again. It had been empty for every view created
   since v0.10.0. The type is now worked out when the command runs, so views
   created in that window report it too, with no need to re-create them.

.. versionchanged:: 0.18.0

   Added the ``synonyms`` and ``comment`` columns, after ``required``, as in Snowflake's output.


.. _ref-show-dims-for-metric-filtering:

Which Dimensions Are Listed
===========================

A dimension is listed when the metric's source table reaches the dimension's table along the relationships, in steps the query can answer. The rules are:

- **Same table:** If the dimension and metric share the same source table, the dimension is always included.
- **Many-to-one (forward):** Following a foreign key to the referenced table is always allowed. Each row on the FK side maps to at most one row on the referenced side. This includes an ``ASOF`` or range relationship followed forward.
- **One-to-one:** If the FK columns match a ``PRIMARY KEY`` or ``UNIQUE`` constraint on the FK table, the relationship is one-to-one, and both directions are allowed.
- **One-to-many (reverse):** Stepping from the referenced table back to the FK table fans the metric's rows out. A query by such a dimension counts each row of the metric's table once per group, so the dimension is listed when the metric is a plain aggregate on a table with a ``PRIMARY KEY``. It is left out for a metric on a table with no ``PRIMARY KEY``, which the query refuses across a fan-out. A semi-additive metric (``NON ADDITIVE BY``) is counted once per group only when grouped by its ``NON ADDITIVE BY`` dimension, so that is the one fanned dimension it lists. When that dimension itself is reached only across a fan-out, a query by any other dimension is refused, the metric's own table's included, and it lists only that dimension. A step back across an ``ASOF`` or range relationship is never taken: a period metric grouped by an event dimension is refused, as in Snowflake.
- **Derived metrics:** For derived metrics (those without a table alias), the extension resolves all base metrics they depend on and uses the union of their source tables. A dimension is included if it is reached from at least one of those source tables. It is listed across a fan-out only when every base metric could be.
- **Window metrics:** A window metric lists the dimensions its inner metric lists, since the query computes the window over the inner metric's result. The dimensions its ``PARTITION BY EXCLUDING``, ``PARTITION BY``, or ``ORDER BY`` clauses name are marked ``required = TRUE``. When the inner metric cannot be grouped by one of them, no query of the window answers, and nothing is listed.
- **Several paths:** A dimension reached along two paths (a diamond, or a table referenced twice) is listed. A query by it needs a metric whose ``USING`` picks one path, and is refused without one. Snowflake lists these too, including dimensions past a role-played table that a query of the metric refuses whatever it names: a gate's terminal for a flights metric, or a flight's carrier for an airport count. A dimension reached by no chain of foreign keys and two ways is left out, as the query refuses it.

.. versionchanged:: 0.19.0

   Dimensions reached across a one-to-many relationship are listed when the query answers them, as Snowflake lists them. Earlier releases listed only the dimensions a metric reaches without a fan-out. A window metric now lists what its inner metric lists.

.. warning::

   For a derived metric whose components sit on **different** tables, one of which refuses a fan-out (a semi-additive metric, or one on a table with no ``PRIMARY KEY``), this command is more permissive than the query. The "at least one source table" rule lists a dimension one component reaches even when it is below the grain of the other, and :ref:`semantic_view() <ref-semantic-view-function>` then refuses that combination. For example, with ``items_per_order AS item_count / latest_orders`` (``item_count`` on ``line_items``, the semi-additive ``latest_orders`` on ``orders``), a ``line_items`` dimension is listed for ``items_per_order``, but the query refuses it. For those metrics, check each component metric separately. With plain components, as in ``item_count / order_count``, the query answers every listed dimension. See :ref:`explanation-grain-refused` for the shapes the query refuses.


.. _ref-show-dims-for-metric-examples:

Examples
========

**Single-table view (all dimensions reachable):**

.. code-block:: sql

   CREATE SEMANTIC VIEW simple_sales AS
   TABLES (
       s AS sales PRIMARY KEY (id)
   )
   DIMENSIONS (
       s.product AS s.product,
       s.region  AS s.region
   )
   METRICS (
       s.total AS SUM(s.amount)
   );

   SHOW SEMANTIC DIMENSIONS IN simple_sales FOR METRIC total;

.. code-block:: text

   ┌────────────┬─────────┬───────────┬──────────┬──────────┬─────────┐
   │ table_name │ name    │ data_type │ required │ synonyms │ comment │
   ├────────────┼─────────┼───────────┼──────────┼──────────┼─────────┤
   │ sales      │ product │ VARCHAR   │ false    │ []       │         │
   │ sales      │ region  │ VARCHAR   │ false    │ []       │         │
   └────────────┴─────────┴───────────┴──────────┴──────────┴─────────┘

With a single table, there are no joins, so every dimension is listed for every metric.

The example tables use ``VARCHAR`` text columns, ``DATE`` dates, ``INTEGER`` quantities and ``DECIMAL(10,2)`` amounts and prices. ``data_type`` is each member's type as a query returns it -- an aggregate's own type, not its input's: ``COUNT`` is ``BIGINT``, ``SUM`` of a ``DECIMAL(10,2)`` is ``DECIMAL(38,2)``, and a ratio of two metrics is ``DOUBLE``. See :ref:`Reported Data Types <explanation-sf-data-types>`.

**Multi-table view with a fan-out:**

Consider a three-table schema: ``customers <- orders <- line_items``, where orders has a many-to-one FK to customers, and line_items has a many-to-one FK to orders.

.. code-block:: sql

   CREATE SEMANTIC VIEW star_sv AS
   TABLES (
       c  AS customers  PRIMARY KEY (id),
       o  AS orders     PRIMARY KEY (id),
       li AS line_items PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       item_order     AS li(order_id)   REFERENCES o
   )
   DIMENSIONS (
       c.customer_name    AS c.name,
       c.customer_country AS c.country,
       li.item_qty        AS li.qty
   )
   METRICS (
       o.order_total    AS SUM(o.total),
       li.line_item_sum AS SUM(li.price * li.qty)
   );

For ``order_total`` (source table: ``orders``), the path to ``customers`` is many-to-one, and the path to ``line_items`` is one-to-many (a fan-out). ``orders`` has a ``PRIMARY KEY``, so a query by ``item_qty`` counts each order once per quantity, and all three dimensions are listed:

.. code-block:: sql

   SHOW SEMANTIC DIMENSIONS IN star_sv FOR METRIC order_total;

.. code-block:: text

   ┌────────────┬──────────────────┬───────────┬──────────┬──────────┬─────────┐
   │ table_name │ name             │ data_type │ required │ synonyms │ comment │
   ├────────────┼──────────────────┼───────────┼──────────┼──────────┼─────────┤
   │ customers  │ customer_country │ VARCHAR   │ false    │ []       │         │
   │ customers  │ customer_name    │ VARCHAR   │ false    │ []       │         │
   │ line_items │ item_qty         │ INTEGER   │ false    │ []       │         │
   └────────────┴──────────────────┴───────────┴──────────┴──────────┴─────────┘

A window metric on ``orders``, a semi-additive one whose ``NON ADDITIVE BY`` dimension is not ``item_qty``, or a metric on an ``orders`` table declared with no ``PRIMARY KEY`` would not list ``item_qty``.

For ``line_item_sum`` (source table: ``line_items``), the path from ``line_items`` to ``orders`` to ``customers`` is all many-to-one, and ``item_qty`` is on the same table. All three dimensions are included:

.. code-block:: sql

   SHOW SEMANTIC DIMENSIONS IN star_sv FOR METRIC line_item_sum;

.. code-block:: text

   ┌────────────┬──────────────────┬───────────┬──────────┬──────────┬─────────┐
   │ table_name │ name             │ data_type │ required │ synonyms │ comment │
   ├────────────┼──────────────────┼───────────┼──────────┼──────────┼─────────┤
   │ customers  │ customer_country │ VARCHAR   │ false    │ []       │         │
   │ customers  │ customer_name    │ VARCHAR   │ false    │ []       │         │
   │ line_items │ item_qty         │ INTEGER   │ false    │ []       │         │
   └────────────┴──────────────────┴───────────┴──────────┴──────────┴─────────┘

**Window metric with required dimensions:**

.. versionadded:: 0.6.0

When querying dimensions for a window function metric, the ``required`` column is ``TRUE`` for any dimension referenced in the metric's ``PARTITION BY EXCLUDING``, ``PARTITION BY``, or ``ORDER BY`` clauses. These dimensions must be included in the query for the window metric to function correctly. This holds however the window spells the dimension: bare (``ORDER BY placed``), qualified with its table alias (``ORDER BY s.placed``), or quoted in a different case (``ORDER BY "PLACED"``).

.. versionchanged:: 0.14.0

   A dimension spelled qualified or quoted in the window used to be reported ``required = false``.

.. code-block:: sql

   CREATE SEMANTIC VIEW monthly_sv AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   DIMENSIONS (
       o.region AS o.region,
       o.month  AS o.month,
       o.status AS o.status
   )
   METRICS (
       o.total_qty AS SUM(o.qty),
       o.avg_qty   AS AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY month)
   );

   SHOW SEMANTIC DIMENSIONS IN monthly_sv FOR METRIC avg_qty;

.. code-block:: text

   ┌────────────┬────────┬───────────┬──────────┬──────────┬─────────┐
   │ table_name │ name   │ data_type │ required │ synonyms │ comment │
   ├────────────┼────────┼───────────┼──────────┼──────────┼─────────┤
   │ orders     │ month  │ DATE      │ true     │ []       │         │
   │ orders     │ region │ VARCHAR   │ true     │ []       │         │
   │ orders     │ status │ VARCHAR   │ false    │ []       │         │
   └────────────┴────────┴───────────┴──────────┴──────────┴─────────┘

In this example, ``region`` is required because it appears in ``EXCLUDING`` (it defines the partition boundary), and ``month`` is required because it appears in ``ORDER BY`` (it defines the window ordering). The ``status`` dimension is optional -- it can be included in the query but is not referenced by the window specification.

.. tip::

   Use the ``required`` column to build valid queries for window metrics. Omitting a required dimension produces a query-time error: ``semantic view 'monthly_sv': window function metric 'avg_qty' requires dimension 'month' to be included in the query (used in ORDER BY)``.

**Filter the dimensions with LIKE, STARTS WITH, and LIMIT:**

The filtering examples below use a two-table view in which every dimension is listed for ``total_amount``:

.. code-block:: sql

   CREATE SEMANTIC VIEW filter_sv AS
   TABLES (
       c AS customers PRIMARY KEY (id),
       o AS orders    PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (
       c.customer_name AS c.name,
       c.region        AS c.region,
       o.order_date    AS o.order_date,
       o.status        AS o.status
   )
   METRICS (
       o.total_amount AS SUM(o.amount)
   );

Narrow the results further by name pattern. ``LIKE`` is case-insensitive:

.. code-block:: sql

   SHOW SEMANTIC DIMENSIONS LIKE '%gio%' IN filter_sv FOR METRIC total_amount;

.. code-block:: text

   ┌────────────┬────────┬───────────┬──────────┬──────────┬─────────┐
   │ table_name │ name   │ data_type │ required │ synonyms │ comment │
   ├────────────┼────────┼───────────┼──────────┼──────────┼─────────┤
   │ customers  │ region │ VARCHAR   │ false    │ []       │         │
   └────────────┴────────┴───────────┴──────────┴──────────┴─────────┘

``STARTS WITH`` is case-sensitive:

.. code-block:: sql

   SHOW SEMANTIC DIMENSIONS IN filter_sv FOR METRIC total_amount STARTS WITH 'customer';

.. code-block:: text

   ┌────────────┬───────────────┬───────────┬──────────┬──────────┬─────────┐
   │ table_name │ name          │ data_type │ required │ synonyms │ comment │
   ├────────────┼───────────────┼───────────┼──────────┼──────────┼─────────┤
   │ customers  │ customer_name │ VARCHAR   │ false    │ []       │         │
   └────────────┴───────────────┴───────────┴──────────┴──────────┴─────────┘

``LIMIT`` applies after the rows are sorted by dimension name:

.. code-block:: sql

   SHOW SEMANTIC DIMENSIONS IN filter_sv FOR METRIC total_amount LIMIT 3;

.. code-block:: text

   ┌────────────┬───────────────┬───────────┬──────────┬──────────┬─────────┐
   │ table_name │ name          │ data_type │ required │ synonyms │ comment │
   ├────────────┼───────────────┼───────────┼──────────┼──────────┼─────────┤
   │ customers  │ customer_name │ VARCHAR   │ false    │ []       │         │
   │ orders     │ order_date    │ DATE      │ false    │ []       │         │
   │ customers  │ region        │ VARCHAR   │ false    │ []       │         │
   └────────────┴───────────────┴───────────┴──────────┴──────────┴─────────┘

**Derived metrics inherit source tables:**

.. code-block:: sql

   CREATE SEMANTIC VIEW derived_sv AS
   TABLES (
       c AS customers PRIMARY KEY (id),
       o AS orders    PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c
   )
   DIMENSIONS (
       c.customer_name AS c.name
   )
   METRICS (
       o.order_total AS SUM(o.total),
       double_total  AS order_total * 2
   );

   SHOW SEMANTIC DIMENSIONS IN derived_sv FOR METRIC double_total;

.. code-block:: text

   ┌────────────┬───────────────┬───────────┬──────────┬──────────┬─────────┐
   │ table_name │ name          │ data_type │ required │ synonyms │ comment │
   ├────────────┼───────────────┼───────────┼──────────┼──────────┼─────────┤
   │ customers  │ customer_name │ VARCHAR   │ false    │ []       │         │
   └────────────┴───────────────┴───────────┴──────────┴──────────┴─────────┘

The derived metric ``double_total`` depends on ``order_total`` (source: ``orders``). The extension traces this dependency and applies the same reachability rules as if querying ``order_total`` directly.

**Error: metric not found:**

.. code-block:: sql

   SHOW SEMANTIC DIMENSIONS IN star_sv FOR METRIC nonexistent;

.. code-block:: text

   Error: metric 'nonexistent' not found in semantic view 'star_sv'

**Error: view does not exist:**

.. code-block:: sql

   SHOW SEMANTIC DIMENSIONS IN nonexistent_sv FOR METRIC total;

.. code-block:: text

   Error: semantic view 'nonexistent_sv' does not exist
