# Persona Report: Data engineers exploring semantic views

**Generated:** 2026-10-26 (v0.17.0 content refresh; S1-S6 reused from `.doc-writer/scenarios.yaml`, S7 and S8 appended for this release)
**Audience:** Data engineers exploring semantic views (intermediate)
**Scenarios tested:** 7 of 8 (S6 not re-run: none of its pages changed)
**Results (pass 1):** 6 PASS, 2 PARTIAL, 0 FAIL
**Results (after the PARTIAL fixes were applied):** 7 PASS, 0 PARTIAL, 0 FAIL

## Summary

Coming from Snowflake, the new standard-SQL form is easy to find: the home page names it with a
link, the getting-started tutorial runs it against the tutorial's own view, and the Snowflake
comparison no longer calls direct SQL "not planned". The how-to now has its own example data, so
every query on it can be pasted and checked against the printed output. The bind-at-CREATE change
reads consistently: the tutorials, troubleshooting entries and comparison pages all say a bad
member is refused at `CREATE`. Two scenarios needed a small fix each on the first pass, and both
fixes were applied.

---

## Scenario S1: install, first view, three query modes

**Verdict:** PASS

### Navigation Path

1. Started at: `index.rst`. The intro now says a view can be queried with `SEMANTIC_VIEW(...)` or a `SELECT region, AGG(revenue) ... GROUP BY region`, with a link.
2. Followed "Getting started" card to `tutorials/getting-started.rst`: install, data, view, three modes, two filters, then a new step "Query It with Standard SQL" that returns the same rows as the previous query, with output.
3. "What You Learned" lists the new form and links the how-to.

---

## Scenario S2: star schema, relationships, grain

**Verdict:** PASS

### Navigation Path

1. `index.rst` -> "Multi-table semantic views" -> `tutorials/multi-table.rst`.
2. "Update the View" now shows a `CREATE OR REPLACE` with a misspelled column, the exact error, and states the old definition stays. This is the behavior I want before trusting a deploy script.
3. Grain pages unchanged since the last PASS.

---

## Scenario S3: compare with Snowflake and Databricks

**Verdict:** PASS

### Navigation Path

1. `index.rst` -> "Snowflake comparison" -> `explanation/snowflake-comparison.rst`.
2. Concept Mapping: Query interface row lists the construct, standard SQL and the table function; a new "Expression checking" row matches Snowflake's compile-at-CREATE.
3. Query Interface section: side-by-side, with the two DuckDB-specific differences (name shadowing, `SELECT *`).
4. Feature Parity Notes: the standard-SQL row is **Supported**, with the rules; no row still says "Not planned" for it.
5. `explanation/databricks-comparison.rst`: `MEASURE()` vs `AGG()` side by side; consistent.

---

## Scenario S4: sources, request-scoped filters, application

**Verdict:** PASS

### Navigation Path

1. `how-to/data-sources.rst`: troubleshooting now says `CREATE` reports a missing table its members read, and that the plain catalog error means the table went missing after `CREATE`.
2. `how-to/filtering.rst`: a tip maps standard SQL's `WHERE` / `HAVING` onto the inner and outer filters; Related links the how-to.
3. `how-to/standard-sql.rst` > "Pass Values from Application Code": `PREPARE` / `EXECUTE` with output and a Python function with two bound parameters and its result, plus a link back to the filtering page's safety rules.

---

## Scenario S5: semi-additive and window metrics

**Verdict:** PASS

### Navigation Path

1. `how-to/window-metrics.rst` > Troubleshooting: "Window metrics cannot be used as inner metrics" now says `CREATE` refuses it and warns the DuckDB part of the message may be a syntax error. Rest unchanged since the last PASS.

---

## Scenario S7: standard-SQL queries from Snowflake habits, from Python

**Verdict (pass 1):** PARTIAL -> **PASS** after fix

### Navigation Path

1. `index.rst` intro -> link -> `how-to/standard-sql.rst`.
2. Set up: tables and view to paste; first query with output.
3. Clause mapping table; aliasing; expressions over `AGG` with `HAVING`; ad-hoc aggregates and fact rows; qualified names; subquery join and CTE window workarounds; parameters from SQL and Python; what is refused; troubleshooting by error message; Related links to the error reference.

### Gap Analysis (pass 1)

**Where:** `how-to/standard-sql.rst` (no section)
**What:** No way to see the SQL the extension writes for a standard-SQL query. Every other query how-to has a "Verify" step; `EXPLAIN` here shows only DuckDB's plan.
**Impact:** Context says this persona checks generated SQL before trusting a number. Determined users could translate to `explain_semantic_view()` from the mapping table themselves.
**Suggested Fix:** In `how-to/standard-sql.rst`, add a "Verify the Generated SQL" section that maps the query onto `explain_semantic_view()` and says what to look for. Also say the view can be schema-qualified in `FROM`.
**Applied:** Yes (section `howto-standard-sql-verify`; example run against the build).

---

## Scenario S8: when are definition mistakes reported

**Verdict (pass 1):** PARTIAL -> **PASS** after fix

### Navigation Path

1. `index.rst` -> "DDL reference" -> `reference/create-semantic-view.rst`: note "Every member is bound at CREATE" with the forms covered, the session context, and a link to the error entry.
2. `reference/error-messages.rst` > "Members that do not bind": message format, cause, fix, example.
3. `tutorials/multi-table.rst`: a refused `CREATE OR REPLACE` keeps the old view.

### Gap Analysis (pass 1)

**Where:** `reference/create-semantic-view.rst` > note "Every member is bound at CREATE"
**What:** The reference did not say that a refused `CREATE OR REPLACE` keeps the existing view (only the tutorial did), and listed only materialization tables as unchecked. A filter's `BOOLEAN` type is unchecked too (only in the annotations how-to), and so is a table no member reaches.
**Impact:** A deploy script author reading only the reference would not know which failures can still appear at query time.
**Suggested Fix:** In `reference/create-semantic-view.rst`, note "Every member is bound at CREATE": add the replace behavior and the full list of what `CREATE` does not check.
**Applied:** Yes (each claim checked against the build).

---

## Revision Recommendations

### FAIL Issues (trigger revision)

None.

### PARTIAL Issues (for project author approval)

| Scenario | Page | Gap | Suggested Fix | Status |
|----------|------|-----|---------------|--------|
| S7 | how-to/standard-sql.rst | No way to see the generated SQL | Add "Verify the Generated SQL"; mention schema-qualified view names | Applied (default: apply all) |
| S8 | reference/create-semantic-view.rst | Replace behavior and unchecked items not in the reference | Extend the bind-at-CREATE note | Applied (default: apply all) |

## Observations (not fixed; for the project author)

- `README.md` (outside this pass, which edited `docs/**` only) does not mention the standard-SQL query form.
