# Editor Report

**Generated:** 2026-10-26 (v0.17.0 content refresh: standard-SQL queries, CREATE binds every member, member tags)
**Files reviewed:** 15 changed pages (14 from the Author pass, plus `reference/create-semantic-view.rst` from the persona PARTIAL fixes); terminology pass over all docs/ pages
**Changes made:** 9
  - BLOCKING: 0
  - SUGGESTION: 6
  - NITPICK: 3

## Summary

Fourteen pages were refreshed for v0.17.0. Every changed or added SQL example, and every
error text quoted in new prose, was run against the debug build of this code in scratch
sqllogictest files (`standard_sql.test`, `getting_started.test`, `multi_table.test`,
`misc.test`: 103 statements, all passing, including the persona-pass fixes); the Python example on the standard-SQL page was run
in the DuckDB 2.0.0 Python package with the same build. `sphinx-build -W` passes. No
accuracy mismatch was left in place.

---

## docs/how-to/standard-sql.rst

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| Aggregate Ad Hoc and Return Fact Rows | Heading ("Aggregate Facts and Dimensions Ad Hoc") did not cover the section's second half, a row-level fact query. | Renamed. |
| Troubleshooting | Draft listed "must be passed to an aggregate or appear in GROUP BY" as the error for a stray `SELECT` item; the build gives that only when an aggregate and a bare fact share one expression. A bare item next to `AGG` gives "cannot combine facts and metrics". | Entry rewritten around the message the build actually gives, including the misspelled-name case. |

### NITPICK

| Section | Description | Fix |
|---------|-------------|-----|
| Related | Heading was "See Also"; every other how-to ends in "Related" (project rule: how-to layout). | Renamed, with the `howto-standard-sql-related` label. |

## docs/explanation/snowflake-comparison.rst

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| Concept Mapping, Query interface | DuckDB column said "direct SQL"; the how-to, CHANGELOG and other pages say "standard SQL". "Direct SQL" stays in the Snowflake column, which quotes Snowflake's term. | Normalized to "standard SQL". |
| Feature Parity Notes | Draft row linked a Snowflake docs URL that could not be verified offline. | Link removed; the row describes the behavior and links the how-to. |

## docs/explanation/semantic-views-vs-regular-views.rst

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| What a Semantic View Does | "The resemblance stops at the surface." is a stock phrase. | "It only looks like a table query." |

## docs/how-to/data-sources.rst

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| Troubleshooting, Catalog Error | "the plain error appears" was vague about which error. | Says the query raises this error when a table goes missing after CREATE. |

### NITPICK

| Section | Description | Fix |
|---------|-------------|-----|
| Troubleshooting | Kept the page's existing `--` dash style rather than introducing em dashes. | None needed. |

## docs/how-to/facts.rst

### NITPICK

| Section | Description | Fix |
|---------|-------------|-----|
| Troubleshooting | The second entry's bold heading now quotes the error the reader sees (`fact '...' does not bind: ...`), matching how the page's other entries lead with the symptom. | Heading changed. |

---

## Diataxis Type Integrity

- `tutorials/getting-started.rst`, `tutorials/multi-table.rst`: new steps are do-then-observe with
  expected output; explanation kept to one sentence each and linked out to the how-to and the
  error reference.
- `how-to/standard-sql.rst`: the clause-mapping table is reference-like but scoped to the task;
  the full message list moved to the error reference (`ref-err-standard-sql`) and is linked rather
  than repeated.
- `explanation/semantic-views-vs-regular-views.rst`: the new example is illustrative, not a step.

## Cross-References

All new `:ref:` targets resolve (strict build). New anchors: `ref-err-standard-sql`,
`ref-err-facts-metrics`, `howto-standard-sql-setup`, `howto-standard-sql-adhoc`, `howto-standard-sql-verify`,
`howto-standard-sql-app`, `howto-standard-sql-troubleshooting`, `howto-standard-sql-related`,
`tutorial-gs-standard-sql`. Pages that mention the standard-SQL form now link
`howto-standard-sql`: index, getting-started, the query reference, error reference, filtering,
both comparisons, semantic-vs-regular views.

---

## Terminology Changes

| Term | Before | After | Authority |
|------|--------|-------|-----------|
| standard SQL | "direct SQL" (DuckDB column of the Snowflake concept table) | "standard SQL" | CHANGELOG `[Unreleased]`, `how-to/standard-sql.rst` title |
| does not bind | n/a (new term) | added to `terminology.yaml` with the rule that CREATE no longer accepts an unbindable member | `ref-err-member-bind` |
| define-time inference pass | note said the bind probe runs only on the query path | note updated: CREATE binds every member (a check, not type inference) | #315 |
