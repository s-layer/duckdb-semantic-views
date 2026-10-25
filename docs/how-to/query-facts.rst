.. meta::
   :description: Query facts directly as row-level columns with the FACTS clause of SEMANTIC_VIEW(...), without aggregation

.. _howto-query-facts:

===========================
How to Query Facts Directly
===========================

This guide shows how to query facts as row-level columns with the ``FACTS`` clause of :ref:`SEMANTIC_VIEW(...) <ref-sv-construct>` (the ``facts`` parameter of :ref:`semantic_view() <ref-semantic-view-function>`). Unlike metrics, fact queries return individual rows without aggregation.

**Prerequisites:**

- A semantic view with a ``FACTS`` clause (see :ref:`howto-facts`)
- Familiarity with :ref:`SEMANTIC_VIEW(...) <ref-semantic-view-function>` queries


.. _howto-query-facts-basic:

Query Facts
===========

List fact names after ``FACTS`` to retrieve them as row-level columns:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       FACTS net_price, tax_amount
   );

Each row in the result contains the computed fact values -- no aggregation is applied. This is equivalent to a ``SELECT`` with the fact expressions inlined.


.. _howto-query-facts-dims:

Combine Facts with Dimensions
==============================

Add dimensions alongside facts. The dimensions appear as columns but do not trigger ``GROUP BY`` -- the output remains row-level:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       DIMENSIONS region
       FACTS net_price
   );

This returns one row per source row, with ``region`` and ``net_price`` columns.


.. _howto-query-facts-expressions:

Compute a Row-Level Expression
==============================

.. versionadded:: 0.16.0

An item in ``FACTS`` can be an expression over the facts and dimensions of one table, named with ``AS``. It is computed per row, like a declared fact:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       FACTS net_price, net_price + tax_amount AS gross_price
   );

The expression lasts for this query only. Declare it in the ``FACTS`` clause when other queries or metrics should use it too. See :ref:`ref-sv-expression-items` for what an expression may use.


.. _howto-query-facts-mutual:

Mutual Exclusion with Metrics
==============================

.. warning::

   Facts and metrics cannot be combined in the same query.

.. code-block:: sql

   -- This fails:
   SELECT * FROM SEMANTIC_VIEW(analytics
       METRICS total_revenue
       FACTS net_price
   );

.. code-block:: text

   semantic view 'analytics': cannot combine facts and metrics in the same query.
   Use facts := [...] OR metrics := [...], not both.

To get both row-level facts and aggregated metrics, run two separate queries. The error names the ``semantic_view()`` parameters because the construct turns into a call to it; for the construct, read them as the ``FACTS`` and ``METRICS`` clauses.


.. _howto-query-facts-wildcard:

Wildcard Selection for Facts
=============================

Use ``alias.*`` to select all public facts for a table alias:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(analytics
       FACTS li.*
   );

Private facts are excluded from wildcard expansion. See :ref:`howto-wildcard-selection` for details.


.. _howto-query-facts-verify:

Verify with explain_semantic_view()
====================================

Use :ref:`explain_semantic_view() <ref-explain-semantic-view>` to see the expanded SQL and query plan for a fact query. It takes the ``FACTS`` clause as its ``facts`` parameter:

.. code-block:: sql

   SELECT * FROM explain_semantic_view('analytics',
       facts := ['net_price', 'tax_amount']
   );

You can also use :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` to inspect ``FACT`` entries and their expressions.


.. _howto-query-facts-troubleshoot:

Troubleshooting
===============

**Unknown fact name**
   The fact name must match a fact declared in the ``FACTS`` clause. The error message lists available facts and suggests close matches.

**Private fact cannot be queried**
   Facts marked ``PRIVATE`` cannot be queried directly. They can only be referenced in metric expressions and other facts. Remove the ``PRIVATE`` keyword to make a fact queryable.

**Incompatible table paths**
   A fact query returns rows as they are, so the tables it references must be joinable without multiplying them. If neither of two tables can be reached from the other without traversing a one-to-many relationship against its direction -- typically two tables that both reference a third -- the extension returns ``fact query references objects from incompatible table paths``. A chain of many-to-one relationships is fine however long it is.


.. _howto-query-facts-related:

Related
=======

- :ref:`ref-semantic-view-function` -- The ``facts`` parameter and the other query modes
- :ref:`howto-facts` -- Declare the facts you query here
- :ref:`howto-wildcard-selection` -- Select every fact on a table alias with ``alias.*``
- :ref:`howto-fan-traps` -- Why some table combinations cannot be joined without multiplying rows
