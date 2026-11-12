# Editor Report

**Generated:** 2026-11-12
**Scope:** v0.19.0 docs pass, changed and new pages only (examples/ excluded)
**Files reviewed:** 15 (docs/how-to/asof-and-range-relationships.rst, many-to-many-relationships.rst, fan-traps.rst, role-playing-dimensions.rst, filtering.rst, how-to/index.rst, docs/explanation/metric-grain.rst, snowflake-comparison.rst, docs/reference/create-semantic-view.rst, semantic-view-function.rst, error-messages.rst, docs/index.rst, docs/conf.py, README.md, CHANGELOG.md)
**Changes made:** 14
  - BLOCKING: 4
  - SUGGESTION: 6
  - NITPICK: 4

## Summary

The two new how-to pages are accurate: every SQL block on them was run against the release build (DuckDB 2.0.0, extension f19aef0) and the output tables, explain output, DESCRIBE rows and error texts are the real ones. Claims with no shown output (a table past the bridge's far side, a metric reaching the bridge by a hop up, `book_count` keeping a book with no author, a `UNIQUE`-only metric table being refused, a `NULL` ASOF date) were each probed. The blocking finds were refusal lists that described cases the build answers, or that a user cannot produce. No INTERNAL NOTES blocks were present. `just docs-check` passes.

Accuracy method: SQL extracted from each page and run with the DuckDB 2.0.0 Python package against the release build; output tables compared cell by cell; refusals triggered and their text compared.

---

## docs/how-to/fan-traps.rst

### BLOCKING

| Section | Description | Fix |
|---------|-------------|-----|
| When the Extension Raises a Fan Trap Error | Listed "a dimension or filter in the query is declared on no table". DDL (`alias.name AS expr`) and YAML (`dimension ... has no source_table`) both refuse such a member, and `WHERE 1 = 1` or a constant-valued dimension beside a fan-out is answered, so the reader cannot reach it. | Bullet removed. |

## docs/reference/error-messages.rst

### BLOCKING

| Section | Description | Fix |
|---------|-------------|-----|
| Fan trap detected, Cause | "a role-played table is on the path, or a ... member declared on no table": an airport count by a gate terminal (a fan-out below the role-played table) is answered. | Now: the hop crosses one of a role-played table's relationships back toward the table that holds them. |
| Ambiguous fact or metric on a role-playing table, Cause | Second form said an airport count by a flight's carrier, or with a `NON ADDITIVE BY` dimension on flights, raises it; both are reported as `fan trap detected`. | Cause rewritten to point to the fan trap entry for those; label `ref-err-fan-trap` added. |

## docs/how-to/asof-and-range-relationships.rst

### BLOCKING

| Section | Description | Fix |
|---------|-------------|-----|
| Title | Overline shorter than the title (`sphinx-build -W` failure). | Over/underlines resized (same on many-to-many-relationships.rst). |

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| ASOF | NULL handling shown for range only; a `NULL` order date was not covered. | One sentence, probed: kept with a `NULL` city. |
| Inspect | Only the ASOF join shown. | One sentence on the range join's `ON` condition (from the real explain output). |

## Other pages

### SUGGESTION

| File | Section | Description | Fix |
|------|---------|-------------|-----|
| fan-traps.rst | Bridge bullets, Related | Linked the CREATE reference for the worked example. | Now link `howto-many-to-many`; Related lists both new pages. |
| create-semantic-view.rst | ASOF, range, bridge sections | No link to a worked example (Rule 4). | Links to `howto-asof`, `howto-range`, `howto-many-to-many`. |
| how-to/index.rst, docs/index.rst, conf.py | Navigation | New pages unlisted. | Index entries, toctree, navbar children, home-page intro links. |
| CHANGELOG.md | [Unreleased] Changed | Bullets out of order of reader impact; double blank line. | Reordered (fan-out first, window rules last); blank line removed. |

### NITPICK

| File | Description | Fix |
|------|-------------|-----|
| metric-grain.rst | Dash parenthetical (humanizer). | Split into a plain clause. |
| semantic-view-function.rst | Dash parenthetical. | Parentheses. |
| terminology.yaml | No entries for the release's terms. | Added ASOF relationship, range relationship, bridge table (with variants to avoid). |
| conf.py | Fan Traps summary still said the error is raised for fan-outs. | "How a metric below its grain is counted, and when a fan trap is refused". |

## Checks with no finding

Terminology (fan trap open, American spelling, no tracker IDs), Diataxis placement (both new pages task-led, with Prerequisites, Troubleshooting and Related as the how-to layout requires), code-block languages (all set; no bare `::`), stacked admonitions (none), cross-references on first mention of `explain_semantic_view()`, `DESCRIBE SEMANTIC VIEW`, `GET_DDL`, `SHOW SEMANTIC DIMENSIONS ... FOR METRIC`.
