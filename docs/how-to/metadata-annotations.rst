.. meta::
   :description: Add COMMENT, WITH SYNONYMS, PRIVATE/PUBLIC access modifiers, LABELS = (FILTER) and WITH TAG to semantic view definitions and inspect them via DESCRIBE and SHOW

.. _howto-metadata-annotations:

=====================================
How to Use Metadata Annotations
=====================================

This guide shows how to annotate dimensions, metrics, facts, and tables with comments, synonyms, access modifiers, named-filter labels and tags in a semantic view definition.

**Prerequisites:**

- A working semantic view with ``TABLES``, ``DIMENSIONS``, and ``METRICS`` (see :ref:`tutorial-multi-table`)
- Familiarity with :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` output


.. _howto-annotations-comment:

Add Comments
============

Comments are human-readable descriptions attached to any entry in the view definition. They appear in :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` output and in the ``comment`` column of ``SHOW`` commands.


View-Level Comment
------------------

Set a comment on the semantic view itself using :ref:`ALTER <ref-alter-semantic-view>`:

.. code-block:: sql

   ALTER SEMANTIC VIEW sales SET COMMENT = 'Revenue and order analytics for the North America region';

Remove a view-level comment:

.. code-block:: sql

   ALTER SEMANTIC VIEW sales UNSET COMMENT;

.. tip::

   View-level comments appear in the ``comment`` column of :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>` and as a ``SEMANTIC_VIEW`` object kind row in :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>`.


Table-Level Comment
-------------------

Add a ``COMMENT`` clause after the table declaration:

.. code-block:: sql
   :emphasize-lines: 3,4

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       o AS orders    PRIMARY KEY (id) COMMENT = 'Core order transactions',
       c AS customers PRIMARY KEY (id) COMMENT = 'Customer master data'
   )
   RELATIONSHIPS (order_customer AS o(customer_id) REFERENCES c)
   DIMENSIONS (o.region AS o.region)
   METRICS (o.revenue AS SUM(o.amount));


Comments on Dimensions, Metrics, and Facts
-------------------------------------------

Add ``COMMENT`` after the expression on any entry:

.. code-block:: sql
   :emphasize-lines: 6,9,12

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       li AS line_items PRIMARY KEY (id)
   )
   FACTS (
       li.net_price AS li.extended_price * (1 - li.discount) COMMENT = 'Price after discount'
   )
   DIMENSIONS (
       li.region AS li.region COMMENT = 'Sales region from shipping address'
   )
   METRICS (
       li.total_net AS SUM(li.net_price) COMMENT = 'Net revenue after discounts'
   );


.. _howto-annotations-synonyms:

Add Synonyms
============

Synonyms are alternative names for an entry. They are informational metadata -- they do not affect query resolution, but they appear in :ref:`DESCRIBE <ref-describe-semantic-view>` and ``SHOW`` output for discoverability.

Add ``WITH SYNONYMS`` after the expression (or after the ``COMMENT`` clause if both are present):

.. code-block:: sql
   :emphasize-lines: 6,9

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       o AS orders PRIMARY KEY (id) COMMENT = 'Order data' WITH SYNONYMS = ('transactions', 'purchases')
   )
   DIMENSIONS (
       o.region AS o.region WITH SYNONYMS = ('sales_region', 'territory')
   )
   METRICS (
       o.revenue AS SUM(o.amount) COMMENT = 'Total sales' WITH SYNONYMS = ('total_sales', 'gmv')
   );

Synonyms appear as a JSON array in :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` output (e.g., ``["sales_region","territory"]``) and in the ``synonyms`` column of :ref:`SHOW SEMANTIC DIMENSIONS <ref-show-semantic-dimensions>`.


.. _howto-annotations-access:

Set Access Modifiers (PRIVATE / PUBLIC)
=======================================

Metrics and facts support ``PRIVATE`` and ``PUBLIC`` access modifiers. ``PUBLIC`` is the default. ``PRIVATE`` items cannot be queried directly. A private metric can only be referenced by derived metrics. A private fact can only be referenced by metric expressions and by other facts.

.. code-block:: sql
   :emphasize-lines: 6,10,11,12

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       li AS line_items PRIMARY KEY (id)
   )
   FACTS (
       PRIVATE li.raw_margin AS li.price - li.cost
   )
   METRICS (
       li.total_revenue AS SUM(li.price),
       PRIVATE li.total_cost AS SUM(li.cost),
       li.total_margin AS SUM(li.raw_margin),
       profit AS total_revenue - total_cost
   );

In this example:

- ``raw_margin`` is a private fact -- it can be referenced by metrics (like ``total_margin``) and by other facts, but cannot be queried (``FACTS raw_margin``), used in a query's expression items, or named in its ``WHERE``.
- ``total_cost`` is a private metric -- it can be referenced by derived metrics (like ``profit``) but cannot be queried (``METRICS total_cost``).
- ``total_margin`` and ``profit`` are public (default) and use the private items to compute their values.

.. warning::

   ``PRIVATE`` is placed before the table alias (``PRIVATE li.total_cost``), not after the expression. Dimensions accept ``PUBLIC`` (the default) but not ``PRIVATE``: a private dimension is rejected with ``PRIVATE is not supported on dimensions``.


.. _howto-annotations-filters:

Mark a Named Filter
===================

A **named filter** is a boolean-valued fact or dimension meant to be reused in a query's pre-aggregation predicate instead of selected as output. Declare one by adding ``LABELS = (FILTER)`` after the expression:

.. code-block:: sql
   :emphasize-lines: 7,10

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       o AS orders PRIMARY KEY (id)
   )
   FACTS (
       o.amount AS o.amount,
       o.is_large AS o.amount > 100 LABELS = (FILTER)
   )
   DIMENSIONS (
       o.is_domestic AS o.country = 'US' LABELS = (FILTER)
   )
   METRICS (
       o.revenue AS SUM(o.amount)
   );

Reference the filter by name in the query's :ref:`WHERE <ref-sv-pre-agg-filtering>`, which applies it *before* the metrics aggregate:

.. code-block:: sql

   SELECT * FROM SEMANTIC_VIEW(sales METRICS revenue WHERE is_domestic AND is_large);

The label is **declarative metadata**, not an access restriction or a resolution rule:

- The ``WHERE`` inside ``SEMANTIC_VIEW(...)`` (``where_clause`` in ``semantic_view()``) already substitutes any declared fact or dimension name, labeled or not. The label records that the member *exists to be filtered on*, and surfaces it in :ref:`DESCRIBE <ref-describe-semantic-view>` and :ref:`GET_DDL <ref-get-ddl>` for discoverability.
- A filter is still a queryable member. ``DIMENSIONS is_domestic`` returns its boolean values like any other dimension. There is no way to hide a filter while keeping it usable: ``PRIVATE`` applies only to facts and metrics, and a private fact is refused in ``WHERE`` as well as in ``FACTS``.

.. note::

   The ``BOOLEAN`` requirement is checked by DuckDB's binder at **query** time, not at ``CREATE``. Typing an arbitrary SQL expression requires a binder, so a filter over a non-boolean expression is created successfully and raises DuckDB's own type error the first time it is used in a predicate.

``FILTER`` is the only supported label. Any other value is rejected at ``CREATE`` rather than silently dropped, so a definition cannot round-trip having quietly lost a label you wrote.

``LABELS`` is likewise valid **only on a fact or a dimension**. Writing it on a table, on a metric, or as a view-level annotation is rejected for the same reason: those entries carry no filter flag, so accepting it there would mean discarding it on the way to storage.


.. _howto-annotations-tags:

Add Tags
========

.. versionadded:: 0.17.0

Tags are name/value pairs for tools that read your view's metadata, such as a governance catalog or a BI layer that needs to know what each member is. Like comments, they never change what a query returns. Add ``WITH TAG (...)`` to any table, fact, dimension or metric entry, or after the last clause to tag the view itself:

.. code-block:: sql
   :emphasize-lines: 3,7,10,12

   CREATE SEMANTIC VIEW sales AS
   TABLES (
       o AS orders PRIMARY KEY (id) WITH TAG (governance.pii = 'none')
   )
   DIMENSIONS (
       o.region AS o.region COMMENT = 'Sales region'
           WITH TAG (s_layer_kind = 'dimension', s_layer_label = 'Region')
   )
   METRICS (
       o.revenue AS SUM(o.amount) WITH TAG (s_layer_kind = 'metric')
   )
   WITH TAG (owner = 'analytics');

The ``WITH`` is optional, as in Snowflake. A tag name can be qualified (``governance.pii``, ``db.governance.pii``) and is stored as written: there is no ``CREATE TAG`` object to create first. Values are single-quoted strings of up to 256 characters.

Read the tags back from the ``TAGS`` row in ``DESCRIBE``, which holds a JSON object:

.. code-block:: sql

   SELECT object_name, json_extract_string(property_value, '$.s_layer_kind') AS kind
   FROM describe_semantic_view('sales')
   WHERE property = 'TAGS' AND object_kind IN ('DIMENSION', 'METRIC');

``GET_DDL`` and :ref:`READ_YAML_FROM_SEMANTIC_VIEW() <ref-read-yaml>` write the tags back out, so they survive a dump and replay. ``SHOW`` commands don't list them, which matches Snowflake. See :ref:`Tags <ref-create-tags>` for the full rules.


.. _howto-annotations-inspect:

Inspect Annotations
===================

Via DESCRIBE
------------

:ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` shows annotation properties as additional rows:

.. code-block:: sql

   DESCRIBE SEMANTIC VIEW sales;

Look for these property rows:

- ``COMMENT`` -- the comment text (conditional, only when set)
- ``SYNONYMS`` -- JSON array of synonyms (conditional, only when set)
- ``LABELS`` -- ``["FILTER"]`` for a :ref:`named filter <howto-annotations-filters>` (conditional, only on labeled facts and dimensions)
- ``TAGS`` -- JSON object of :ref:`tags <howto-annotations-tags>` (conditional, only when set)
- ``ACCESS_MODIFIER`` -- ``PUBLIC`` or ``PRIVATE`` (always emitted for facts and metrics)
- ``NON_ADDITIVE_BY`` -- non-additive dimension list (conditional, only for semi-additive metrics)
- ``WINDOW_SPEC`` -- reconstructed OVER clause (conditional, only for window metrics)

Via SHOW Commands
-----------------

The :ref:`SHOW SEMANTIC DIMENSIONS <ref-show-semantic-dimensions>`, :ref:`SHOW SEMANTIC METRICS <ref-show-semantic-metrics>`, and :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>` commands include ``synonyms`` and ``comment`` columns in their output:

.. code-block:: sql

   SHOW SEMANTIC DIMENSIONS IN sales;

.. code-block:: text

   ┌───────────────┬─────────────┬────────────────────┬────────────┬────────┬───────────┬──────────────────────────────┬─────────┐
   │ database_name │ schema_name │ semantic_view_name │ table_name │ name   │ data_type │ synonyms                     │ comment │
   ├───────────────┼─────────────┼────────────────────┼────────────┼────────┼───────────┼──────────────────────────────┼─────────┤
   │ memory        │ main        │ sales              │ orders     │ region │ VARCHAR   │ ["sales_region","territory"] │         │
   └───────────────┴─────────────┴────────────────────┴────────────┴────────┴───────────┴──────────────────────────────┴─────────┘

This output is for the ``sales`` view from :ref:`howto-annotations-synonyms`. Its ``region`` dimension has synonyms but no comment.

.. tip::

   Private items are excluded from :ref:`SHOW COLUMNS IN SEMANTIC VIEW <ref-show-columns>` and from wildcard expansion (``alias.*``). They still appear in :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` (with ``ACCESS_MODIFIER`` set to ``PRIVATE``), :ref:`SHOW SEMANTIC METRICS <ref-show-semantic-metrics>`, and :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>`.


.. _howto-annotations-troubleshoot:

Troubleshooting
===============

**Comment not appearing in SHOW SEMANTIC VIEWS**
   Only view-level comments appear in :ref:`SHOW SEMANTIC VIEWS <ref-show-semantic-views>`. Table/dimension/metric/fact comments appear in :ref:`DESCRIBE SEMANTIC VIEW <ref-describe-semantic-view>` and in the ``comment`` column of :ref:`SHOW SEMANTIC DIMENSIONS <ref-show-semantic-dimensions>`, :ref:`SHOW SEMANTIC METRICS <ref-show-semantic-metrics>`, and :ref:`SHOW SEMANTIC FACTS <ref-show-semantic-facts>`.

**Cannot query a private metric or fact**
   Private items return an error when queried directly, for example ``metric 'total_cost' is private and cannot be queried directly``. Reference a private metric from a derived metric, and a private fact from a metric or another fact. To make an item queryable again, recreate the view without the ``PRIVATE`` keyword.

**Synonyms not affecting query resolution**
   Synonyms are informational metadata only. They do not expand the set of names recognized by :ref:`SEMANTIC_VIEW(...) and semantic_view() <ref-semantic-view-function>` or :ref:`explain_semantic_view() <ref-explain-semantic-view>`. Use the declared name to query an item.

**COMMENT, WITH SYNONYMS, LABELS and TAG order**
   The annotations on one entry may appear in any order -- ``o.region AS o.region WITH SYNONYMS = ('territory') COMMENT = 'c'`` parses the same as the reverse. What the parser does require is that the annotation region be *tiled* by recognized clauses: once the first annotation keyword is seen, everything after it must be a valid ``COMMENT`` / ``WITH SYNONYMS`` / ``LABELS`` / ``[WITH] TAG`` clause separated by whitespace. Leftover text (``COMMENT = 'a' banana``) or a repeated clause is an error rather than being silently dropped.

**Unsupported label**
   ``FILTER`` is the only value accepted in ``LABELS = (...)``. Other label values are rejected at ``CREATE`` -- deliberately, so a definition cannot round-trip having quietly lost a label. Remove the unsupported value to create the view.

**LABELS rejected on a table, metric, or the view itself**
   ``LABELS`` applies only to facts and dimensions -- they are the entries that carry the filter flag. On a ``TABLES`` or ``METRICS`` entry, or in the trailing view-level annotation position, it raises *LABELS is not valid on a ...*. Move the annotation to the fact or dimension you meant to mark.

**A named filter still shows up in query output**
   Expected. ``LABELS = (FILTER)`` declares intent and drives introspection; it does not hide the member. ``PRIVATE`` is not a way round this: it applies only to facts and metrics, and a private fact cannot be used in ``WHERE`` either.


.. _howto-annotations-related:

Related
=======

- :ref:`ref-create-semantic-view` -- Where each annotation fits in the ``CREATE SEMANTIC VIEW`` grammar
- :ref:`ref-describe-semantic-view` -- The property rows annotations produce
- :ref:`howto-filtering` -- Use named filters in a query's ``WHERE``
- :ref:`howto-wildcard-selection` -- How private items are left out of ``alias.*``
