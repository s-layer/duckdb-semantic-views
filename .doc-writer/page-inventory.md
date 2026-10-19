# Page Inventory -- v0.15.0 Content Refresh (DuckDB 2.0)

**Generated:** 2026-10-16
**Mode:** Update, content refresh only (no new pages; gap scan found no new user-facing symbols)
**Trigger:** v0.15.0 moves the extension to DuckDB 2.0. Source of truth: `git diff origin/main` and CHANGELOG `[Unreleased]`.

User-facing docs say "DuckDB 2.0", never the pre-release build id (`v2.0.0-alpha47600`); the pins move to GA `v2.0.0` on 2026-10-21.

## Content Refresh

| # | Type | Title | Key Sections / Discrepancies | File Path |
|---|------|-------|------------------------------|-----------|
| 1 | (refresh) | Getting Started | Prerequisites and Install step: state that the extension needs DuckDB 2.0, that DuckDB 1.5 users stay on v0.14.0, and what a mismatched `LOAD` reports (link to the new load-error entry). | docs/tutorials/getting-started.rst |
| 2 | (refresh) | Functions | "Discovering Functions over SQL": the "In DuckDB 1.5, duckdb_functions() has no column that names the extension" sentence. DuckDB 2.0 has `extension_name`, but it is NULL for this extension's functions; filtering on `categories` is still the way. | docs/reference/functions.rst |
| 3 | (refresh) | Transactional DDL and Known Limitations | PEG-parser section (`explanation-txn-ddl-peg`): `enable_peg_parser()` / `disable_peg_parser()` are gone in 2.0. Rewrite around the remaining caveat (setting `allow_parser_override_extension` back to `DEFAULT` turns semantic-view DDL off until `FALLBACK` is restored). Update intro sentence and summary bullet. Keep the label. | docs/explanation/transactional-ddl-and-limitations.rst |
| 4 | (refresh) | Explanation index | Abstract for the transactional page names "the PEG parser"; align with #3. | docs/explanation/index.rst |
| 5 | (refresh) | Error Messages | Add a "Loading Errors" section (version-check message, cause, fix). Add a DDL entry for the two quoted-identifier errors now raised by DuckDB. Add a query entry for `Query exceeded maximum execution time`. | docs/reference/error-messages.rst |
| 6 | (refresh) | semantic_view() | Add a short "Time Limits" section: `SET max_execution_time` applies to `semantic_view()` (and the type-probing SHOW / DESCRIBE commands). | docs/reference/semantic-view-function.rst |
| 7 | (refresh) | README | Quick start: one-line DuckDB 2.0 requirement. Building: add CMake (the engine is built from source). | README.md |

## Pages Verified -- No Changes Needed (pre-deselected)

| Type | Title | Reason | File Path |
|------|-------|--------|-----------|
| (refresh) -- no changes needed | All other docs/ pages | No DuckDB version statements, no PEG-parser toggles, no arrow-lambda SQL, no quoted-identifier error text. Behaviour they describe is unchanged by the engine move. | docs/**/*.rst |
| (refresh) -- no changes needed | Release notes | Includes CHANGELOG.md verbatim; out of scope. | docs/changelog.md |

## API Reference Status

Manual SQL reference under `docs/reference/` (config `api_reference: manual`). No new symbols; new prose links existing `:ref:` labels.

## Audience Targeting

Single persona: Data engineers exploring semantic views (intermediate). They know DuckDB basics, so "DuckDB 2.0", `SET`, and `LOAD` need no explanation; the version-lock behaviour (one exact DuckDB release) does.

## Coverage Gaps

None introduced. Interrupt behaviour (Ctrl-C, `con.interrupt()`) was never documented; this refresh documents only the 2.0 time limit and does not open a cancellation page.
