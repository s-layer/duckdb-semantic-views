# Page Inventory -- v0.19.0 (ASOF and range relationships, many-to-many through a bridge, Snowflake-parity query rules)

**Generated:** 2026-11-12
**Mode:** Update: two new pages plus a content refresh
**Trigger:** `git log v0.18.0..origin/main`: #336 (ASOF), #338 (range), #339 (bridge), #340-#352 (fan-outs, diamonds, roles, dims-only, SHOW FOR METRIC, window and semi-additive metrics, FACTS table). Source of truth: CHANGELOG `[Unreleased]`, the release build (`extension_version` f19aef0, DuckDB 2.0.0), Snowflake fixture results for ASOF and range.

Approval: the inventory was not put to the project author (no interactive session); every entry below was taken as approved, and the "no changes needed" entries as deselected.

Every SQL block on a new or changed page was run against the release build with the DuckDB Python package (scratch scripts outside the repo); output tables are the real outputs.

## New Pages

| # | Type | Title | Key Sections | File Path |
|---|------|-------|--------------|-----------|
| 1 | how-to | How to Join on Dates with ASOF and Range Relationships | Choose the relationship; ASOF (addresses in effect at an order's date); range with `DISTINCT RANGE` (campaign running at a signup's date); what you can group by; inspect (explain, DESCRIBE, GET_DDL, YAML); troubleshooting | docs/how-to/asof-and-range-relationships.rst |
| 2 | how-to | How to Model Many-to-Many Relationships with a Bridge | Set up; define the bridge; group and filter across it; bridge dimensions; dimensions only; a fact table as the bridge; verify; limits; troubleshooting | docs/how-to/many-to-many-relationships.rst |

## Content Refresh

| # | Type | Title | Discrepancies | File Path |
|---|------|-------|---------------|-----------|
| 3 | (refresh) | How to Diagnose and Fix Fan Traps | Refusal list rewritten for 0.19.0; bridge links to the new how-to | docs/how-to/fan-traps.rst |
| 4 | (refresh) | Metric Grain | FACTS reads the member's table and lists every row; "What Is Still Refused" rewritten | docs/explanation/metric-grain.rst |
| 5 | (refresh) | Role-Playing Dimensions | A metric on the role-played table is answered by its own dimensions; troubleshooting entry rewritten | docs/how-to/role-playing-dimensions.rst |
| 6 | (refresh) | Snowflake Comparison | Concept Mapping rows for ASOF / range and many-to-many; window and role-played statements | docs/explanation/snowflake-comparison.rst |
| 7 | (refresh) | SEMANTIC_VIEW(...) | A WHERE member on a role-played table follows each metric's role | docs/reference/semantic-view-function.rst |
| 8 | (refresh) | Filtering | Role-played filter troubleshooting | docs/how-to/filtering.rst |
| 9 | (refresh) | CREATE SEMANTIC VIEW | Links to the worked examples | docs/reference/create-semantic-view.rst |
| 10 | (refresh) | How-to index, home page, navbar | Entries for the two new pages | docs/how-to/index.rst, docs/index.rst, docs/conf.py |

## Pages Verified -- No Changes Needed

| Title | Reason | File Path |
|-------|--------|-----------|
| Error Messages | ASOF, range, multi-path and fan trap entries landed with the PRs | docs/reference/error-messages.rst |
| SHOW SEMANTIC DIMENSIONS FOR METRIC | Fanned dimensions listed (#346) | docs/reference/show-semantic-dimensions-for-metric.rst |
| Window Metrics, Semi-Additive Metrics | Updated with #341, #348, #349, #351 | docs/how-to/window-metrics.rst, docs/how-to/semi-additive-metrics.rst |
| Databricks Comparison | No statement touched by this release | docs/explanation/databricks-comparison.rst |
| Functions | Function surface unchanged | docs/reference/functions.rst |
| Release notes | Includes CHANGELOG.md verbatim | docs/changelog.md |

## API Reference Status

Manual SQL reference (`api_reference: manual`). New anchors: `howto-asof-range` (+ `-choose`, `-grain`, `-inspect`, `-troubleshooting`, `-related`), `howto-asof`, `howto-range`, `howto-many-to-many`, `howto-m2m-*` (setup, define, group, filter, bridge-dims, dims-only, fact-bridge, verify, limits, troubleshooting, related).

## Audience Targeting

Single persona: Data engineers exploring semantic views (intermediate).

## Coverage Gaps

None left in docs/. README.md and CHANGELOG.md were refreshed in the same pass.
