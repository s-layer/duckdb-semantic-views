# Editor Report

**Generated:** 2026-10-16 (v0.15.0 content refresh, DuckDB 2.0)
**Files reviewed:** 7 changed pages; terminology pass over all docs/ pages
**Changes made:** 3
  - BLOCKING: 0
  - SUGGESTION: 2
  - NITPICK: 1

## Summary

Seven pages were refreshed for the move to DuckDB 2.0. Every new factual claim was checked
against the source (`src/load_guard.rs`, `cpp/include/parser_extension_compat.hpp`,
`test/sql/peg_compat.test`, `test/sql/*quoted*`) and by running the built v2.0.0-alpha47600
extension in the matching DuckDB 2.0 Python package (error texts, `extension_name` being NULL,
`allow_parser_override_extension` behaviour) and in DuckDB 1.5.6 (load-guard message).
`just docs-check` (`sphinx-build -W`) passes.

---

## docs/reference/error-messages.rst

### SUGGESTION

| Section | Description | Fix |
|---------|-------------|-----|
| Query time limit exceeded | Draft listed `explain_semantic_view()` as subject to the time limit; it only expands SQL and runs no query, so the claim was unverified. | Removed; the entry names `semantic_view()` and the type-reporting SHOW / DESCRIBE commands. |
| Extension built for a different DuckDB | Draft said `FORCE INSTALL` on DuckDB 1.5 "installs v0.14.0"; builds are per exact DuckDB release, so that only holds for the release v0.14.0 was built for. | Reworded to "works once a build for your DuckDB release has been published; DuckDB 1.5 gets no release newer than v0.14.0", matching the load-guard hint's own hedge. |

## docs/tutorials/getting-started.rst

### NITPICK

| Section | Description | Fix |
|---------|-------------|-----|
| Install the Extension | Contraction ("don't") in a warm-businesslike page. | "do not". |

## docs/explanation/transactional-ddl-and-limitations.rst, docs/explanation/index.rst, docs/reference/functions.rst, docs/reference/semantic-view-function.rst, README.md

No findings. Diataxis types unchanged (tutorial admonition is a prerequisite note, the
explanation section explains why the setting matters, reference additions are syntax + example).
No em dashes, AI vocabulary, or hedging in new prose. All new `:ref:` targets resolve
(`ref-err-load-version`, `ref-err-quoted-identifier`, `ref-err-time-limit`, `ref-sv-time-limits`,
plus existing SHOW / DESCRIBE labels).

---

## Terminology Changes

| Term | Before | After | Authority |
|------|--------|-------|-----------|
| PEG parser caveat | "DuckDB's Experimental PEG Parser" / "the PEG parser" | "The Parser Override Setting" / "the parser override setting" | DuckDB 2.0 has no PEG toggle; the caveat is now the `allow_parser_override_extension` setting (label `explanation-txn-ddl-peg` kept so links survive) |
| DuckDB version | unstated / "DuckDB 1.5" | "DuckDB 2.0" (never the alpha build id) | CHANGELOG [Unreleased], `.duckdb-version` |
