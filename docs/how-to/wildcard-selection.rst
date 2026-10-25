.. meta::
   :description: Select all dimensions, metrics, or facts for a table alias using wildcard patterns (alias.*) in SEMANTIC_VIEW(...) and semantic_view() queries

.. _howto-wildcard-selection:

================================
How to Use Wildcard Selection
================================

This guide shows how to use the ``alias.*`` wildcard pattern to select all dimensions, metrics, or facts belonging to a specific table alias in a single expression, instead of listing each name individually.

**Prerequisites:**

- A working semantic view with multiple dimensions and/or metrics (see :ref:`tutorial-multi-table`)
- Familiarity with :ref:`SEMANTIC_VIEW(...) <ref-semantic-view-function>` queries


.. _howto-wildcard-syntax:

Wildcard Syntax
===============

Use ``table_alias.*`` in ``DIMENSIONS``, ``METRICS`` or ``FACTS`` (or in the ``dimensions``, ``metrics`` and ``facts`` lists of ``semantic_view()``, written ``'o.*'``) to expand to all items scoped to that table alias:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       DIMENSIONS o.*
       METRICS o.*
   );

This expands ``o.*`` to all dimensions scoped to table alias ``o`` and all metrics scoped to ``o``.

Items declared without a table alias, such as derived metrics, belong to the view's **first** declared table. ``alias.*`` for that table's alias includes them; a wildcard for any other alias does not.


.. _howto-wildcard-private:

PRIVATE Item Exclusion
======================

Wildcard expansion excludes ``PRIVATE`` metrics and facts. Only ``PUBLIC`` items (the default) are included in the expanded list.

Given a view with public, private and derived metrics:

.. code-block:: sql

   CREATE TABLE orders (
       id INTEGER, region VARCHAR, status VARCHAR,
       amount DECIMAL(10,2), cost DECIMAL(10,2)
   );
   INSERT INTO orders VALUES
       (1, 'East', 'open',   100.00, 40.00),
       (2, 'West', 'closed',  50.00, 30.00);

   CREATE SEMANTIC VIEW sales AS
   TABLES (o AS orders PRIMARY KEY (id))
   DIMENSIONS (o.region AS o.region)
   METRICS (
       o.revenue AS SUM(o.amount),
       PRIVATE o.internal_cost AS SUM(o.cost),
       profit AS revenue - internal_cost
   );

.. code-block:: sql

   -- o.* expands to ['revenue', 'profit']: internal_cost is PRIVATE,
   -- and profit, a derived metric, belongs to o, the first declared table
   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS region
       METRICS o.*
   ) ORDER BY region;

.. code-block:: text

   ┌────────┬─────────┬────────┐
   │ region │ revenue │ profit │
   ├────────┼─────────┼────────┤
   │ East   │  100.00 │  60.00 │
   │ West   │   50.00 │  20.00 │
   └────────┴─────────┴────────┘

The private ``internal_cost`` is left out of the result, but ``profit`` can still use it.


.. _howto-wildcard-dedup:

Deduplication
=============

When an item appears both explicitly and via a wildcard, it appears only once in the expanded list. With a ``status`` dimension added to the ``sales`` view:

.. code-block:: sql

   CREATE OR REPLACE SEMANTIC VIEW sales AS
   TABLES (o AS orders PRIMARY KEY (id))
   DIMENSIONS (
       o.region AS o.region,
       o.status AS o.status
   )
   METRICS (
       o.revenue AS SUM(o.amount),
       PRIVATE o.internal_cost AS SUM(o.cost),
       profit AS revenue - internal_cost
   );

   -- 'region' is listed explicitly AND is part of o.*
   -- Result: ['region', 'status'] (region appears once)
   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS region, o.*
   );


.. _howto-wildcard-bare:

Bare Wildcard Rejection
========================

.. warning::

   Unqualified ``*`` (bare wildcard) is not supported. All wildcards must be qualified with a table alias.

.. code-block:: sql

   -- This fails:
   SELECT * FROM SEMANTIC_VIEW(sales
       DIMENSIONS *
   );

.. code-block:: text

   semantic view 'sales': unqualified wildcard '*' is not supported. Use table_alias.*
   to select all items for a specific table.


.. _howto-wildcard-facts:

Wildcards for Facts
===================

``FACTS`` also supports wildcard expansion:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       FACTS li.*
   );

This expands to all public facts scoped to table alias ``li``.


.. _howto-wildcard-troubleshoot:

Troubleshooting
===============

**Unknown table alias in wildcard**
   The table alias must match an alias declared in the ``TABLES`` clause. The error lists available aliases: ``unknown table alias 'x' in wildcard 'x.*'. Available aliases: [o, c, li]``.

**Empty expansion**
   A wildcard for an alias with no public items of that kind expands to nothing. If every list in the query ends up empty, the query fails with ``specify at least one of DIMENSIONS, METRICS or FACTS``. If another list still has items, the query runs without the empty one: ``DIMENSIONS region METRICS c.*`` on a view with no metrics on ``c`` returns the distinct ``region`` values only. Check the alias's members with :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>`.

**A derived metric is missing from, or included in, an alias wildcard**
   Derived metrics have no table alias, so only the first declared table's ``alias.*`` includes them. List a derived metric by name to query it next to another alias's wildcard.


.. _howto-wildcard-related:

Related
=======

- :ref:`ref-sv-wildcard` -- Wildcard rules in the query reference
- :ref:`howto-annotations-access` -- Mark metrics and facts ``PRIVATE``
- :ref:`howto-query-facts` -- Query facts, including with ``alias.*``
- :ref:`ref-show-columns` -- List every public member of a view
