.. meta::
   :description: Model a three-table star schema with PK/FK relationships and learn how the extension joins only the tables each query needs

.. _tutorial-multi-table:

==========================
Multi-Table Semantic Views
==========================

In this tutorial, you will define a semantic view over three related tables, declare PK/FK relationships between them, and see how the extension joins only the tables needed for each query. By the end, you will understand how to model a star schema with semantic views and query across table boundaries.

**Time:** 10 minutes

**Prerequisites:**

- Completed the :ref:`tutorial-getting-started` tutorial
- Familiarity with star schema concepts (fact table, dimension tables, foreign keys)

.. _tutorial-mt-create-tables:

Create the Schema
=================

Create a simple e-commerce schema with three tables: ``orders`` (fact table), ``customers`` (dimension table), and ``products`` (dimension table).

.. code-block:: sql

   CREATE TABLE customers (id INTEGER, name VARCHAR, city VARCHAR);
   INSERT INTO customers VALUES
       (1, 'Alice', 'Portland'),
       (2, 'Bob',   'Seattle'),
       (3, 'Carol', 'Portland');

   CREATE TABLE products (id INTEGER, name VARCHAR, category VARCHAR);
   INSERT INTO products VALUES
       (10, 'Widget',  'Hardware'),
       (20, 'Gadget',  'Hardware'),
       (30, 'Service', 'Software');

   CREATE TABLE orders (
       id INTEGER, customer_id INTEGER, product_id INTEGER,
       amount DECIMAL(10,2), ordered_at DATE
   );
   INSERT INTO orders VALUES
       (1, 1, 10, 25.00,  '2024-01-15'),
       (2, 1, 20, 50.00,  '2024-01-20'),
       (3, 2, 10, 25.00,  '2024-02-10'),
       (4, 2, 30, 100.00, '2024-02-14'),
       (5, 3, 20, 50.00,  '2024-03-01');


.. _tutorial-mt-define:

Define the Semantic View
========================

Declare all three tables in the ``TABLES`` clause, each with an alias and primary key. Then declare how they connect in the ``RELATIONSHIPS`` clause.

.. code-block:: sql
   :emphasize-lines: 8,9

   CREATE SEMANTIC VIEW shop AS
   TABLES (
       o AS orders    PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id),
       p AS products  PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       order_product  AS o(product_id)  REFERENCES p
   )
   DIMENSIONS (
       c.customer AS c.name,
       c.city     AS c.city,
       p.product  AS p.name,
       p.category AS p.category,
       o.month    AS date_trunc('month', o.ordered_at)
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*)
   );

The ``RELATIONSHIPS`` clause tells the extension how tables connect:

- ``order_customer AS o(customer_id) REFERENCES c`` means "the ``customer_id`` column on orders (alias ``o``) is a foreign key to the primary key of customers (alias ``c``)."
- Each relationship gets a name (``order_customer``, ``order_product``) that identifies the join path.

Dimensions and metrics reference columns through their table alias: ``c.customer AS c.name`` creates a dimension called ``customer`` from the ``name`` column on the ``c`` (customers) table.

.. tip::

   The ``PRIMARY KEY`` declaration is used by the extension to synthesize JOIN ON clauses. It does not create a constraint in DuckDB. It is metadata for the semantic view only.


.. _tutorial-mt-query-single:

Query One Dimension Table
=========================

Request revenue by customer. The extension joins only the ``customers`` table; ``products`` is not needed and is not included.

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS customer
       METRICS revenue, order_count
   ) ORDER BY revenue DESC;

.. code-block:: text

   ┌──────────┬─────────┬─────────────┐
   │ customer │ revenue │ order_count │
   ├──────────┼─────────┼─────────────┤
   │ Bob      │  125.00 │           2 │
   │ Alice    │   75.00 │           2 │
   │ Carol    │   50.00 │           1 │
   └──────────┴─────────┴─────────────┘

Verify which tables were joined by inspecting the generated SQL:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('shop',
       dimensions := ['customer'],
       metrics := ['revenue']
   );

The expanded SQL includes a ``LEFT JOIN`` to ``customers`` but no join to ``products``.


.. _tutorial-mt-query-both:

Query Across Both Dimension Tables
===================================

Request dimensions from both ``customers`` and ``products``. The extension joins both tables through the ``orders`` fact table.

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS customer, product
       METRICS revenue
   ) ORDER BY customer, product;

.. code-block:: text

   ┌──────────┬─────────┬─────────┐
   │ customer │ product │ revenue │
   ├──────────┼─────────┼─────────┤
   │ Alice    │ Gadget  │   50.00 │
   │ Alice    │ Widget  │   25.00 │
   │ Bob      │ Service │  100.00 │
   │ Bob      │ Widget  │   25.00 │
   │ Carol    │ Gadget  │   50.00 │
   └──────────┴─────────┴─────────┘


.. _tutorial-mt-time-dim:

Use a Computed Dimension
========================

The ``month`` dimension uses ``date_trunc('month', o.ordered_at)`` as its expression. Dimensions can be any SQL expression, not just column references.

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS month
       METRICS revenue, order_count
   ) ORDER BY month;

.. code-block:: text

   ┌─────────────────────┬─────────┬─────────────┐
   │        month        │ revenue │ order_count │
   ├─────────────────────┼─────────┼─────────────┤
   │ 2024-01-01 00:00:00 │   75.00 │           2 │
   │ 2024-02-01 00:00:00 │  125.00 │           2 │
   │ 2024-03-01 00:00:00 │   50.00 │           1 │
   └─────────────────────┴─────────┴─────────────┘

The ``month`` column is a ``TIMESTAMP``: DuckDB's ``date_trunc('month', ...)`` returns a timestamp even when its input is a ``DATE``. The column type always follows the dimension's expression, so to get plain dates, write the dimension as ``CAST(date_trunc('month', o.ordered_at) AS DATE)``.


.. _tutorial-mt-describe:

Describe the View
=================

Use :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` to see the full definition:

.. code-block:: sql

   DESCRIBE SEMANTIC VIEW shop;

The output uses a property-per-row format. Each row describes one property of one object (table, relationship, dimension, metric) in the view.


.. _tutorial-mt-update:

Update the View
===============

To change a semantic view, use :ref:`CREATE OR REPLACE <ref-create-variants>`:

.. code-block:: sql

   CREATE OR REPLACE SEMANTIC VIEW shop AS
   TABLES (
       o AS orders    PRIMARY KEY (id),
       c AS customers PRIMARY KEY (id),
       p AS products  PRIMARY KEY (id)
   )
   RELATIONSHIPS (
       order_customer AS o(customer_id) REFERENCES c,
       order_product  AS o(product_id)  REFERENCES p
   )
   DIMENSIONS (
       c.customer AS c.name,
       c.city     AS c.city,
       p.product  AS p.name,
       p.category AS p.category,
       o.month    AS date_trunc('month', o.ordered_at),
       o.year     AS date_trunc('year', o.ordered_at)
   )
   METRICS (
       o.revenue     AS SUM(o.amount),
       o.order_count AS COUNT(*),
       o.avg_order   AS AVG(o.amount)
   );

The new ``year`` dimension and ``avg_order`` metric are immediately available for queries.

``CREATE`` and ``CREATE OR REPLACE`` check a definition before they store it: each dimension, fact and metric is bound against its tables. Try replacing the view with a misspelled column:

.. code-block:: sql

   CREATE OR REPLACE SEMANTIC VIEW shop AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   METRICS (
       o.revenue AS SUM(o.amout)
   );

The statement stops with an error that names the metric and quotes DuckDB's own message:

.. code-block:: text

   Binder Error: semantic view 'shop': metric 'revenue' does not bind: Binder Error: Table "o" does not have a column named "amout"

Nothing is replaced, so ``shop`` keeps the definition from the previous step, ``year`` and ``avg_order`` included.

.. tip::

   ``CREATE OR REPLACE`` replaces the full view definition. To rename a view without changing its definition, use :ref:`ALTER SEMANTIC VIEW … RENAME TO <ref-alter-semantic-view>` instead:

   .. code-block:: sql

      ALTER SEMANTIC VIEW shop RENAME TO shop_v2;


.. _tutorial-mt-cleanup:

Clean Up
========

.. code-block:: sql

   DROP SEMANTIC VIEW shop;


.. _tutorial-mt-summary:

What You Learned
================

You now know how to:

- Declare multiple tables with aliases and primary keys in the ``TABLES`` clause
- Define PK/FK relationships in the ``RELATIONSHIPS`` clause
- Query dimensions from different tables, with the extension joining only what is needed
- Use SQL expressions (like ``date_trunc()``) as dimension definitions
- Update a semantic view with ``CREATE OR REPLACE SEMANTIC VIEW``, which checks every member before it replaces the view

Next, learn how to build a complete model with facts and derived metrics in the :ref:`tutorial-building-model` tutorial. Or explore specific features in the how-to guides:

- :ref:`howto-facts` -- reusable row-level expressions
- :ref:`howto-derived-metrics` -- metric-on-metric composition
- :ref:`howto-role-playing` -- same table joined via multiple relationships

Every metric in ``shop`` is declared on ``o`` (orders), so every query in this tutorial was computed at a single grain. :ref:`explanation-metric-grain` explains what that means, what changes once a view carries metrics on more than one table, and why the table a metric is declared on decides the numbers it returns.
