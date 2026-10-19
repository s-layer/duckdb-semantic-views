# Gap Detection Report

**Source root:** src/
**Language:** rust
**Generated:** 2026-10-16, worktree on `59589d4` (v0.15.0 release candidate, DuckDB 2.0 build) against `origin/main`
**Total exported symbols:** Rust `pub` items (internal; see previous report) + 18 registered SQL functions
**Undocumented symbols:** 0 user-facing (the internal `__sv_compute_create_from_yaml` remains correctly undocumented)
**Potentially stale pages:** 7 (scoped to this release's changes; see below)

## Undocumented Symbols

None new. The registered SQL functions were listed from `duckdb_functions()` after `LOAD`ing
the v2.0.0-alpha47600 build into the matching DuckDB 2.0 Python package: the same 18 functions
as v0.14.0 (17 documented, 1 internal). No DDL statement or `semantic_view()` parameter was added.

The release changes behaviour, not surface:

- built for, and loadable only in, DuckDB 2.0 (DuckDB 1.5 users stay on v0.14.0)
- `SET max_execution_time` now stops `semantic_view()` and the SHOW / DESCRIBE commands that
  probe member types (`INTERRUPT Error: Query exceeded maximum execution time`)
- unterminated / zero-length quoted identifiers in semantic-view DDL are rejected by DuckDB 2.0's
  tokenizer (`unterminated quoted identifier`, `zero-length delimited identifier`)
- the load-time version check's message: `semantic_views was built for DuckDB X and cannot run in DuckDB Y ...`

Side effects of the DuckDB 2.0 move that the docs also describe:

- `enable_peg_parser()` / `disable_peg_parser()` no longer exist (2.0's parser is the PEG parser)
- `duckdb_functions()` now has an `extension_name` column, but it is NULL for this extension's functions
- arrow lambdas (`x -> ...`) are deprecated in favour of `lambda x: ...` (no doc page uses one)

## Potentially Stale Pages

The timestamp heuristic flags every page (src/ changed after all of them). Narrowed to pages whose
content touches the release's changes, by grepping docs/ and README.md for DuckDB versions,
the PEG parser, quoted-identifier errors, loading, and time limits:

- `docs/tutorials/getting-started.rst` (install step states no DuckDB version requirement)
- `docs/reference/functions.rst` ("In DuckDB 1.5, duckdb_functions() has no column ...")
- `docs/explanation/transactional-ddl-and-limitations.rst` (PEG-parser section built on `disable_peg_parser()`)
- `docs/explanation/index.rst` (abstract mentions "the PEG parser" caveat)
- `docs/reference/error-messages.rst` (no load-version or time-limit entries; quoted-identifier errors now DuckDB's)
- `docs/reference/semantic-view-function.rst` (no mention of query time limits)
- `README.md` (quick start states no DuckDB version; Building omits CMake)

No other page states a DuckDB 1.x version, pins `duckdb==1.5…`, or contains a version matrix.
