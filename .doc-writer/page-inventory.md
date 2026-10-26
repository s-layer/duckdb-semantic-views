# Page Inventory -- v0.17.0 Content Refresh (standard-SQL queries, CREATE binds every member, member tags)

**Generated:** 2026-10-26
**Mode:** Update, content refresh of existing pages (no new pages)
**Trigger:** `git log v0.16.0..HEAD`: #314 (query a semantic view by name with standard SQL), #315 (`CREATE SEMANTIC VIEW` binds every member and refuses one DuckDB cannot bind), #316 (member-level `[WITH] TAG`). Source of truth: CHANGELOG `[Unreleased]`, `cpp/src/shim.cpp` (standard-SQL rewrite and refusals), `test/sql/standard_sql_query*.test`, `test/sql/create_binds_members.test`, `test/sql/member_tags.test`.

Approval: the inventory was not put to the project author (no interactive session); every refresh entry below was taken as approved, and the "no changes needed" entries as deselected.

Each changed or added SQL example was run against the debug build of this code (scratch sqllogictest files under `/tmp/claude-0/docs-pass-0170/`); output tables were produced by running them.

## Content Refresh

| # | Type | Title | Key Sections / Discrepancies | File Path |
|---|------|-------|------------------------------|-----------|
| 1 | (refresh) | How to Query a Semantic View with Standard SQL | No example data, so no example runs as written; the join example joins region totals to customers by region (repeats rows). Add a set-up view, outputs, ad-hoc aggregates and fact rows, a prepared-statement / Python section for application code, a troubleshooting section quoting the refusal messages, links to the error reference. | docs/how-to/standard-sql.rst |
| 2 | (refresh) | Getting Started | Teaches only the construct and the function. Add a step querying the same view with `SELECT ... AGG(...) ... GROUP BY`; summary line. | docs/tutorials/getting-started.rst |
| 3 | (refresh) | Multi-Table Semantic Views | "Update the View" step: say that `CREATE OR REPLACE` binds every member and that a refused replace leaves the old view in place. | docs/tutorials/multi-table.rst |
| 4 | (refresh) | Home page | Intro and meta description name only the construct. | docs/index.rst |
| 5 | (refresh) | SEMANTIC_VIEW(...) and semantic_view() | Intro: the standard-SQL form is a third spelling that turns into the construct. | docs/reference/semantic-view-function.rst |
| 6 | (refresh) | Error Messages | Query Errors intro names the standard-SQL form; new entry for the standard-SQL refusals. | docs/reference/error-messages.rst |
| 7 | (refresh) | Snowflake Comparison | Feature Parity "Direct SQL query interface: Not planned" is false; Concept Mapping gains a row for expression checking at CREATE. | docs/explanation/snowflake-comparison.rst |
| 8 | (refresh) | Semantic Views vs Regular Views | Query interface row and the "What a Semantic View Does" example show only the construct; add a row for what CREATE checks. | docs/explanation/semantic-views-vs-regular-views.rst |
| 9 | (refresh) | Filtering | Tip mapping the standard-SQL form's `WHERE` / `HAVING` onto the two filters; Related link. | docs/how-to/filtering.rst |
| 10 | (refresh) | Facts | Troubleshooting: "Fact name not found" and "syntax error at or near AS when queried" now fail at CREATE. | docs/how-to/facts.rst |
| 11 | (refresh) | Derived Metrics | Troubleshooting: a table-qualified column in a derived metric is now refused at CREATE. | docs/how-to/derived-metrics.rst |
| 12 | (refresh) | Window Metrics | Troubleshooting: a window metric over a window metric is now refused at CREATE. | docs/how-to/window-metrics.rst |
| 13 | (refresh) | Data Sources | Troubleshooting "Catalog Error": CREATE now checks the tables its members read; schema-evolution note that a replace is checked. | docs/how-to/data-sources.rst |
| 14 | (refresh) | Metadata Annotations | BOOLEAN note: CREATE binds the filter but does not check its type. | docs/how-to/metadata-annotations.rst |
| 15 | (refresh, persona PARTIAL fix) | CREATE SEMANTIC VIEW | Bind-at-CREATE note: a refused OR REPLACE keeps the old view; what is still not checked (materialization tables, a table no member reaches, a filter's BOOLEAN type). Tags section already current. | docs/reference/create-semantic-view.rst |

## Pages Verified -- No Changes Needed

| Type | Title | Reason | File Path |
|------|-------|--------|-----------|
| (refresh) -- no changes needed | DESCRIBE / YAML format / GET_DDL | Tag rows and YAML shape landed with #316. | docs/reference/describe-semantic-view.rst, docs/reference/yaml-format.rst, docs/reference/get-ddl.rst |
| (refresh) -- no changes needed | Databricks Comparison | Query-interface rows and section updated with #314. | docs/explanation/databricks-comparison.rst |
| (refresh) -- no changes needed | How-to index | Lists the standard-SQL guide. | docs/how-to/index.rst |
| (refresh) -- no changes needed | Building a Complete Model | Its queries and refactors all CREATE and run on the build; no claim about when errors appear. | docs/tutorials/building-a-model.rst |
| (refresh) -- no changes needed | Functions | Lists `__sv_check_create` as internal. | docs/reference/functions.rst |
| (refresh) -- no changes needed | Release notes | Includes CHANGELOG.md verbatim. | docs/changelog.md |

## API Reference Status

Manual SQL reference under `docs/reference/` (config `api_reference: manual`). New anchors: `ref-err-standard-sql`, `howto-standard-sql-setup`, `howto-standard-sql-adhoc`, `howto-standard-sql-app`, `howto-standard-sql-verify`, `howto-standard-sql-troubleshooting`, `howto-standard-sql-related`, `ref-err-facts-metrics`, `tutorial-gs-standard-sql`.

## Audience Targeting

Single persona: Data engineers exploring semantic views (intermediate).

## Coverage Gaps

- README.md (outside this pass's scope, `docs/**` only) does not mention the standard-SQL query form.
