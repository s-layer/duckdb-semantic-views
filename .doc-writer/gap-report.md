# Gap Report

**Generated:** 2026-11-15, branch `claude/project-thread-xyewi7` (origin/main `bd5fa6e`, v0.20.0 release prep) against `v0.19.0`
**Source root:** src/ (plus cpp/src/shim.cpp)
**Language:** rust
**Total undocumented symbols:** 0 user-facing
**Potentially stale pages:** 9 (scoped to this release's changes; see below)

Previous run (v0.19.0): 0 undocumented symbols. Nothing persists from it.

## Undocumented Symbols

None. The SQL functions registered by `LOAD`ing the build into the DuckDB 2.0.0 Python package
are the v0.19.0 set; every name is in `docs/reference/functions.rst`, with the three `__sv_*`
helpers listed as internal.

The release changes syntax and query behaviour, not the function surface:

- #358: optional relationship names (`SYS_RELATIONSHIP_<id>`). In the CREATE reference; README unmentioned.
- #362: cross-table dimension references. In the CREATE reference and error reference; no how-to.
- #363: uncorrelated subqueries in DIMENSIONS items. In the SEMANTIC_VIEW reference.
- #364: window functions in facts and dimensions. In the CREATE reference; no how-to.
- #366: direct many-to-many relationships. In the CREATE reference; the many-to-many how-to covered bridges only.
- #367: USING past a role-played table in any declaration order. Behaviour fix, in the CHANGELOG.
- #368, #369, #371: LOD expressions (FIXED, INCLUDE, EXCLUDE). How-to page landed with #368/#369; navbar entry missing.

## Potentially Stale Pages

Narrowed by grepping docs/ for `format 4`, `0.19.0 and older`, `many-to-many`, `PRIMARY KEY`,
`FIXED DIMENSIONS` and `window`, then checking each claim against the build:

- `docs/reference/create-semantic-view.rst` (format-4 notes; ASOF key rule)
- `docs/how-to/asof-and-range-relationships.rst` (ASOF key rule)
- `docs/how-to/many-to-many-relationships.rst` (bridge only)
- `docs/how-to/window-metrics.rst` (window members missing)
- `docs/reference/error-messages.rst` (LOD INCLUDE text)
- `docs/explanation/snowflake-comparison.rst` (LOD row)
- `docs/how-to/index.rst`, `docs/index.rst`, `docs/conf.py` (no entries for the new topics)

Outside docs/: README.md (no Phase 8 features), CHANGELOG.md `[Unreleased]` (format-4 sentence).
