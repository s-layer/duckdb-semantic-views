# DuckDB Semantic Views — Project Instructions

## Snowflake is the model for *semantics*, DuckDB for *conventions*

When in doubt about what a semantic view **means**, follow Snowflake: the DDL
surface and clause set, what a metric / dimension / fact / relationship is, grain
and fan-trap behaviour, what each `SHOW` / `DESCRIBE` command reports. That is the
feature being built, and Snowflake is its reference implementation.

But Snowflake's *host-language* rules are Snowflake's, not ours. Where the two
disagree on something that belongs to the surrounding SQL dialect rather than to
semantic views, **DuckDB wins**:

- **Identifier quoting and case-sensitivity.** DuckDB matches identifiers
  case-insensitively whether or not they are quoted; Snowflake folds unquoted
  names to upper case and treats quoted ones as case-sensitive. We follow DuckDB
  — see `ident::ident_matches`, and TECH-DEBT #25/#28 for the sites migrated onto
  it.
- **Name resolution.** Schema/search-path semantics follow DuckDB's rules, not
  Snowflake's current-schema-only rule.
- **Anything else that is a property of the dialect** rather than of semantic
  views — literal syntax, type names and coercion, `NULL` ordering, collation.

So "Snowflake does X" settles a question about semantic-view behaviour, and does
**not** settle a question about identifiers, resolution or dialect. When a
Snowflake behaviour can only be reproduced by breaking a DuckDB convention, keep
the convention and record the divergence in TECH-DEBT rather than importing the
Snowflake rule wholesale.

## Quality Gate

**All phases must pass the full test suite before verification can be marked complete.**

The verification command is:

```bash
just test-all
```

This runs: Rust unit tests, property-based tests, SQL logic tests (sqllogictest), and DuckLake CI tests.

Individual test commands:
- `cargo test` — Rust unit + proptest + doc tests
- `just test-sql` — SQL logic tests via sqllogictest runner (requires `just build` first)
- `just test-ducklake-ci` — DuckLake integration tests

A phase verification that only runs `cargo test` is **incomplete** — sqllogictest covers integration paths that Rust tests do not (e.g., type dispatch through the full extension load → DDL → query pipeline).

**Before pushing to main**, run the full CI mirror:

```bash
just ci
```

This adds linting (clippy pedantic + fmt + cargo-deny) and fuzz target compilation checks on top of `test-all`. The Rust toolchain version is pinned in `rust-toolchain.toml` and bumped automatically via Dependabot.

## Bug Fixing & Refactoring Discipline

**Fix bugs test-first.** Before changing any production code to fix a bug, first
write a test that reproduces the issue and **fails** against the current code
(run it, confirm the red). Only then write the fix, and confirm the same test
goes green. This proves the bug was real, that the fix actually addresses it, and
leaves a permanent regression guard behind. Prefer the layer that most directly
exercises the defect — a `#[cfg(test)]`/`tests_*.rs` unit test for logic, a
sqllogictest for anything on the extension-load → DDL → query path (added to
`test/sql/TEST_LIST`). A "fix" landed without a failing-first test is incomplete.

**Confirm the red for *each* case, not just the run.** The sqllogictest runner stops a file at
its first failing statement, so N new cases in one `.test` file yield exactly one observed red —
the other N-1 are unproven, and any that would have passed anyway (a vacuous test) look
identical. The same halt masks regressions later: a break in the second case stays hidden until
the first is repaired. When one fix needs several cases, either give each its own unit test so
they report independently, or verify the set by reverting the fix and checking every case goes
red. Claiming "confirmed red" for cases you did not individually watch fail is the same false
green as the exit-code trap under "Build/test command rules" below.

**Refactor coverage-forward.** A refactor must never silently reduce test
coverage. Be alert for changes that quietly remove coverage — deleting or
weakening an assertion, gating a test behind a feature/flag that CI doesn't
exercise, dropping a `.test` file from `TEST_LIST`, narrowing a proptest
generator so edge cases no longer occur, or asserting equality on a field the
generator never populates (a vacuous check). When you move or rewrite code,
carry its tests with it and strive to strengthen them; if a change genuinely
must drop a test, say so explicitly and explain why rather than letting it vanish
in a diff.

**A new query-semantics feature must reach the numeric oracles, not just fixed
examples.** Anything that changes what number a query returns — a new query
parameter, a new aggregation shape, a new join or CTE topology — is not
adequately covered by hand-written `.test` rows alone. Hand-picked examples
check the cases you thought of; the differential proptests in `tests/`
(`differential_proptest`, `star_schema_proptest`, `multi_hop_join_proptest`,
`semi_additive_proptest`, `window_metric_proptest`) check the cases you didn't,
against an independently-formulated oracle. Extend at least one of them in the
same change: generate the new construct and mirror it into that harness's oracle
so the comparison stays honest.

Watch specifically for a new parameter left pinned at its inert default in every
generator (`where_clause: None` in all five harnesses is how EXP-9/EXP-10 — two
silent wrong-number bugs — reached `main`). A feature that only ever appears in
fixed examples has no randomized coverage no matter how many `.test` rows it
has, and the field being *present* in a struct literal is not coverage: the
generator has to vary it. If a feature genuinely cannot be oracled, say so in
the PR and record why.

**When a change adds a capability, list the fences it walks past.** Any change
that makes a new table reachable (join emission, reference inlining,
`source_tables`), accepts new syntax, or opens a new ingress path must name — in
the PR description — each existing guard that assumed the old, smaller set, and
say for each: extended, or why not applicable. The guards worth enumerating are
the fan-trap checks, the role-playing ambiguity check, the scanner set, and the
validation choke points.

This is not hypothetical process. It is now the second and third time a fix has
opened a hole in a fence that did not know about the new edge: #207 (EXP-23)
taught `where_clause` members to reach facts on other tables and joined those
tables, but neither `check_where_clause_fan_traps` nor
`check_where_clause_role_playing_path` was told — converting a loud binder error
into a silent 2× (EXP-27) and a silent wrong-role binding (EXP-31). PARSE-7 made
escape strings first-class in the scanners without teaching `scan_chains` that a
chain abutting a quote is a literal introducer (PARSE-12). In each case the fix
was correct and the fence was correct; only the pairing was missed, and a
one-paragraph enumeration in the PR would have caught it more cheaply than the
review round that did.

**A deliberately-degraded interim state needs a TECH-DEBT entry in the same
change.** Sometimes the right call is to ship a partial behaviour and finish it
later — removing an inference pass before its replacement lands, accepting a
clause but narrowing what it does, erroring where a fuller implementation would
compute. That is legitimate. What is not legitimate is letting the *only* record
of the unfinished half be a comment in a test that pins the degraded output.

A passing test asserting the degraded value looks identical to a passing test
asserting correct behaviour: CI stays green, review sees an assertion, and the
promise quietly expires. (`SHOW SEMANTIC DIMENSIONS/METRICS/FACTS` has returned
an empty `data_type` since v0.10.0 for exactly this reason — the follow-up was
recorded only in a `phase39_metadata_storage.test` comment saying "Until Plan 05
lands", and Plan 05 never landed. No amount of extra testing would have caught
it; the test was there and passing.) So when you knowingly ship less than the
full behaviour, open a TECH-DEBT entry in the same change describing what is
missing and what would finish it, and reference that entry from the test comment
rather than the other way round.

## Milestone Completion

At the end of every milestone, before tagging:

1. **Update CHANGELOG.md** — Add a new version section with user-facing feature descriptions. Group related commits into feature-level summaries (don't list individual commits). Use ROADMAP.md phase descriptions and success criteria as the source, not raw git log.
   - **Format**: Keep a Changelog 1.1.0. The only allowed `###` subheadings under a version are `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`. `Known limitations` is also permitted as a final subheading when a release ships with documented constraints. **Do not** introduce ad-hoc subheadings for internal phases ("Phase 62 — ..."), workstreams, or chronology — fold those bullets into the standard categories.
   - **Unreleased section**: keep an `## [Unreleased]` section at the top above the most recent tagged version. Between milestones it can read `_No unreleased changes yet._`; in-flight changes on `main` that aren't yet folded into a milestone version go here. The matching `[Unreleased]: ...compare/<latest-tag>...HEAD` link reference at the bottom must point at the latest tag.
   - **In-version churn**: if a feature was added and reverted within the same unreleased version, do not list it in `Added`. Only list what actually shipped at tag time. Likewise, do not include strikethrough "resolved later in the same version" entries.
   - **Audience**: this file is also rendered verbatim as the docs site Release Notes page (`docs/changelog.md` includes it via MyST). Avoid GSD/phase-internal vocabulary in user-facing bullets; if implementation detail belongs anywhere it's inline within the relevant `Added`/`Changed`/`Fixed` bullet, not as its own subhead.
2. **Add example file** — New Python example under `examples/` demoing the milestone's features.
3. **Bump version** — Update Cargo.toml + description.yml.
4. **Reclaim disk** — Run `just clean-stale` after the milestone tag is pushed. This uses `cargo-sweep` to evict target/ artifacts older than 14 days without invalidating the current build cache, so the next milestone's first build stays incremental. The DuckDB amalgamation produces ~30 GB of cumulative build-script output per toolchain rev across a milestone — left unchecked this fills the disk within a few milestones. Do **not** run `just clean` (full `cargo clean`) at milestone boundaries; the cold-rebuild cost (~10 min for the amalgamation) isn't worth it relative to `clean-stale`.

## Build

- `just build` — debug build (extension binary)
- `cargo test` — runs without the extension feature (in-memory DuckDB)
- `just test-sql` — requires a fresh `just build` to pick up code changes

### The DuckDB engine build

`just build` → `make ensure_engine` (`scripts/ensure_engine.py`) builds the DuckDB
engine the C++ shim links against (DuckDB 2.0 has no amalgamation) from the PyPI
sdist of the pinned release, with CMake, and caches it under `.engine/<id>/`. The
first build of an engine takes ~20 minutes on four cores; afterwards `just build`
only checks the cache. It needs `cmake` (and uses `ninja` when present) and reaches
only PyPI (`pypi.org`, `files.pythonhosted.org`), so it works in sandboxes that block
GitHub. Run long builds in the background and redirect to a file (Rule 1 below).

To lint the `extension`-gated FFI layer without any C++ build at all (no
engine needed), use (see MAINTAINER.md):

```bash
SV_SKIP_CPP_BUILD=1 cargo clippy --no-default-features --features extension -- -D warnings
```

## Build/test command rules (non-negotiable)

These two rules have previously caused multi-hour agent stalls. They apply to every
command in this project's build/test surface: `just build`, `just test-sql`, `just test-all`,
`just ci`, `cargo build`, `cargo test`, `cargo nextest run`, `cargo fmt`, `cargo check`,
`cargo clippy`, `uv run test/integration/*.py`.

**Rule 1 — Never pipe long-running commands to bare `tail -N`.** The macOS pipe buffer fills,
`tail` waits for EOF that never arrives until the producer exits, and the run appears hung for
5-30 minutes. Always redirect to a file first, then tail the file:

```bash
cmd > /tmp/claude/x.log 2>&1
RC=$?
tail -100 /tmp/claude/x.log
```

This applies to ANY command above and any cargo/just/sqllogictest invocation that runs longer
than a few seconds.

The `RC=$?` line is not decoration. In a pipeline `$?` is the **last** command's status, so
`cmd | tail -N; echo $?` reports `tail`'s success and hides a failing gate — `cargo clippy`
exiting 101 and `cargo fmt --check` exiting 1 both read as green that way. A hang is at least
obvious; this failure mode silently produces a false pass you may then report as fact. Capture
the status from the command itself, before anything else runs.

**Rule 2 — Use `dangerouslyDisableSandbox: true` for the listed build/test commands when
needed.** The project's Makefile invokes `mktemp` which writes to `/var/folders/.../T/`
(macOS hardcoded), which the sandbox may block depending on session snapshot. If you see
`mktemp: mkstemp failed ... Operation not permitted`, use the sandbox bypass directly for
that command — no need to ask. The bypass is pre-approved for the build/test command list
above and ONLY those commands.

If a command not on the list needs the bypass, halt and ask first.

## Code editing rules

- Pre-commit hook runs `cargo fmt --check` + the **fast** extension-feature clippy
  (`SV_SKIP_CPP_BUILD=1 cargo clippy --no-default-features --features extension -- -D warnings`,
  == `just lint-fast`), which skips the ~25 MB bundled-DuckDB amalgamation build so committing
  stays fast (seconds warm, ~1 min cold) instead of the ~10 min the default-features clippy costs.
  It lints the same production code; CI's full default-features `cargo clippy` (`just lint`) is the
  authoritative gate. If a commit fails on fmt-check, run `cargo fmt`, re-stage, and retry. Never
  use `--no-verify`.
- New sqllogictest files must be added to `test/sql/TEST_LIST` or the runner will skip them.
- For `statement error` assertions in sqllogictest, use the block form (`---- separator` +
  substring), not inline regex — the runner does not support inline form.
- **Keep MAINTAINER.md in sync.** It documents the `src/` tree layout, the CI workflows and
  their triggers, the Justfile test/fuzz recipes, the fuzz-target list, and the DuckDB
  version-pin locations. Whenever you change any of those — add/rename/move/delete a module,
  add or re-trigger a workflow, rename a `just` recipe, add or remove a fuzz target, bump the
  version-pin plumbing — update the corresponding MAINTAINER.md section in the *same* change.
  A structural change that leaves MAINTAINER.md describing the old layout is incomplete. The
  same applies to any other doc that tracks these specifics (e.g. TECH-DEBT.md for debt items).

## Opening pull requests

When you open a pull request, ask the user whether it needs a review. If they say yes:

1. Mark the PR ready for review, since the review bot skips drafts.
2. Request a review from `anentropic-reviewbot`. Claude then posts inline comments and a summary on the PR.
3. Watch the PR and address the review comments and any CI failures as they come in, until CI is green and no blocking comments are left open.

To get another review after pushing fixes, request one from `anentropic-reviewbot` again.
