.. meta::
   :description: Group a metric by a dimension on the other side of a bridge table, such as books by author country, counting each row once per group

.. _howto-many-to-many:

=====================================================
How to Model Many-to-Many Relationships with a Bridge
=====================================================

.. versionadded:: 0.19.0

This guide shows how to group or filter a metric by a dimension that is linked to it many-to-many, such as page counts by the authors' country when a book can have several authors and an author several books. The link is a **bridge table** (``book_authors``) that references both sides. No new syntax is needed: you declare the bridge's two relationships, and the extension counts each book once per group, never once per author, as Snowflake does.

**Prerequisites:**

- A working multi-table semantic view (see :ref:`tutorial-multi-table`)
- Familiarity with how a metric is counted below its grain (see :ref:`howto-fan-traps`)


.. _howto-m2m-setup:

Set Up Example Tables
=====================

Four authors, four books and the bridge between them. *Duo* has two French authors, *Mixed* a French and a British one, *Solo* one British author, and *Orphan* none. Dan has written nothing yet:

.. code-block:: sql

   CREATE TABLE authors (author_id INTEGER, name VARCHAR, country VARCHAR);
   INSERT INTO authors VALUES
       (1, 'Ana',  'FR'),
       (2, 'Ben',  'FR'),
       (3, 'Cleo', 'UK'),
       (4, 'Dan',  'UK');

   CREATE TABLE books (book_id INTEGER, title VARCHAR, pages INTEGER);
   INSERT INTO books VALUES
       (10, 'Duo',    100),
       (11, 'Mixed',  200),
       (12, 'Solo',   300),
       (13, 'Orphan',  50);

   CREATE TABLE book_authors (book_id INTEGER, author_id INTEGER, role VARCHAR);
   INSERT INTO book_authors VALUES
       (10, 1, 'lead'),
       (10, 2, 'co'),
       (11, 3, 'lead'),
       (11, 1, 'co'),
       (12, 3, 'lead');


.. _howto-m2m-define:

Declare the Bridge
==================

Declare the bridge as a table with a key, and give it one relationship to each side. Both relationships start at the bridge, because the bridge holds both foreign keys:

.. code-block:: sql
   :emphasize-lines: 5,8,9

   CREATE SEMANTIC VIEW library
   TABLES (
       b  AS books        PRIMARY KEY (book_id),
       a  AS authors      PRIMARY KEY (author_id),
       ba AS book_authors PRIMARY KEY (book_id, author_id)
   )
   RELATIONSHIPS (
       ba_book   AS ba(book_id)   REFERENCES b,
       ba_author AS ba(author_id) REFERENCES a
   )
   DIMENSIONS (
       b.title   AS b.title,
       a.author  AS a.name,
       a.country AS a.country,
       ba.role   AS ba.role
   )
   METRICS (
       b.total_pages  AS SUM(b.pages),
       b.book_count   AS COUNT(*),
       a.author_count AS COUNT(*)
   );

Every table a metric is declared on needs a ``PRIMARY KEY``. The extension tells a book's groups apart by it (see :ref:`howto-m2m-troubleshooting`).


.. _howto-m2m-group:

Group a Metric Across the Bridge
================================

Count books and pages by the authors' country:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(library
       DIMENSIONS country
       METRICS book_count, total_pages
   ) ORDER BY country;

.. code-block:: text

   ┌─────────┬────────────┬─────────────┐
   │ country │ book_count │ total_pages │
   ├─────────┼────────────┼─────────────┤
   │ FR      │          2 │         300 │
   │ UK      │          2 │         500 │
   │ NULL    │          1 │          50 │
   └─────────┴────────────┴─────────────┘

*Duo* counts once for ``FR``, although both its authors are French. *Mixed* counts once for ``FR`` and once for ``UK``. *Orphan* has no author and counts in the ``NULL`` group. A plain join of books to authors would have counted *Duo*'s 100 pages twice.

The groups overlap, so they add up to more than the total. Query the metrics without the dimension for the total, which counts each book once:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(library
       METRICS book_count, total_pages
   );

.. code-block:: text

   ┌────────────┬─────────────┐
   │ book_count │ total_pages │
   ├────────────┼─────────────┤
   │          4 │         650 │
   └────────────┴─────────────┘

The same works the other way round. An author count by title counts each author once per book, and Dan, with no books, is in the ``NULL`` group:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(library
       DIMENSIONS title
       METRICS author_count
   ) ORDER BY title;

.. code-block:: text

   ┌────────┬──────────────┐
   │ title  │ author_count │
   ├────────┼──────────────┤
   │ Duo    │            2 │
   │ Mixed  │            2 │
   │ Solo   │            1 │
   │ NULL   │            1 │
   └────────┴──────────────┘


.. _howto-m2m-filter:

Filter Across the Bridge
========================

A filter on the other side keeps a book when any of its authors matches, and counts it once:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(library
       METRICS book_count, total_pages
       WHERE country = 'FR'
   );

.. code-block:: text

   ┌────────────┬─────────────┐
   │ book_count │ total_pages │
   ├────────────┼─────────────┤
   │          2 │         300 │
   └────────────┴─────────────┘

*Duo* and *Mixed* have a French author. *Mixed* is kept although its other author is British.


.. _howto-m2m-bridge-dims:

Group by a Column of the Bridge
===============================

A dimension on the bridge itself groups each book by the bridge rows it has, once per value. Here each book with an author has one lead, and two have a co-author:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(library
       DIMENSIONS role
       METRICS total_pages
   ) ORDER BY role;

.. code-block:: text

   ┌──────┬─────────────┐
   │ role │ total_pages │
   ├──────┼─────────────┤
   │ co   │         300 │
   │ lead │         600 │
   │ NULL │          50 │
   └──────┴─────────────┘

A dimension on a table past the far side, such as a ``countries`` table that ``authors`` references for a continent, works the same way, each book still counted once per group.


.. _howto-m2m-dims-only:

List the Pairs the Bridge Holds
===============================

A query with only dimensions on the two sides lists the pairs the bridge holds. It reads ``book_authors`` and joins up to both sides, so *Orphan* and Dan are not listed:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(library
       DIMENSIONS title, author
   ) ORDER BY title, author;

.. code-block:: text

   ┌───────┬────────┐
   │ title │ author │
   ├───────┼────────┤
   │ Duo   │ Ana    │
   │ Duo   │ Ben    │
   │ Mixed │ Ana    │
   │ Mixed │ Cleo   │
   │ Solo  │ Cleo   │
   └───────┴────────┘

``DIMENSIONS title`` alone lists all four titles, *Orphan* included, since it reads the books themselves (see :ref:`Dimensions only <ref-sv-modes>`).


.. _howto-m2m-fact-bridge:

Use a Fact Table as the Bridge
==============================

A fact table that references two dimension tables links them many-to-many too. Sales reference customers and products, so a customer count by product category counts each customer once per category they bought from:

.. code-block:: sql

   CREATE TABLE customers (customer_id INTEGER, name VARCHAR);
   INSERT INTO customers VALUES (1, 'Ana'), (2, 'Ben'), (3, 'Cleo');

   CREATE TABLE products (product_id INTEGER, category VARCHAR);
   INSERT INTO products VALUES (100, 'book'), (101, 'book'), (200, 'toy');

   CREATE TABLE sales (sale_id INTEGER, customer_id INTEGER, product_id INTEGER, amount DECIMAL(10,2));
   INSERT INTO sales VALUES
       (1, 1, 100, 20.00),
       (2, 1, 101, 30.00),
       (3, 1, 200, 30.00),
       (4, 2, 100, 20.00);

   CREATE SEMANTIC VIEW shop
   TABLES (
       s AS sales     PRIMARY KEY (sale_id),
       c AS customers PRIMARY KEY (customer_id),
       p AS products  PRIMARY KEY (product_id)
   )
   RELATIONSHIPS (
       sale_customer AS s(customer_id) REFERENCES c,
       sale_product  AS s(product_id)  REFERENCES p
   )
   DIMENSIONS (
       p.category AS p.category
   )
   METRICS (
       c.customer_count AS COUNT(*),
       s.revenue        AS SUM(s.amount)
   );

   SELECT * FROM SEMANTIC_VIEW(shop
       DIMENSIONS category
       METRICS customer_count, revenue
   ) ORDER BY category;

.. code-block:: text

   ┌──────────┬────────────────┬─────────┐
   │ category │ customer_count │ revenue │
   ├──────────┼────────────────┼─────────┤
   │ book     │              2 │   70.00 │
   │ toy      │              1 │   30.00 │
   │ NULL     │              1 │    NULL │
   └──────────┴────────────────┴─────────┘

Ana bought two books and a toy, and counts once under each category. Cleo bought nothing and counts in the ``NULL`` group. ``revenue`` is on the sales themselves, so it is an ordinary many-to-one query: each sale counts once.

The same holds for a metric whose table reaches the bridge by a hop up. With ``reviews`` referencing ``books``, a review count by author country counts each review once per country its book's authors have.


.. _howto-m2m-verify:

Verify the Generated SQL
========================

:ref:`explain_semantic_view() <ref-explain-semantic-view>` shows how a book is kept to one row per group: the distinct pairs of a book key and a country are found first, then joined to the books and aggregated:

.. code-block:: duckdb-sql

   SELECT * FROM explain_semantic_view('library',
       dimensions := ['country'],
       metrics := ['book_count']
   );

.. code-block:: text

   -- Expanded SQL:
   SELECT
       "__sv_bridge"."__sv_d0" AS "country",
       COUNT(*) AS "book_count"
   FROM "memory"."main"."books" AS "b"
   JOIN (
       SELECT DISTINCT
           "b"."book_id" AS "__sv_k0",
           a.country AS "__sv_d0"
       FROM "memory"."main"."books" AS "b"
       LEFT JOIN "memory"."main"."book_authors" AS "ba" ON "ba"."book_id" = "b"."book_id"
       LEFT JOIN "memory"."main"."authors" AS "a" ON "ba"."author_id" = "a"."author_id"
   ) AS "__sv_bridge" ON "b"."book_id" IS NOT DISTINCT FROM "__sv_bridge"."__sv_k0"
   GROUP BY
       1

:ref:`SHOW SEMANTIC DIMENSIONS ... FOR METRIC <ref-show-dims-for-metric>` lists the dimensions across the bridge among those a metric can be grouped by: for ``total_pages`` it lists ``author``, ``country``, ``role`` and ``title``.


.. _howto-m2m-limits:

What Is Refused
===============

- **A semi-additive metric** whose ``NON ADDITIVE BY`` dimension is not in the query cannot be grouped or filtered across a bridge. The query is refused as a fan trap. With its ``NON ADDITIVE BY`` dimension in the query it is an ordinary metric and is counted once per group.
- **Two bridges between the same tables**, such as ``book_authors`` and ``book_editors``, are accepted at ``CREATE``. A query that crosses from one side to the other is refused, as in Snowflake, since a book would be paired with its authors through either bridge. ``USING`` cannot pick one: a metric's ``USING`` names a relationship from the metric's own table, and both relationships start at the bridges. A query that stays on one side is answered.
- **A window clause that names a dimension across the bridge.** A window metric can be grouped across a bridge, and is computed over its inner metric's once-per-group result, but its ``PARTITION BY`` and ``ORDER BY`` cannot name an author dimension in a window over a book metric, since a book has no single author country to order by. ``CREATE`` refuses it, as Snowflake does (see :ref:`howto-window-relationships`).


.. _howto-m2m-troubleshooting:

Troubleshooting
===============

**fan trap detected -- metric 'total_pages' (table 'b') would be duplicated when joined to dimension 'country' (table 'a') via relationship 'ba_book'**
   The metric's table has no ``PRIMARY KEY``, or the metric is semi-additive and its ``NON ADDITIVE BY`` dimension is not queried. The message ends with a sentence listing these cases. Declare ``PRIMARY KEY`` on the metric's table: a ``UNIQUE`` key is not enough, because it can hold more than one ``NULL``.

**dimension 'country' is on table 'a', which metric 'total_pages' reaches from table 'b' by no chain of relationships and by more than one path across a many-to-many relationship: b <- ba -> a, b <- be -> a**
   The view has two bridges between books and authors. Query members on one side, or a metric on one of the bridges, or declare the second bridge's far table under its own alias (``ed AS authors``), so that each bridge leads to its own table. See :ref:`err-multi-path`.

**The groups add up to more than the total**
   A row that reaches several groups counts in each, as *Mixed* does above. Query the metric without the dimension for the total.

**A book is missing from a dimensions-only query**
   A query with dimensions on both sides lists only the pairs the bridge holds. Add a metric on the books, such as ``book_count``, to keep every book, with a ``NULL`` group for the books with no author.


.. _howto-m2m-related:

Related
=======

- :ref:`Many-to-many relationships through a bridge <ref-create-bridge-relationships>` -- The rules in the ``CREATE SEMANTIC VIEW`` reference
- :ref:`howto-fan-traps` -- How a metric is counted by a dimension below its grain
- :ref:`howto-asof-range` -- Relationships that match on a date or a period
- :ref:`explanation-metric-grain` -- Why a metric is counted over its own table
