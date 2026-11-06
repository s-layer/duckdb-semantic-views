# Editor Report

**Generated:** 2026-11-06
**Scope:** v0.18.0 docs pass, changed and new pages only (examples/ excluded)
**Files reviewed:** 11 (docs/how-to/view-variables.rst, query-defined-tables.rst, create-or-alter.rst, role-playing-dimensions.rst, yaml-definitions.rst, data-sources.rst, how-to/index.rst, docs/explanation/snowflake-comparison.rst, docs/index.rst, docs/conf.py, README.md)
**Changes made:** 18
  - BLOCKING: 1
  - SUGGESTION: 12
  - NITPICK: 5

## Summary

The three new how-to pages are accurate: every SQL block on them, and the new output tables on the role-playing page, was run against the built extension (DuckDB 2.0.0) and the results and error texts match. One troubleshooting entry named the wrong error for a missing table, and three pages stacked admonitions back to back. The remaining edits are wording: ambiguous "the query's WHERE", one terminology slip, missing cross-references and two British spellings. No INTERNAL NOTES blocks were present to strip. `sphinx-build -W` passes.

Accuracy method: SQL extracted from each page and run with `uv run --with duckdb==2.0.0` against `build/debug/semantic_views.duckdb_extension`; claims about errors were checked by triggering them, and two behaviors (which error a missing table reports; the `:=` default form) were confirmed against `src/query/member_types.rs` and by running them.

---

## docs/how-to/query-defined-tables.rst

### BLOCKING

| Section | Description | Fix |
|---------|-------------|-----|
| Troubleshooting | Accuracy mismatch: entry was headed `table 'c' does not bind: Catalog Error ...`. When a member reads the table (as `city` and `segment` read `c` in the page's view), CREATE reports the first such member (`dimension 'city' does not bind: Catalog Error: Table with name ... does not exist`); `table 'c' does not bind` appears only when no member reads the table (`first_unbound_member`, src/query/member_types.rs). | Heading changed to the dimension form; body now says the error names the first member that reads the table, or the table itself when none does. |

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| Join and Filter | Stacked admonitions (warning + tip back to back). | Tip turned into a paragraph; the PRIMARY KEY warning stays. |
| Join and Filter, How Table Names Resolve | "the query's WHERE" was ambiguous on a page where "the query" usually means the table's query (2 instances). | Now "the WHERE inside SEMANTIC_VIEW(...)". |
| Pre-Aggregate | "each customer has exactly one ltv row and the relationship is one-to-one": a customer with no paid orders has none, and the reader is not told why the extension treats it as one-to-one (persona never_assume: relationship modeling). | "its foreign-key column c.id is c's primary key, so each customer has at most one ltv row and the extension infers the relationship as one-to-one". |
| Intro | First mention of `GET_DDL` unlinked (Rule 4). | Linked to `ref-get-ddl`. |

## docs/how-to/view-variables.rst

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| Declare Variables | Stacked admonitions (tip + warning). | Typing tip turned into a paragraph; the shadowing warning stays. |
| Make a Variable Required | Terminology: "tools that write plain SQL" (term map: "standard SQL"). | Now "standard SQL". |
| Set Variables in a Query | First mention of the construct unlinked (Rule 4). | Linked to `ref-sv-construct`. |

### NITPICK

| Section | Description | Fix |
|---------|-------------|-----|
| Declare Variables | "``:=`` may stand for ``DEFAULT``" reads oddly. | "``:=`` can replace ``DEFAULT``". |

## docs/how-to/create-or-alter.rst

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| Keep Materializations Correct | Stacked admonitions (note + tip); the tip carried a Snowflake difference, which context.md assigns to `.. warning::`. | Schema-resolution note turned into a paragraph; Snowflake difference is now a warning. |
| Deploy from a YAML File | "which is how Snowflake's create_or_alter option for YAML works here" was vague about which Snowflake surface is meant. | Names the `create_or_alter` argument of `SYSTEM$CREATE_SEMANTIC_VIEW_FROM_YAML`, matching the comparison page. |
| Deploy from a YAML File | `verify_semantic_view_yaml` unlinked on its first mention; "pass the YAML text" omitted the view-name argument. | Linked to `ref-create-verify-yaml`; "pass the view name and the YAML text". |
| Change the View | `SHOW SEMANTIC MATERIALIZATIONS` unlinked (Rule 4). | Linked to `ref-show-semantic-materializations`. |

## docs/index.rst

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| Intro | "Snowflake's CREATE SEMANTIC VIEW clauses use Snowflake's syntax" was circular. | "The CREATE SEMANTIC VIEW clauses follow Snowflake's syntax". |

## docs/explanation/snowflake-comparison.rst

### NITPICK

| Section | Description | Fix |
|---------|-------------|-----|
| USING RELATIONSHIPS, Clause semantics | British "behaviour" (2 instances); convention is American spelling. | "behavior". |

## docs/how-to/role-playing-dimensions.rst

No editor changes. New data rows, output tables and the corrected `:emphasize-lines` (7,8,17,18) were verified against the built extension.

### NITPICK

| Section | Description | Fix |
|---------|-------------|-----|
| Query with USING Context | (Persona-step fix, recorded here for completeness.) Arrivals query had no output; refused query had no error text. | Output and error added; see the persona report. |

## docs/how-to/yaml-definitions.rst, data-sources.rst, how-to/index.rst, docs/conf.py, README.md

No changes needed. The verify example in yaml-definitions runs as written. nav_links summaries and toctree entries match the new page titles. README's em dashes are in lines this pass did not touch (README is Markdown and outside the RST `--` convention).

### NITPICK

| Section | Description | Fix |
|---------|-------------|-----|
| README.md | Pre-existing em dashes in untouched lines 178-181, 220. | Left as is (out of scope). |

---

## Diataxis Type Integrity

All three new pages are how-to guides and follow the project layout (title, versionadded, intro, Prerequisites, tasks, Troubleshooting, Related). "Choose Between OR ALTER and OR REPLACE" and "Variables and Materializations" are short decision aids that stay goal-oriented; no structural blur.

## Cross-Reference Linking

`api_reference: manual`. Every `:ref:` on the changed pages resolves (`sphinx-build -W` exits 0). Links added: `ref-get-ddl`, `ref-sv-construct`, `ref-create-verify-yaml`, `ref-show-semantic-materializations`.

## Terminology Changes

| Term | Before | After | Authority |
|------|--------|-------|-----------|
| standard SQL | plain SQL | standard SQL | terminology.yaml project_terms |
| behavior | behaviour | behavior | terminology.yaml conventions (American spelling) |

terminology.yaml needed no new entries: the new pages use existing canonical forms ("the WHERE inside SEMANTIC_VIEW(...)", `where_clause` only in the function form, "fan trap", "define time").
