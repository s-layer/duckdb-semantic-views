.. meta::
   :description: Define named row-level expressions in the FACTS clause that metrics can reference, query facts directly, and annotate facts with metadata

.. _howto-facts:

==============================================
How to Use FACTS for Reusable Row-Level Logic
==============================================

This guide shows how to use the ``FACTS`` clause to define reusable row-level expressions that metrics can reference. FACTS eliminate duplicated calculations across metrics and support chaining (one fact referencing another). Facts can also be queried directly as row-level columns and annotated with comments, synonyms, access modifiers, and the named-filter label.

**Prerequisites:**

- A working semantic view with ``TABLES``, ``DIMENSIONS``, and ``METRICS`` (see :ref:`tutorial-multi-table`, or :ref:`tutorial-building-model` for a guided introduction to facts)
- Understanding of aggregate vs. row-level expressions in SQL


.. _howto-facts-basic:

Define a Basic Fact
===================

A fact is a named row-level expression scoped to a table alias. Unlike metrics, facts do not contain aggregate functions. They compute a value for each row.

.. code-block:: sql
   :emphasize-lines: 8

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       li AS line_items PRIMARY KEY (id)
   )
   FACTS (
       li.net_price AS li.extended_price * (1 - li.discount)
   )
   DIMENSIONS (
       li.region AS li.region
   )
   METRICS (
       li.total_net AS SUM(li.net_price)
   );

The metric ``total_net`` references the fact ``net_price``. At expansion time, the extension inlines the fact expression into the metric: ``SUM(li.extended_price * (1 - li.discount))``.


.. _howto-facts-chain:

Chain Facts Together
====================

Facts can reference other facts. The extension resolves them in dependency order (topological sort) and inlines them recursively.

.. code-block:: sql
   :emphasize-lines: 6,7

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       li AS line_items PRIMARY KEY (id)
   )
   FACTS (
       li.net_price  AS li.extended_price * (1 - li.discount),
       li.tax_amount AS li.net_price * li.tax_rate
   )
   DIMENSIONS (
       li.region AS li.region
   )
   METRICS (
       li.total_net AS SUM(li.net_price),
       li.total_tax AS SUM(li.tax_amount)
   );

Here ``tax_amount`` references ``net_price``. The extension resolves the chain:

1. ``net_price`` = ``li.extended_price * (1 - li.discount)``
2. ``tax_amount`` = ``(li.extended_price * (1 - li.discount)) * li.tax_rate``

Both metrics receive the fully inlined expressions.


.. _howto-facts-multi-table:

Use Facts in Multi-Table Views
==============================

Facts are scoped to their table alias. In a multi-table view, each fact references columns from its own table.

.. code-block:: sql

   CREATE SEMANTIC VIEW analytics AS
   TABLES (
       li AS line_items PRIMARY KEY (id),
       o  AS orders      PRIMARY KEY (id),
       c  AS customers   PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       li_to_order       AS li(order_id)    REFERENCES o,
       order_to_customer AS o(customer_id)  REFERENCES c
   )
   FACTS (
       li.net_price  AS li.extended_price * (1 - li.discount),
       li.tax_amount AS li.net_price * li.tax_rate
   )
   DIMENSIONS (
       o.region  AS o.region,
       c.country AS c.country
   )
   METRICS (
       li.total_net AS SUM(li.net_price),
       li.total_tax AS SUM(li.tax_amount)
   );

The facts are still scoped to ``li`` (line_items), but the dimensions come from ``o`` (orders) and ``c`` (customers). The extension joins all necessary tables based on what the query requests.

.. warning::

   **An expression may only reference columns of its own table.** A fact,
   dimension, or metric expression that names a raw column of *another* logical
   table -- ``li.margin AS li.extended_price - c.discount`` -- is rejected at
   ``CREATE``:

   .. code-block:: text

      semantic view: metric 'margin' references 'c.discount', a column of table
      'c', but a metric expression may only reference columns of its own table
      ('li'). To use a value from another table, define a FACT on that table and
      reference the fact by name (e.g. 'c.<fact_name>').

   Snowflake applies the `same rule
   <https://docs.snowflake.com/en/user-guide/views-semantic/validation-rules>`_
   ("Expressions cannot refer to base table columns from other tables") and
   rejects at definition time as well.

   .. versionchanged:: 0.12.0
      Previously the ``CREATE`` succeeded and the reference surfaced at query
      time as a DuckDB unknown-alias error.

   Referencing a *named fact* -- on its own table or, given a relationship, on
   another -- is fully supported for facts, dimensions and metrics alike; see
   :ref:`howto-facts-cross-table` below. Composing *metrics* across tables
   (``m AS t1.metric_a + t2.metric_b``) is likewise supported. It is only the
   raw foreign *column* that is rejected.


.. _howto-facts-cross-table:

Reference a Fact on Another Table
=================================

.. versionadded:: 0.12.0

A fact, dimension or metric expression may reference a **declared fact** -- on
its own table, or on another one given a relationship. This is Snowflake's
documented way to cross tables, and the only one: define the fact where its
columns live, then refer to it from the connected table.

.. versionchanged:: 0.12.0
   Dimension expressions reference facts on the same footing as metrics. Before,
   facts were inlined only into metric and fact expressions, so a fact named in
   a dimension was emitted verbatim and failed on the unknown column -- including
   when the fact was declared on the dimension's *own* table.

.. code-block:: sql

   CREATE SEMANTIC VIEW order_margin AS
     TABLES (
       o AS orders PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id)
     )
     RELATIONSHIPS (
       o_to_c AS o(customer_id) REFERENCES c(id)
     )
     FACTS (
       c.cust_discount AS c.discount
     )
     METRICS (
       o.net_total AS SUM(o.amount - c.cust_discount)
     );

The fact's expression is inlined at the reference site and its table is joined
for you, so ``net_total`` computes over ``orders LEFT JOIN customers``. The
reference must be to the fact **by name** -- bare (``cust_discount``) or
qualified by the fact's own table (``c.cust_discount``). A qualifier naming any
other relation is read as a raw column of that relation, which is the
unsupported form in the warning above.

.. warning::

   **The fact's table must not fan the member's.** Reaching a fact requires
   joining its table, and if that join multiplies the member's rows -- a fact
   on a *child* table, reached across a one-to-many edge -- the aggregate would
   count each row once per child. There is no correct number to return, so the
   query is rejected:

   .. code-block:: text

      semantic view 'v': fan trap detected -- 'bad_margin' (table 'o')
      references the fact 'item_cost' on table 'li', and relationship 'li_to_o'
      fans out on the way there, so joining it would multiply 'bad_margin's
      rows. Reference a fact on a table reachable without fanning out, or
      define the fact on 'o'.

   The same check applies when the metric is reached indirectly -- as the
   inner metric of a window metric or a component of a derived metric. The
   error then names the metric that references the fact.

   Facts on the *parent* side (many-to-one, one row per member row) join safely
   and are the usual case -- a customer's discount, a product's list price, a
   region's tax rate.


.. _howto-facts-query:

Query Facts Directly
====================

.. versionadded:: 0.6.0

Facts can be queried as row-level columns with the ``FACTS`` clause of :ref:`SEMANTIC_VIEW(...) <ref-sv-construct>`. Unlike metric queries, fact queries return individual rows without aggregation.

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       FACTS net_price, tax_amount
   );

Each row in the result contains the computed fact values. A dimension listed in ``FACTS`` is read row by row alongside them, without a ``GROUP BY``:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       FACTS region, net_price
   );

.. warning::

   Facts and metrics cannot be combined in the same query. Use ``FACTS`` or ``METRICS``, not both. Attempting to mix them produces an error.

For a complete guide to fact queries, including wildcard selection and troubleshooting, see :ref:`howto-query-facts`.


.. _howto-facts-annotations:

Annotate Facts with Metadata
==============================

.. versionadded:: 0.6.0

Facts accept four annotations: ``COMMENT``, ``WITH SYNONYMS``, the ``PRIVATE`` / ``PUBLIC`` access modifiers, and ``LABELS = (FILTER)``. ``PRIVATE`` or ``PUBLIC`` goes **before** the fact's name, as in ``PRIVATE li.raw_margin AS ...`` below. ``COMMENT`` and ``WITH SYNONYMS`` follow the expression, in any order. ``LABELS = (FILTER)`` goes between the fact's name and ``AS``, as in Snowflake, or after the expression with the others.

.. code-block:: sql
   :emphasize-lines: 6,7

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       li AS line_items PRIMARY KEY (id)
   )
   FACTS (
       li.net_price  AS li.extended_price * (1 - li.discount) COMMENT = 'Price after discount',
       PRIVATE li.raw_margin AS li.price - li.cost WITH SYNONYMS = ('margin', 'gross_margin')
   )
   DIMENSIONS (
       li.region AS li.region
   )
   METRICS (
       li.total_net    AS SUM(li.net_price),
       li.total_margin AS SUM(li.raw_margin),
       profit_margin   AS total_margin / total_net * 100
   );

- ``COMMENT`` adds a human-readable description, visible in :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` and :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>` output.
- ``WITH SYNONYMS`` adds informational alternative names for discoverability.
- ``PRIVATE`` prevents a fact from being queried directly (in ``FACTS``), while still allowing it to be referenced in metric expressions -- here the base metric ``total_margin`` aggregates the private fact ``raw_margin``. Private facts are also excluded from wildcard expansion (``alias.*``).
- ``LABELS = (FILTER)`` marks a boolean fact as a reusable named filter for :ref:`where_clause <howto-filtering>` predicates. See :ref:`howto-annotations-filters`.

Note that ``profit_margin`` is a :ref:`derived metric <howto-derived-metrics>`: it has no table alias and combines two base metrics by name. A derived metric may not contain an aggregate of its own, so the aggregation over ``raw_margin`` has to live in the base metric ``li.total_margin`` rather than inline in ``profit_margin``.

For more on metadata annotations, see :ref:`howto-metadata-annotations`.


.. _howto-facts-verify:

Verify the Inlined SQL
======================

Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to confirm that fact expressions are inlined correctly:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('analytics',
       dimensions := ['region'],
       metrics := ['total_net']
   );

The expanded SQL shows the fully inlined expression in the SELECT clause, with no reference to the fact name.


.. _howto-facts-errors:

Troubleshooting
===============

**Circular fact references**
   Facts that reference each other in a cycle cause a define-time error. The extension
   detects cycles during :ref:`CREATE SEMANTIC VIEW <ref-create-semantic-view>` and reports which facts are involved.

**Aggregate functions in facts**
   Facts must be row-level expressions. Using an aggregate function like ``SUM()`` or
   ``COUNT()`` in a fact expression causes a define-time error. Aggregation belongs in
   the ``METRICS`` clause.

**Fact name not found**
   If a metric references a fact name that does not exist, the extension treats it as a
   regular column reference. If the column also does not exist, ``CREATE`` refuses the
   view with an error naming the metric, such as ``semantic view 'sales': metric
   'revenue' does not bind: Binder Error: Table "li" does not have a column named
   "net_totl"``. Double-check that fact names match exactly.

**Private fact cannot be queried**
   Facts marked ``PRIVATE`` cannot be queried in ``FACTS``. They return an
   error: ``fact '<name>' is private and cannot be queried directly``. Remove the
   ``PRIVATE`` keyword to make a fact queryable.

**fact '...' does not bind: Parser Error: syntax error at or near "AS"**
   ``PRIVATE`` or ``PUBLIC`` was written after the expression
   (``li.net_price AS li.extended_price PRIVATE``). There the keyword is taken as
   part of the expression, and ``CREATE`` refuses the fact when it binds it. Move
   the modifier before the name: ``PRIVATE li.net_price AS li.extended_price``.


.. _howto-facts-related:

Related
=======

- :ref:`ref-create-facts` -- Full ``FACTS`` clause syntax and validation rules
- :ref:`howto-query-facts` -- Query facts as row-level columns
- :ref:`howto-derived-metrics` -- Compose the metrics that aggregate your facts
- :ref:`howto-metadata-annotations` -- Comments, synonyms, access modifiers and named filters
