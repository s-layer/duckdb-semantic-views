# Persona Report

**Generated:** 2026-11-06
**Audience:** Data engineers exploring semantic views (intermediate)
**Scope:** v0.18.0 changed and new pages. Scenarios S9-S12 appended to `.doc-writer/scenarios.yaml` (S1-S8 reused, not re-run in this pass: the page set grew by three pages, under the regeneration threshold).
**Scenarios tested:** 4 (S9-S12)
**Results (first evaluation):** 2 PASS, 2 PARTIAL, 0 FAIL
**Results (after fixes):** 4 PASS, 0 PARTIAL, 0 FAIL

## Summary

The three new features are easy to reach: the home page names all three with links, the How-To index lists them, and the navbar dropdown has each with a one-line summary. Each new page starts from table creation with data, so every example can be pasted into a fresh DuckDB session and the shown output reproduced. Snowflake differences are called out where they bite (standard SQL cannot set variables, OR ALTER drops rather than suspends materializations, name resolution in inline queries). The first pass had two points of friction. The transaction preview on the CREATE OR ALTER page used a placeholder statement that could not be run as written, and the role-playing page showed a refused query without its error. Both are fixed, along with two smaller "show me the generated SQL" gaps.

## Per-Page Verdicts

| Page | Scenario(s) | Before | After |
|------|-------------|--------|-------|
| docs/how-to/view-variables.rst | S9 | PASS (minor: explain output described, not shown) | PASS |
| docs/how-to/query-defined-tables.rst | S10 | PASS (minor: no generated SQL shown) | PASS |
| docs/how-to/create-or-alter.rst | S11 | PARTIAL | PASS |
| docs/how-to/role-playing-dimensions.rst | S12 | PARTIAL | PASS |
| docs/how-to/yaml-definitions.rst | S11 | PASS | PASS |
| docs/how-to/data-sources.rst | S10 | PASS | PASS |
| docs/explanation/snowflake-comparison.rst | S9-S12 | PASS | PASS |
| docs/index.rst, docs/how-to/index.rst, docs/conf.py (nav) | S9-S12 | PASS | PASS |
| README.md | S9-S12 | PASS | PASS |

---

## Scenario S9: Give a view a threshold and a rate that each query can set, and bind them from Python

**Verdict:** PASS

### Navigation Path

1. Started at: docs/index.rst
   - Found: intro sentence linking "view variables" to `howto-variables`.
2. Navigated to: how-to/view-variables.rst
   - Found: where `VARIABLES` sits in the clause order and the entry grammar; a complete example with data, query and output for the defaults; a per-query override with output; the `semantic_view()` struct form; a default that is re-evaluated per query (`current_date`) with a back-dated query; required variables with the exact error; `PREPARE`/`EXECUTE` and a Python function with bound parameters and the returned rows; DESCRIBE `VARIABLE` rows; limits (standard SQL, materializations, window frames); troubleshooting by error text.
   - Friction (first pass): "Verify the Generated SQL" described where values land but did not show the expanded SQL. A reader who wanted to check the cast had to run it. Fixed: the expanded SQL excerpt is now shown.
3. Followed: Related → `ref-create-variables`, `ref-sv-variables`, `explanation-snowflake` (row lists DuckDB's type names for refused types).

---

## Scenario S10: Model a logical table as a query

**Verdict:** PASS

### Navigation Path

1. Started at: docs/index.rst → "tables defined by a query" link.
2. Navigated to: how-to/query-defined-tables.rst
   - Found: syntax with keys, comment, synonyms; a two-table example (filtered orders, joined customer details) with output that visibly excludes the refunded order; a PRIMARY KEY warning explaining the double count; a pre-aggregated table at a new grain, with an explanation of why the relationship is one-to-one; reading Parquet directly, with guidance on when to prefer a DuckDB `CREATE VIEW`; name resolution (schema of creation, never a CTE); DESCRIBE, GET_DDL and YAML forms; the upgrade warning for older releases; troubleshooting by error text.
   - Friction (first pass, minor): context.md asks examples to show the generated SQL, and this page did not. A reader could not see that the table's query is inlined as a subquery. Fixed: an `explain_semantic_view()` example with the expanded SQL is in "Inspect and Export the Definition".
3. Also visited: how-to/data-sources.rst Iceberg section, which now points to this page for putting `iceberg_scan` inside the view.

---

## Scenario S11: Deploy the same definition on every release, and check YAML first

**Verdict:** PARTIAL (first pass) → PASS

### Navigation Path

1. Started at: docs/index.rst → "CREATE OR ALTER" link.
2. Navigated to: how-to/create-or-alter.rst
   - Found: a table of what OR ALTER keeps and what OR REPLACE resets; a setup view with tags and a materialization; an altering statement and a list of what changed and what was kept; a warning that anything left out is unset; how a materialization is dropped and restored; changing tags around the statement; YAML deploy from a file; `verify_semantic_view_yaml` with its output; a warning about exported YAML carrying `tags:`; troubleshooting by error text.
   - Friction (first pass): "Preview a Change Before Committing It" used `-- ... the rest of the new definition ...` inside the `CREATE OR ALTER`, so the block could not be pasted and run, and it did not say what the SHOW would return.
3. Followed: yaml-definitions.rst "Check a Definition Without Creating It", which has a complete runnable YAML example.

### Gap Analysis (first pass)

**Where:** how-to/create-or-alter.rst > Preview a Change Before Committing It
**What:** The example statement was a placeholder, not a runnable definition, and the expected result was not shown.
**Impact:** The page's main safety workflow (try in a transaction, inspect, roll back) could not be followed by copy-paste.
**Suggested Fix:** In create-or-alter.rst, section Preview: give a complete statement against the page's `sales` view, say what `SHOW SEMANTIC MATERIALIZATIONS` returns inside the transaction and after `ROLLBACK`.
**Applied:** Yes. The statement now reverts the refund rule; the comments and the following sentence give the empty SHOW inside the transaction and `by_region` after rollback. Verified against the extension.

---

## Scenario S12: Departures and arrivals per city in one query

**Verdict:** PARTIAL (first pass) → PASS

### Navigation Path

1. Started at: docs/index.rst → How-To Guides → role-playing-dimensions.rst (index blurb mentions picking the route with `USING`).
2. Navigated to: how-to/role-playing-dimensions.rst
   - Found: tables with data; the two relationships and `USING` metrics; departures-by-city output; the combined departures-and-arrivals query with output and an explanation of the NULLs; the rule that a metric without `USING` cannot be grouped by city; carrier output; derived metrics across paths.
   - Friction (first pass): the arrivals-only query had no output, so the reader could not check the second role on its own. The "Every metric needs a path" example was marked "Refused" with no error text, which left the reader unable to recognize the failure in their own logs.

### Gap Analysis (first pass)

**Where:** how-to/role-playing-dimensions.rst > Query with USING Context
**What:** Missing output for the arrivals query; missing error text for the refused query.
**Impact:** Slows verification and leaves the refusal unrecognizable; the goal itself was reachable.
**Suggested Fix:** In role-playing-dimensions.rst, section Query with USING Context: add the arrivals output table and the binder error for the refused query.
**Applied:** Yes, both, taken from runs against the extension.

---

## Revision Recommendations

### FAIL Issues (trigger revision)

No FAIL issues.

### PARTIAL Issues (for project author approval)

All PARTIAL issues found in this pass were cheap and clearly right, and were applied and re-checked (see S11 and S12). Remaining observations, not applied:

| Scenario | Page | Gap | Suggested Fix |
|----------|------|-----|---------------|
| S10 | how-to/query-defined-tables.rst | A customer with no paid orders has no `ltv` row, so `value_tier` is NULL for them; the example data has none, so this is not visible. | Optional: one sentence noting the NULL tier, or add a customer without paid orders to the example data. |
| S9 | how-to/view-variables.rst | Snowflake differences (DuckDB type names for refused types) are only on the comparison page, linked from Related. | Optional: a one-line pointer in the intro. Low value; the link exists. |
