# Gap Report

**Generated:** 2026-11-12, branch `claude/project-thread-txbnay` (origin/main `17a4c03`, v0.19.0 release prep) against `v0.18.0`
**Source root:** src/ (plus cpp/src/shim.cpp)
**Language:** rust
**Total undocumented symbols:** 0 user-facing
**Potentially stale pages:** 10 (scoped to this release's changes; see below)

Previous run (v0.17.0): 0 undocumented symbols. Nothing persists from it.

## Undocumented Symbols

None. The SQL functions registered by `LOAD`ing the release build into the DuckDB 2.0.0 Python
package are the v0.18.0 set; every name is in `docs/reference/functions.rst`, with the three `__sv_*`
helpers listed as internal.

The release changes syntax and query behaviour, not the function surface:

- #336: `ASOF` relationships (`o(cust_id, order_date) REFERENCES a(cust_id, ASOF start_date)`).
  In the CREATE reference and the error reference; no how-to.
- #338: range relationships (`REFERENCES p(BETWEEN start AND end EXCLUSIVE)`) with a
  `CONSTRAINT ... DISTINCT RANGE` on the target. In the CREATE, DESCRIBE, GET_DDL and YAML
  references; no how-to.
- #339, #343: many-to-many relationships through a bridge table. In the CREATE reference only;
  fan-traps.rst pointed to the reference for the worked example.
- #340-#352: fan-outs answered once per group, any acyclic graph at CREATE, diamond and role
  rules, dims-only meeting tables, SHOW FOR METRIC, window metrics over the query, semi-additive
  snapshots along USING, a filter read along each role, window rules at CREATE, a bare column
  read from its member's own table. In the CHANGELOG; several pages still described the 0.18
  refusals.

## Potentially Stale Pages

The timestamp heuristic flags every page. Narrowed by grepping docs/ for `refused`, `fan trap`,
`role-play`, `USING`, `window metric`, `FACTS`, `declared first` and `bridge`, then checking each
claim against the release build:

- `docs/how-to/fan-traps.rst` (refusal list: window metrics, role-played tables, bridges)
- `docs/explanation/metric-grain.rst` (FACTS reads the first-declared table; "What Is Still Refused")
- `docs/how-to/role-playing-dimensions.rst` (a metric on the role-played table is refused; base-table exception)
- `docs/explanation/snowflake-comparison.rst` (window metric below grain refused; no ASOF / range / bridge rows)
- `docs/reference/semantic-view-function.rst` (a WHERE member on a role-played table is rejected)
- `docs/how-to/filtering.rst` (role-played filter troubleshooting)
- `docs/how-to/index.rst`, `docs/index.rst`, `docs/conf.py` (no entries for the new how-tos)
- `docs/reference/create-semantic-view.rst` (no links to worked examples)

Outside docs/: README.md (fan-trap paragraph says the error is raised; no ASOF / range / bridge),
CHANGELOG.md `[Unreleased]` (window metric listed as refused in three bullets).
