# Page Inventory -- v0.20.0 (LOD expressions, direct many-to-many, dimension references, window facts and dimensions)

**Generated:** 2026-11-15
**Mode:** Update: one new page, two new sections, a content refresh
**Trigger:** `git log v0.19.0..origin/main`: #358 (optional relationship names), #362 (dimension references), #363 (DIMENSIONS subqueries), #364 (window facts/dimensions), #366 (direct many-to-many), #367 (USING past a role), #368/#369/#371 (LOD). Source of truth: CHANGELOG `[Unreleased]` and a build of main (DuckDB 2.0.0).

Approval: the inventory was not put to the project author (no interactive session); every entry below was taken as approved, and the "no changes needed" entries as deselected.

Every SQL block on a new or changed page was run against the build with the DuckDB Python package (scratch scripts outside the repo); output tables are the real outputs.

## New Pages and Sections

| # | Type | Title | Key Sections | File Path |
|---|------|-------|--------------|-----------|
| 1 | how-to | How to Reuse a Dimension of a Related Table | Set up; declare; query; what is refused; related | docs/how-to/dimension-references.rst |
| 2 | how-to (section) | Relate Two Tables on a Non-Key Column | Data, view, grouped, filtered and dims-only queries; keyless metric refusal; format 4 | docs/how-to/many-to-many-relationships.rst |
| 3 | how-to (section) | Rank or Total Each Row with a Window Fact or Dimension | Rank, category total, filter keeps every row in the window, metric over a window fact | docs/how-to/window-metrics.rst |

## Content Refresh

| # | Type | Title | Discrepancies | File Path |
|---|------|-------|---------------|-----------|
| 4 | (refresh) | CREATE SEMANTIC VIEW | Format-4 notes; ASOF key rule | docs/reference/create-semantic-view.rst |
| 5 | (refresh) | ASOF and Range Relationships | ASOF key rule | docs/how-to/asof-and-range-relationships.rst |
| 6 | (refresh) | Error Messages | LOD INCLUDE text | docs/reference/error-messages.rst |
| 7 | (refresh) | Snowflake Comparison | LOD row names all three forms | docs/explanation/snowflake-comparison.rst |
| 8 | (refresh) | How-to index, home page, navbar | Entries for the new page and sections; LOD navbar entry | docs/how-to/index.rst, docs/index.rst, docs/conf.py |

## Pages Verified -- No Changes Needed

| Title | Reason | File Path |
|-------|--------|-----------|
| LOD Expressions | Landed with #368/#369; INCLUDE/EXCLUDE outputs current | docs/how-to/lod-expressions.rst |
| SEMANTIC_VIEW(...) | DIMENSIONS subquery example and claims run against the build | docs/reference/semantic-view-function.rst |
| Functions | Function surface unchanged | docs/reference/functions.rst |
| Release notes | Includes CHANGELOG.md verbatim | docs/changelog.md |

## API Reference Status

Manual SQL reference (`api_reference: manual`). New anchors: `howto-dimension-references` (+ sections), `howto-m2m-direct`, `howto-window-members`.

## Audience Targeting

Single persona: Data engineers exploring semantic views (intermediate).

## Coverage Gaps

None left in docs/. README.md and CHANGELOG.md were refreshed in the same pass.
