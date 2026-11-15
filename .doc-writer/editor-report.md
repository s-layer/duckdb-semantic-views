# Editor Report

**Generated:** 2026-11-15
**Scope:** v0.20.0 docs pass, changed and new pages only (examples/ excluded)
**Files reviewed:** 14 (docs/how-to/dimension-references.rst, many-to-many-relationships.rst, window-metrics.rst, lod-expressions.rst, asof-and-range-relationships.rst, how-to/index.rst, docs/explanation/snowflake-comparison.rst, docs/reference/create-semantic-view.rst, semantic-view-function.rst, error-messages.rst, docs/index.rst, docs/conf.py, README.md, CHANGELOG.md)
**Changes made:** 13
  - BLOCKING: 4
  - SUGGESTION: 6
  - NITPICK: 3

## Summary

Phase 8 landed its reference text with each PR, but three of its features had no task page: direct many-to-many relationships, dimension references, and window facts and dimensions. They now have one each (a new page for dimension references, a new section on each of the many-to-many and window pages). Every SQL block on the new page and sections was run against a build of main and the output tables are the real ones. The blocking finds were a wrong storage-format statement repeated in five places, a wrong error text, a navbar entry missing for an existing page, and an ASOF statement made false by direct many-to-many. No INTERNAL NOTES blocks were present. `just docs-check` passes.

Accuracy method: SQL extracted from each page and run with the DuckDB 2.0.0 Python package against the build (`LOAD` by SQL, as the Python `load_extension` path does not activate the grammar until a statement parses); output tables compared cell by cell; refusals triggered and their text compared.

---

## BLOCKING

| File | Section | Description | Fix |
|------|---------|-------------|-----|
| CHANGELOG.md, create-semantic-view.rst | Format 4 notes (3 + 3 places) | "0.19.0 and older refuse to read it." Only 0.18.0 and 0.19.0 check the stored format; 0.17.0 and older open a format-4 view without checking and answer wrongly. | "0.19.0 and 0.18.0 refuse to read it, while 0.17.0 and older open it without checking, so don't open such a database with them." |
| error-messages.rst | LOD INCLUDE entry | Text said "fact 'orders_incl_customer'"; the build says "LOD fact 'orders_incl_customer'". | Text replaced with the build's. |
| conf.py | Navbar | LOD Expressions (added in 8.5a) was missing from the How-To children. | Added, with Dimension References. |
| create-semantic-view.rst, asof-and-range-relationships.rst | ASOF key rule | "as for any relationship": since #366 a relationship to non-key columns is many-to-many. | ASOF and range need the target's key; another relationship to non-key columns is many-to-many, which an ASOF one can't be. |

## SUGGESTION

| File | Section | Description | Fix |
|------|---------|-------------|-----|
| many-to-many-relationships.rst | New section `howto-m2m-direct` | Direct many-to-many in the reference only. | Section with data, view, grouped/filtered/dims-only outputs, keyless-metric refusal and format-4 warning; page retitled. |
| dimension-references.rst | New page | Dimension references in the reference only. | Task page: set up, declare, query, what is refused (full error text), related. |
| window-metrics.rst | New section `howto-window-members` | Window facts and dimensions in the reference only. | Section ranking and totalling rows, with a filter showing the window sees every row. |
| how-to/index.rst, docs/index.rst | Navigation | New page and sections unlisted; LOD entry named FIXED only. | Index entries, toctree, home-page intro links. |
| snowflake-comparison.rst | Concept Mapping, LOD row | Named FIXED only. | Names FIXED, INCLUDE or EXCLUDE. |
| README.md | Features | No direct many-to-many, dimension references, window members or INCLUDE/EXCLUDE; optional relationship names unmentioned. | Bullets added; one paragraph on `SYS_RELATIONSHIP_<id>` names. |

## NITPICK

| File | Description | Fix |
|------|-------------|-----|
| CHANGELOG.md | LOD bullet title said `FIXED DIMENSIONS` though the bullet covers all three. | Title names all three. |
| conf.py | Many-to-many summary named the bridge only. | Covers non-key columns too. |
| how-to/index.rst | Meta description lacked the new topics. | Updated. |

## Checks with no finding

- `semantic-view-function.rst` DIMENSIONS subquery example run against the page's `shop` view: in_focus/since_holiday rows as described; a bare subquery item and a member name inside the subquery fail as the page says.
- LOD across many-to-many error text matches the build.
- Function surface unchanged (every registered name is in functions.rst).
- Terminology, American spelling, no tracker IDs, code-block languages set, no stacked admonitions.
