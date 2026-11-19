# Review rules

What a reviewer (the review bot or a person) should look for in a pull request here.
CLAUDE.md says how to work in this repository; this file says what to check in a diff.

## Severity

Tag every finding with one of these:

- **Important**: blocks the merge. A wrong result, a crash, a security problem, a test
  that would pass on broken code, or a break of one of the checks below.
- **Nit**: worth fixing but does not block. Naming, clarity, a small missing test, docs.
- **Pre-existing**: a problem the diff did not cause but sits next to it. Label it so it
  is not mistaken for a regression. It does not block this PR.

Lead with the Important findings. If there are none, say so in one line.

## What to look for

- Wrong numbers first. A query that returns a plausible but wrong result (a fan-out
  counted twice, the wrong role of a role-playing table, a filter applied at the wrong
  grain) is the worst bug this extension can have, worse than an error.
- Snowflake is the model for what a semantic view means: the DDL clauses, what a metric,
  dimension, fact or relationship is, grain and fan-trap behaviour, and what SHOW and
  DESCRIBE report. DuckDB wins on dialect matters: identifier case and quoting
  (`ident::ident_matches`), name resolution, literals, types, NULL ordering. A change
  that imports a Snowflake dialect rule over a DuckDB convention is a finding.
- Unsafe and FFI code in the `extension` feature: lifetimes across the C++ shim, panics
  that can cross the FFI boundary, and stack depth on recursive paths (the v0.18.0
  Windows build overflowed its 1 MB stack).
- Error messages a user will read: they name the view, the member and what to do.

## Diff-triggered checks

Apply a check only when its trigger matches the diff. Each one comes from a miss that
already happened, named under "Source".

- **When the diff touches AGENTS.md, CLAUDE.md, REVIEW.md or anything under `.claude/`**
  Check: Important. List every rule the change adds, loosens or removes in the review, and
  review the rest of the diff against the base branch's copy of these files, not the PR's.
  A list in the PR description does not clear it, since the PR writes both the list and
  the rules; the review bot never clears it. It is cleared only by a maintainer who has
  read the list and approves or merges the PR.
  Source: the review bot reads the root AGENTS.md, CLAUDE.md and REVIEW.md from the PR's
  own checkout (checked 2026-11-19), so a PR can rewrite the rules it is reviewed against.
  `.claude/` is included as a precaution.
- **When the diff makes a new table reachable (join emission, reference inlining,
  `source_tables`), accepts new syntax, or opens a new ingress path**
  Check: the PR description lists each existing guard that assumed the old, smaller set
  and says for each one: extended, or why not applicable. The guards are the fan-trap
  checks, the role-playing ambiguity check, the scanner set and the validation choke
  points. A missing list is Important.
  Source: EXP-27 (silent 2x) and EXP-31 (silent wrong-role binding) after #207 taught
  `where_clause` members to reach other tables without telling
  `check_where_clause_fan_traps` or `check_where_clause_role_playing_path`; PARSE-12 after
  PARSE-7 taught the scanners escape strings without telling `scan_chains`.
- **When the diff changes what number a query returns (a new query parameter, aggregation
  shape, join or CTE topology)**
  Check: at least one differential proptest in `tests/` generates the new construct and
  its oracle mirrors it. A new field pinned to its inert default in every generator (for
  example `where_clause: None`) is not coverage and is Important.
  Source: EXP-9 and EXP-10, two silent wrong-number bugs that reached main because every
  harness pinned `where_clause: None`.
- **When the diff fixes a bug**
  Check: a test that reproduces the bug and fails without the fix (a `#[cfg(test)]` /
  `tests_*.rs` unit test for logic, a sqllogictest for the load, DDL and query path). When
  one fix has several cases, each case needs its own observed failure, not just the run.
  Source: the failing-test-first rule in duckdb-semantic-views' and semolina's CLAUDE.md,
  which applies to every s-layer repo.
- **When the diff changes behaviour that a Snowflake fixture recorded**
  Check: the parity replay (`test/integration/test_snowflake_parity.py`) still agrees, and
  any new entry in `test/snowflake_parity/approved_differences.toml` gives the reason the
  difference is expected (a DuckDB dialect rule, not a semantic disagreement).
  Source: the 2026-11 replay of 15 recorded fixtures found two parity bugs that the
  hand-copied `.test` cases had missed.
- **When the diff ships less than the full behaviour on purpose (a clause accepted but
  narrowed, an error where a fuller version would compute)**
  Check: a TECH-DEBT.md entry in the same change says what is missing and what would
  finish it, and the test comment points at that entry.
  Source: `SHOW SEMANTIC DIMENSIONS/METRICS/FACTS` returned an empty `data_type` from
  v0.10.0 because the follow-up lived only in a test comment.
- **When the diff adds a `.test` file or a `statement error` case**
  Check: the file is in `test/sql/TEST_LIST`, and errors use the block form (`----` then
  the expected substring).
  Source: the sqllogictest runner silently skips unlisted files and does not support the
  inline regex form.
- **When the diff adds, renames or removes a module, workflow, `just` recipe, fuzz target
  or DuckDB version-pin site**
  Check: MAINTAINER.md (and TECH-DEBT.md where relevant) is updated in the same PR.
  Source: the MAINTAINER.md sync rule in CLAUDE.md.
- **When the diff edits CHANGELOG.md**
  Check: Keep a Changelog 1.1.0 subheadings only, an `[Unreleased]` section on top, and no
  internal phase vocabulary, since the file is the docs' release notes.
  Source: the milestone completion rules in CLAUDE.md.
- **When the diff adds or changes a test**
  Check: the assertions would fail if the behaviour were wrong. Flag `len(x) > 0`, a
  substring check standing in for a result, an error test that does not check the message,
  fixtures where different answers produce the same number, and checks on a field the
  fixture or generator never varies.
  Source: the 2026-11 test review, which found each of these in s-layer repos
  (`pytest.raises(Exception)` in arrowmodel, a one-row fixture in dbt-s-layer where
  revenue, paid revenue and average order value were all 10).
- **When the diff deletes, skips or loosens a test, or narrows what a generator produces**
  Check: Important unless the PR says why. A refactor carries its tests with it.
  Source: the coverage-forward refactoring rule (duckdb-semantic-views CLAUDE.md), which
  applies to every s-layer repo.
- **When the diff adds a retry, a rerun, a longer timeout or a sleep to make a flaky test
  pass**
  Check: Important. A flaky test is fixed, or deleted with the reason written down. It is
  never rerun into green. A timeout raised because a job really got slower is not this.
  Source: a maintainer rule for all s-layer repos, set 2026-10-19 after a flaky failure
  was rerun instead of fixed: never re-run CI hoping for a lucky green.
- **When the diff changes a `uses:` line under `.github/workflows/`**
  Check: `.github/workflows/actions.lock` changes in the same PR, and an action is
  referenced by a release tag, not a branch or a commit SHA. Local workflows (`uses:
  ./...`) carry no ref, and the reusable review-bot workflow
  (`anentropic/github-reviewbot/...@main`) and DuckDB's `extension-ci-tools` workflows run
  at a branch on purpose; none of these is a finding.
  Source: the GitHub Actions lockfile section in CLAUDE.md; a branch ref
  (`dtolnay/rust-toolchain@stable`) broke every job's setup in arrowmodel when the branch
  was force-pushed, and Dependabot only bumps tags.
