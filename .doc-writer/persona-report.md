# Persona Report: Data engineers exploring semantic views

**Generated:** 2026-10-16 (v0.15.0 content refresh; scenarios reused from `.doc-writer/scenarios.yaml`)
**Scope:** scenarios that touch the refreshed pages. S2-S5 exercise pages this release did not change and were not re-run.

| Scenario | Verdict | Notes |
|----------|---------|-------|
| S1 install, first view, three queries | PASS | Prerequisites now say DuckDB 2.0; the install step says 1.5 users stay on v0.14.0 and links the load-error entry, which shows the exact message, its cause and the `FORCE INSTALL` fix. |
| S6 discover functions over SQL | PASS | The `categories` query on `reference/functions.rst` was run against the DuckDB 2.0 build and returns the documented 18 rows; the page now explains why `extension_name` cannot be used. |

## Observations (not fixed; for the project author)

- The docs assume DuckDB 2.0 is installable. GA is 2026-10-21; before then only pre-release
  DuckDB 2.0 builds exist, the community repository has no build for them, and the load-guard
  hint for such hosts reads "Load a semantic_views build compiled against this DuckDB." Publish
  these docs with the release, not before.
