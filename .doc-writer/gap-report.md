# Gap Report

**Generated:** 2026-10-26, worktree on `c9a767b` (v0.17.0 release prep) against `v0.16.0`
**Source root:** src/ (plus cpp/src/shim.cpp, which holds the standard-SQL rewrite and its refusals)
**Language:** rust
**Total undocumented symbols:** 0 user-facing
**Potentially stale pages:** 14 (scoped to this release's changes; see below)

Previous run (v0.16.0): 0 undocumented symbols. Nothing persists from it.

## Undocumented Symbols

None. The registered SQL functions, listed from `duckdb_functions()` after `LOAD`ing the
debug build into the DuckDB 2.0.0 Python package, are the v0.16.0 set plus one internal
helper, `__sv_check_create` (used by `CREATE` to bind each member). `docs/reference/functions.rst`
already lists it as internal and not part of the supported interface.

The release changes behaviour and syntax, not the function surface:

- #314: a semantic view can be queried by name with standard SQL
  (`SELECT region, AGG(revenue) FROM sv WHERE ... GROUP BY ... HAVING ...`). Documented in
  `docs/how-to/standard-sql.rst`; refusal messages (`must be passed to AGG`, `must appear in
  GROUP BY`, `SELECT * is not supported ...`, `cannot be joined, pivoted or read alongside other
  tables`, window / QUALIFY / sampling / grouping sets / correlated references) are not in the
  error reference.
- #315: `CREATE SEMANTIC VIEW` binds every member and refuses one DuckDB cannot bind
  (`semantic view '<view>': <kind> '<name>' does not bind: <DuckDB error>`). Documented in the
  CREATE reference and the error reference; several troubleshooting entries elsewhere still
  describe the old accept-then-fail-at-query behaviour.
- #316: member-level `[WITH] TAG (...)`. Documented in the CREATE / DESCRIBE / YAML references,
  the metadata-annotations how-to and both comparison pages.

## Potentially Stale Pages

The timestamp heuristic flags every page (src/ changed after all of them). Narrowed by grepping
docs/ for the query forms, `direct SQL`, `Not planned`, and claims about when a member's error
appears (`query time`, `when queried`, `CREATE succeeds`, `does not check`, `binder error`),
then checking each claim against the debug build:

- `docs/explanation/snowflake-comparison.rst` (Feature Parity row "Direct SQL query interface: Not planned")
- `docs/explanation/semantic-views-vs-regular-views.rst` (query interface row shows only the construct)
- `docs/index.rst` (intro and meta description name only the construct)
- `docs/tutorials/getting-started.rst` (teaches only the construct and the function)
- `docs/tutorials/multi-table.rst` (CREATE OR REPLACE step says nothing of the bind check)
- `docs/reference/semantic-view-function.rst` (intro lists two spellings, not the standard-SQL form)
- `docs/reference/error-messages.rst` (no standard-SQL refusal entries; Query Errors intro)
- `docs/how-to/standard-sql.rst` (no example data: none of its examples run as written)
- `docs/how-to/filtering.rst` (no pointer to WHERE / HAVING in the standard-SQL form)
- `docs/how-to/facts.rst` (two troubleshooting entries say CREATE succeeds and the query fails)
- `docs/how-to/derived-metrics.rst` ("a table-qualified column ... is not caught at define time")
- `docs/how-to/window-metrics.rst` ("CREATE does not catch this" for a window metric over a window metric)
- `docs/how-to/data-sources.rst` ("CREATE SEMANTIC VIEW does not check that the tables in TABLES exist")
- `docs/how-to/metadata-annotations.rst` (BOOLEAN note reasons that typing needs a binder CREATE lacks)
