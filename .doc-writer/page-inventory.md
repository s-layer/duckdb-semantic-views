# Page Inventory -- v0.16.0 Content Refresh (the SEMANTIC_VIEW(...) query surface)

**Generated:** 2026-10-25
**Mode:** Update, content refresh plus new sections on existing pages (no new pages)
**Trigger:** v0.16.0 makes the `SEMANTIC_VIEW(v DIMENSIONS ... METRICS ... FACTS ... WHERE ...)` construct the query front door, removes the parser-override hook, and adds expression items, `AS` aliases / `column_order`, `params`, prepared-statement parameters in the construct's `WHERE`, and SHOW / DESCRIBE as subqueries. Source of truth: `git log v0.15.0..HEAD` and CHANGELOG `[Unreleased]`. `semantic_view()` stays a supported function; the docs lead with the construct and keep the function form where it is the subject (binding lists, `params`, `column_order`, `explain_semantic_view()`).

Every converted query was run against the built extension before and after the change (each page's sql blocks in order, results compared); new examples and their output tables were produced by running them.

## Content Refresh

| # | Type | Title | Key Sections / Discrepancies | File Path |
|---|------|-------|------------------------------|-----------|
| 1 | (refresh) | SEMANTIC_VIEW(...) and semantic_view() | Retitled; intro and Syntax lead with the construct, the function follows with the clause-to-parameter mapping. Construct section moved up beside Syntax (composition in joins, column aliases, prepared parameters). Examples, Output, Filtering and Name Resolution in the construct. Expression-items example fixed to name the `shop` view's members (it named `o.order_date` / `o.amount`, which an expression item now refuses). New construct example for binding values from Python. | docs/reference/semantic-view-function.rst |
| 2 | (refresh) | explain_semantic_view() | How to explain a construct query (clause = parameter, `WHERE` = `where_clause`); items may be expressions. | docs/reference/explain-semantic-view-function.rst |
| 3 | (refresh) | Error Messages | Query Errors cover the construct. New entries: malformed construct, parameter outside `WHERE`, two columns with one name, expression item errors (incl. `count(*)` with no member). Function-list wording in existing entries names the clauses too. | docs/reference/error-messages.rst |
| 4 | (refresh) | Getting Started | Queries in the construct; new step filtering inside the construct; note introducing `semantic_view()` as the function form. | docs/tutorials/getting-started.rst |
| 5 | (refresh) | Building a Complete Model | Queries in the construct; new step trying `revenue - cost AS profit` as an expression item before declaring it. | docs/tutorials/building-a-model.rst |
| 6 | (refresh) | Filtering | Leads with the `WHERE` inside the construct; `where_clause` shown as the function spelling; application section leads with `?` in the construct's `WHERE`. | docs/how-to/filtering.rst |
| 7 | (refresh) | Query Facts | `FACTS` clause; new section on row-level expression items. | docs/how-to/query-facts.rst |
| 8 | (refresh) | Snowflake / Databricks comparisons | Query-interface rows and the pre-aggregation `WHERE` parity row were stale (said `where_clause` only / "table function"). | docs/explanation/snowflake-comparison.rst, docs/explanation/databricks-comparison.rst |
| 9 | (refresh) | Remaining tutorials, how-tos, explanation and reference pages | Query examples converted to the construct; prose naming `dimensions := [...]`, `facts := [...]` or `where_clause` updated to the clauses. | docs/**/*.rst |
| 10 | (refresh) | README | Quick start and multi-table queries in the construct; expression items, SHOW as a subquery, and the function form shown once. | README.md |

## Pages Verified -- No Changes Needed

| Type | Title | Reason | File Path |
|------|-------|--------|-----------|
| (refresh) -- no changes needed | Functions | Lists the registered functions and their `duckdb_functions()` examples, which are the function form by design. | docs/reference/functions.rst |
| (refresh) -- no changes needed | YAML pages | No query examples beyond `READ_YAML_FROM_SEMANTIC_VIEW()`. | docs/how-to/yaml-definitions.rst, docs/reference/yaml-format.rst, docs/reference/read-yaml-from-semantic-view.rst |
| (refresh) -- no changes needed | Release notes | Includes CHANGELOG.md verbatim. | docs/changelog.md |

## API Reference Status

Manual SQL reference under `docs/reference/` (config `api_reference: manual`). New anchors: `ref-err-construct`, `ref-err-expression-items`, `howto-query-facts-expressions`.

## Coverage Gaps

- For a construct query whose expanded SQL fails to bind, DuckDB's caret points at the start of the query, not at the construct; documented in the reference Output section.
