---
name: verify
description: Prove a change to duckdb-semantic-views works end to end. Use before saying a change is done, before committing, and before opening or updating a pull request.
---

# Verify a change

Run from the repository root:

```bash
just build > /tmp/claude/build.log 2>&1; echo "build rc=$?"
just test-all > /tmp/claude/test-all.log 2>&1; echo "test-all rc=$?"
tail -100 /tmp/claude/test-all.log
```

It passes when both report rc=0. `just test-all` runs the Rust unit and property tests,
every sqllogictest in `test/sql/TEST_LIST`, the DuckLake tests and all Python
integration suites, including the Snowflake parity replay. Before pushing to main run
`just ci` the same way, which adds clippy (pedantic), fmt, cargo-deny, the TEST_LIST and
fuzz-list checks and the docs check. The first engine build takes about 20 minutes, so
run it in the background.

Take each command's exit status from the command itself. In a pipeline `$?` is the last
command's status, so `cmd | tail` hides a failure. Redirect long output to a file, then
read the file.

## What this covers

- the extension-load, DDL and query path, which `cargo test` alone does not reach
- the differential proptests against their independent oracles
- every recorded Snowflake fixture, replayed statement by statement

## What it does not cover

- Windows and the other registry platforms, which only BuildAll on main runs (a
  Windows-only stack overflow shipped in v0.18.0)
- long fuzz runs: `just fuzz <target>` when a change touches a parser or scanner
- Snowflake itself: new semantics need a fixture run on Snowflake, recorded under
  `test/snowflake_parity/recorded/`

## When it fails

- Read the first failure and fix its cause. Do not rerun hoping for a green, and do not
  skip, loosen or delete the failing test to get one.
- If the failure is real but outside the change, say so in the PR and in your report
  rather than working around it.
- The sqllogictest runner stops a file at its first failure. If you added several cases, confirm each one, as CLAUDE.md describes.
