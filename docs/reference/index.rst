.. meta::
   :description: Complete SQL syntax reference for all CREATE, ALTER, DROP, DESCRIBE, SHOW, GET_DDL, READ_YAML, and query function statements, the SEMANTIC_VIEW(...) query construct, plus the full list of registered functions

.. _reference:

=========
Reference
=========

SQL syntax reference for all DuckDB Semantic Views statements and functions.

**DDL statements**

- :ref:`ref-create-semantic-view` -- Create a new semantic view with tables, relationships, dimensions, metrics, facts, and materializations.
- :ref:`ref-alter-semantic-view` -- Rename, set, or unset comment on a semantic view.
- :ref:`ref-drop-semantic-view` -- Remove a semantic view from the catalog.
- :ref:`ref-describe-semantic-view` -- Inspect the full definition of a semantic view.
- :ref:`ref-show-semantic-views` -- List all registered semantic views with optional filtering.
- :ref:`ref-show-semantic-dimensions` -- List dimensions across one or all semantic views.
- :ref:`ref-show-semantic-metrics` -- List metrics across one or all semantic views.
- :ref:`ref-show-semantic-facts` -- List facts across one or all semantic views.
- :ref:`ref-show-semantic-materializations` -- List materializations across one or all semantic views.
- :ref:`ref-show-dims-for-metric` -- List dimensions safe to use with a specific metric (fan trap aware).
- :ref:`ref-show-columns` -- List all queryable columns in a semantic view with types, expressions, and comments.
- :ref:`ref-get-ddl` -- Retrieve the full CREATE DDL text for a stored semantic view.
- :ref:`ref-read-yaml` -- Export a semantic view definition as a YAML string.
- :ref:`ref-yaml-format` -- Field-by-field specification of the YAML schema accepted by ``FROM YAML``.

**Querying**

- :ref:`ref-semantic-view-function` -- Query a semantic view with any combination of dimensions, metrics and facts, with the ``SEMANTIC_VIEW(...)`` construct or the ``semantic_view()`` table function.
- :ref:`ref-explain-semantic-view` -- Inspect the SQL generated for a semantic view query.

**Functions and discovery**

- :ref:`ref-functions` -- Every function the extension registers, which ones to call directly, and how a tool reads their parameters and examples from ``duckdb_functions()``.

**Error reference**

- :ref:`ref-error-messages` -- Error messages, causes, and fixes.

.. toctree::
   :hidden:

   create-semantic-view
   alter-semantic-view
   drop-semantic-view
   describe-semantic-view
   show-semantic-views
   show-semantic-dimensions
   show-semantic-metrics
   show-semantic-facts
   show-semantic-materializations
   show-semantic-dimensions-for-metric
   show-columns-semantic-view
   get-ddl
   read-yaml-from-semantic-view
   yaml-format
   semantic-view-function
   explain-semantic-view-function
   functions
   error-messages
