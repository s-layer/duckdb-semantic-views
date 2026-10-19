# Epic: DuckDB 2.0 PEG grammar and full Snowflake parity

**Drafted:** 2026-10-12, against `main` at v0.13.0 (DuckDB v1.5.6).
**Revised:** 2026-10-12, after a second-opinion review (Fable). Changes: §2 adds the transactional-DDL, signature, layout and error-message findings; Phase 0 is split and runs in parallel with the spike; #51 moves to read-side Phase 0c; the spike gains a transactional go/no-go, an activation/coexistence item and a hard toolchain gate; D1, D2, D4, D5 and D7 are refined.
**Evidence:** `_notes/peg-parser-research-2026-10-12.md` (DuckDB 2.0 grammar API,
the current Snowflake surface, and the inventory behind §3 of this plan).

## 1. Why

Most of the extension's remaining limitations come from how it has to talk to
DuckDB's parser, not from the semantic model. Today every statement goes through a
`parser_override` hook. Hand-written Rust scanners decide whether the text is ours,
parse it, and lower it into SQL text that DuckDB then parses again. That design is
the cause of:

- **the query surface**: `semantic_view('v', dimensions := [...])` instead of
  Snowflake's `SEMANTIC_VIEW(v DIMENSIONS ... METRICS ... WHERE ...)`, and the
  `where_clause :=` spelling, which exists only because `where` is reserved in a
  named-parameter slot;
- **query-time expressions and aliases** (#76), which are impossible without a grammar;
- **SHOW/DESCRIBE in subquery position** (#77). We only recognise statement *prefixes*;
- **a whole class of scanner bugs**: #24, #25, #38, #46, #59, #64/PARSE-9, #73 and
  PARSE-12, plus the double-parse error path (#22) and the `disable_peg_parser`
  interaction (#21);
- **~11k lines of Rust** (parse/, body_parser/ mechanics, statement-level util/ident
  helpers) and **~750 lines of C++** hook plumbing, plus roughly 8k lines of tests
  and three fuzz targets that only exist to guard those scanners.

DuckDB 2.0 moves its PEG parser into core and gives C++ extensions a supported way
to add grammar (`GrammarExtension` / `GrammarChange`). That lets our statements become
real productions: they parse wherever DuckDB's own statements parse, with DuckDB's
expression grammar inside them.

**Goal:** finish with full Snowflake semantic-view parity on DuckDB 2.0, with the
workaround layer deleted rather than wrapped. The tests, oracles, persistence model
and round-trip contracts that make the extension trustworthy all come through intact.

## 2. What DuckDB 2.0 gives us (summary; details and sources in the research note)

- **Timeline.** 2.0.0 is scheduled for **2026-10-21**. The 1.5.x line reaches end of
  life on **2026-11-01**. The PEG parser is the default in 2.0, and libpg_query is
  gone. 1.5.6 has **no** grammar-extension API: its PEG parser is opt-in and lives
  inside the autocomplete extension.
- **API.** You subclass `GrammarExtension`, return `vector<GrammarChange>`
  (`AddRule`, `AddChoice`, `PrependChoice`, `ReplaceChoice`, `ReplaceRule`,
  `SetTransformProcess`, ...), and register with
  `GrammarExtension::Register(db, ...)`. Each new rule gets a transform callback that
  turns the parse result into DuckDB AST nodes, and it can call the stock transformer
  for sub-rules such as expressions.
- **Hook points we need all exist:** `CreateStatementVariation`,
  `DescribeStatement`/`ShowOrDescribe`, `DropEntries`, `InnerTableRef`, `SelectAtom`.
  DuckDB's own demo (`test/extension/loadable_grammar_extension_demo.cpp`, pipe SQL)
  is the reference.
- **C++ only.** None of this is in the C API, so it lives in `cpp/src/shim.cpp`.
  That does not change our ABI posture. We already compile the amalgamation, use
  `C_STRUCT_UNSTABLE`, and pin DuckDB exactly (TECH-DEBT "Known Architectural
  Limitations" #2).
- **Activation is per connection.** `SET active_grammar_extensions = [...]` is
  local-scope, so we must turn it on for every connection, e.g. via an
  `OnConnectionOpened` callback. duckdb-psql (ported 2026-10-06) is a working example.
- **The parse is context-free.** The transformer has no `ClientContext`, so it cannot
  look up whether `FROM sales` is a semantic view. That matters for the
  standard-SQL query form (Phase 5).
- **The API is still moving.** The August blog snippet no longer matches `main`.
  Phase 1 is a spike for this reason.
- **A transform returns a parsed AST node, never side effects.** This shapes DDL
  (§5 Phase 1 item 3a). Transactional DDL works today because the lowered
  `INSERT`/`DELETE`/`UPDATE` runs as the caller's own statement. A transform must
  therefore return those statements as AST, built by hand or by re-parsing the
  lowered SQL with a nested `Parser`. Handing the IR to a table function that
  writes the catalog would run the write on a fresh `Connection` and lose
  `BEGIN … ROLLBACK` (#19). That is the one thing we must not trade away.
- **`parser_override` still exists on 2.0 with the same `const string &query`
  signature** (`parser_extension.hpp`). Only the fallback `parse_function` moved to
  tokens. So the engine bump (Phase 2) is small, and the no-go fallback is simply
  to keep today's hook.
- **Transforms read host objects by layout.** `ParseResult::Cast<>` is a
  `reinterpret_cast`, so any header drift between our amalgamation and the host is
  undefined behaviour, not a load error. Exact version pinning is what makes this
  safe, and that pin stays non-negotiable.
- **No rule-local syntax errors.** A failed match inside our rule yields DuckDB's
  generic syntax error. Positioned `ParserException`s are only available from a
  transform, which means after a successful match. Expect the ported caret tests to
  become accept/reject cases, with positioned messages only for semantic failures.
  Negative lookahead `!` is parsed but ignored by the matcher, so clause-boundary
  rules must be designed without it.

## 3. Keep, rewire, delete

The full inventory is in the research note. In short:

**Keep unchanged (the domain, ~37k LOC):** `model.rs`, `graph/*`, `expand/*`,
`catalog/mod.rs`, `render_ddl.rs`, `render_yaml.rs`, `errors.rs`, `ddl/*` row
producers, `expr_tokens.rs`, `ffi_util.rs`, `body_parser/cross_refs.rs` (moves out of
`body_parser`), and the shim's function registration and bind/exec engine.

**Keep but rewire:**
- `parse/native_sql.rs` and `catalog/writes.rs`. Lowering DDL to DML on the caller's
  connection is what makes our DDL transactional, and it stays. Its *input* changes
  from a scanned `RewriteAction` to the grammar transform's output.
- `RewriteAction` / `CreateMode` survive as the IR between the C++ transform and Rust.
- `query/*`. Expansion and the type probe stay. The entry becomes the grammar's table ref.
- `ident.rs`. `ident_matches` and normalisation stay; name-slot slicing goes.
- `util.rs`. `suggest_closest` stays; statement-level quote and comment helpers go.
  `QuoteState` stays for as long as expressions are stored as text.
- `lib.rs` `REGISTRATIONS`. The parser-hook entry is replaced by grammar registration
  and the per-connection activation.

**Delete:**
- `parse/{ffi,detect,rewrite (scanning half),create_body,show_clauses,search_path}.rs`
- `body_parser/{lexer,cursor,scan,clause_bounds}.rs` and the recursive-descent
  mechanics of the clause parsers. The semantic checks inside them (duplicates,
  PRIVATE rules, verbatim-representability) move into the transform layer.
- shim `sv_parser_override`, `sv_parse_stub`, `sv_plan_unreachable`,
  `sv_register_parser_hooks`, and `cpp/include/parser_extension_compat.hpp`.
- The `search_path :=` text injection. The grammar transform adds the argument
  structurally.
- `tests/parser_hook_idempotent.rs` and `tests/registration_order_guard.rs`. Both are
  structural checks on the hook, and that hook will be gone.

**Port the cases, not the harness (coverage-forward, per CLAUDE.md):**
- ~3.5k lines of `rewrite.rs` unit tests, ~4.3k of `body_parser` tests,
  `parse_proptest`, `create_front_door_proptest`, `crlf_body_frontdoor`,
  `quoted_idents_regression`, the `error_caret_*` / `cr*_parser_*` / `phase2x`
  sqllogictests, and fuzz targets `fuzz_ddl_parse`, `fuzz_keyword_body` and
  `fuzz_parser_override_ffi`.
- These encode the accepted and rejected DDL language. Each case becomes a row in an
  accept/reject corpus run against the new grammar (sqllogictest plus a Rust-side
  corpus test).
- Error wording and caret positions will change, so cases assert on the new messages.
  Do not drop them.
- A case that no longer applies, because the grammar makes the bug unrepresentable,
  is listed in its phase's PR with the reason.

**Untouched:** all numeric oracles (differential, star_schema, multi_hop_join,
child_grain, semi_additive, window_metric, metamorphic, expand, output) and the
expansion, JSON, YAML, where-predicate and query-name fuzz targets. These are the
safety net the migration is measured against. `roundtrip_proptest`, `yaml_proptest`
and `fuzz_render_roundtrip` are re-pointed from `parse_keyword_body` to the new parse
entry, so the `GET_DDL` → parse contract keeps its guard.

**Mechanical churn to expect:** 83 of 112 `.test` files call
`semantic_view(...)`. Converting them is a scripted rewrite, landed as its own PR so
review can tell syntax churn from semantic change.

## 4. Snowflake parity map (beyond what we ship today)

Checked against docs.snowflake.com on 2026-10-12. "P#" is the phase in §5.

| Area | Snowflake | Ours today | Phase |
|---|---|---|---|
| `SEMANTIC_VIEW(v DIMENSIONS … METRICS … FACTS … WHERE …)` as a table expression (joins, CTEs, PIVOT) | GA | table function | P4 |
| Query items as expressions with `[AS] alias`, `tbl.*`, ad-hoc aggregates of dims/facts in METRICS | GA | names only (#76) | P4 |
| Standard SQL: `SELECT d, AGG(m) FROM sv WHERE … GROUP BY … HAVING …` | GA 2026-03 | none | P5 |
| SHOW/DESCRIBE usable as subqueries | (DuckDB idiom) | no (#77) | P3 |
| `data_type` in SHOW/DESCRIBE | populated | populated, read-side probe (#51 ✅) | P0c ✅ |
| BI discovery (view appears with typed columns) | n/a (Snowflake catalog) | none | P5 |
| `VARIABLES (...)` + query-time `VARIABLES v => val` | GA 2026-06 | none | P6 |
| Logical table as `alias AS (<query>)` | GA 2026-06 | none | P6 |
| `CREATE OR ALTER SEMANTIC VIEW` | preview | rejected | P6 |
| `SHOW … IN ACCOUNT`, `LIMIT n FROM '…'`, `GET_DDL('SEMANTIC VIEW', …)` spelling, `DERIVED_METRIC` kind in DESCRIBE, `required` column in `… FOR METRIC` | GA | partial | P6 |
| Metadata clauses: `AI_SQL_GENERATION`, `AI_QUESTION_CATEGORIZATION`, `AI_VERIFIED_QUERIES`, `TAG` / `SET TAG`, `COPY GRANTS`, `SAMPLE_VALUES`, `IS_ENUM`, `WITH CORTEX SEARCH SERVICE`, `MAX_STALENESS` | GA | rejected | P6 (accept + store + round-trip; decision D5) |
| Multi-path `USING` (several relationships) | preview 2026-03 | single | P6 |
| `ALTER … SET/UNSET TAG`; YAML import flags (`verify_only`, `create_or_alter`) | GA | none | P6 |
| `ASOF` relationships | GA | not planned | P7 |
| Range relationships (`CONSTRAINT DISTINCT RANGE` + `BETWEEN … EXCLUSIVE`) | preview | none | P7 |
| Many-to-many via bridge tables | GA | none | P7 |
| Materializations (`ADD/DROP/REFRESH MATERIALIZATION`) | limited access | our own `MATERIALIZATIONS` clause | **out of scope for this epic** (per Anentropic, 2026-10-12) |
| Composable views (`IMPORTS`) | private preview | none | watch only |
| Privileges, PRIVATE visibility by role, Cortex Search | — | n/a in DuckDB | out of scope |

**Altertable features that go beyond Snowflake.** Anentropic asked for these to get a look
too (2026-10-12): metric `FILTER (WHERE …)`, `OFFSET (dim BY interval)`,
`CUMULATIVE`, and a built-in time spine (`TIME (col)` / `metric_time`). They are not
Snowflake syntax, and CLAUDE.md makes Snowflake the model for semantics, so they get
their own phase (P8) after parity. Each needs an explicit adopt / skip call.

Caveat on Altertable: our comparison is from the README Sylvain sent. The research
pass could not find their source repo publicly, so "they vendored the PEG parser on
1.5" is their claim, not something we have read.

## 5. Phases

One PR open at a time. Each waits for Anentropic's Copilot review and merges only on Anentropic's word.
Every PR passes `just test-all`. Any query-semantics change extends at least one
numeric oracle (CLAUDE.md).

**Standing rules for every phase** (from Anentropic, 2026-10-12):

- **Every phase strengthens the tests.** Each phase adds or reinforces
  high-quality, *discriminating* tests, meaning tests that would fail on the
  plausible wrong implementation, not just pass on the right one. Each red is
  confirmed per case, as CLAUDE.md requires. Each phase's PR says which new tests
  discriminate what. Porting the parser tests (§3) counts as preservation, not
  strengthening. Phases that delete code still have to add something, such as the
  DDL-text fuzz target, grammar accept/reject corpora, or `BEGIN … ROLLBACK`
  coverage on the new ingress.
- **Docs before release.** No version is published until its user-facing changes
  are documented through the doc-writer workflow
  (https://github.com/anentropic/for-agents/tree/main/doc-writer; repo config in
  `.doc-writer/`). This applies especially to Phases 2, 3, 4 and 5, which change
  syntax users write. `.doc-writer/context.md` still describes v0.5.3 and the
  `semantic_view()` interface, so refresh it as part of the first release in this
  epic.
- **Code review before the next release.** Before the next release only
  (Anentropic, 2026-10-13): a thorough code review by Fable of everything going
  into the release; its findings go to Anentropic for a decision and are resolved
  before tagging. Whether later releases get one is decided case by case.

### Phase 0: fixes on v1.5.6 (now, in parallel with the Phase 1 spike)

Expansion-layer work that does not touch the parser. It lands while the parser is
stable, so the differential harnesses judge it without migration noise. It has no
code overlap with the spike branch, so the two run side by side.

- **0a:** #72 (disconnected base table accepted), #69 and #70 (role-play × fact
  inlining, quoted relationship name). These are small, loud bugs, so do them first.
- **0b, gated on D1:** TECH-DEBT #71 (HIGH), a metric's value changing with its
  co-queried metrics. See §6. It is a deliberate numbers change, so it does not
  block Phases 1 to 3. It must land before Phases 4 and 7 add emission shapes.
  **✅ Done 2026-10-13** on branch `claude/phase0b-home-anchoring`: every metric is
  home-table anchored, with D1 settled by the Snowflake fixture (Q1–Q6 reproduced in
  `test/sql/d1_home_table_anchoring.test`). The shapes the per-grain planner still
  declines, and the unchanged dimensions-only / FACTS row semantics, are TECH-DEBT
  #82; Phases 4 and 7 should target the home-anchored emitter, not those fallbacks.
- **0c (✅ done, branch `claude/phase0c-data-type`):** #51 member `data_type`, **read-side**: probe at `SHOW`/`DESCRIBE` bind
  time, as `query::table_function::try_infer_schema` already does for queries. It
  has no parser dependency. It also cannot be done at CREATE under the grammar,
  because the transform has no context and a function called from the lowered DML
  binds on a fresh connection that can't see same-transaction tables (#19). A
  read-side type never goes stale when a source column changes. The Phase 5 BI
  placeholders consume it.

### Phase 1: spike on `v2.0-cyanoptera` (go / no-go) — ✅ GO, 2026-10-12

**Result:** see `_notes/spikes/peg-grammar-2026-10-12/README.md`. Every item below
worked against a real 2.0 build, including transactional DDL through a grammar
transform. Three findings feed later phases:

- `C_STRUCT_UNSTABLE` can't load on 2.0, but a `C_STRUCT` extension pinned to the
  v1.5.6 V1 C API can (→ Phase 2).
- That loses DuckDB's exact-version check, so we need our own load-time version
  guard (→ Phase 2).
- Comments inside member expressions are lost in text capture (→ D4).

Original spike brief, kept for reference:

2.0.0 is days away and every later phase hinges on this answer. Throwaway branch;
the output is a `_notes/` write-up.

1. Register a `GrammarExtension` from our shim, where the amalgamation copy registers
   into the host's `DatabaseInstance`, and confirm it loads under the CLI and the
   Python client (the Phase 15 visibility trap).
2. **Activation and coexistence.**
   - Activate the grammar per connection via `ExtensionCallback::OnConnectionOpened`
     and on the loading connection.
   - Measure what happens on connections opened **before** `LOAD`: the CLI's own,
     Python's default connection, `.cursor()` clones, ADBC/JDBC pools.
   - Measure what happens when a user, or another grammar extension such as psql,
     runs `SET active_grammar_extensions = [...]`, which replaces the list.
   - Design the "our syntax on a connection where the grammar is off" error so it is
     actionable.
   - Confirm that none of the shim's ~12 internal `Connection` sites ever runs text
     containing our syntax.
3. One production each:
   - **3a, the real go/no-go:**
     `CREATE SEMANTIC VIEW x AS TABLES (...) DIMENSIONS (...)` → transform → Rust IR
     → existing `native_sql` lowering → returned as **statement AST executed on the
     caller's connection**. Proved by a green `BEGIN … CREATE … ROLLBACK` test.
   - `FROM SEMANTIC_VIEW(x DIMENSIONS a METRICS m)` → transform → a stock
     `TableFunctionRef` to our table function. It must be stock, so that
     `CREATE VIEW v AS SELECT * FROM SEMANTIC_VIEW(...)` serializes into the catalog
     and re-binds on connections without the grammar. Use `PrependChoice` on
     `InnerTableRef` and fail cleanly on a `'v'` string argument, so the deprecated
     `semantic_view('v', …)` alias (D3) still parses as an ordinary table function.
   - `SELECT * FROM (SHOW SEMANTIC VIEWS)`. Choices on `DescribeStatement` /
     `ShowByName` should get subquery position for free, since `TableSubquery`
     already admits `DescribeStatement`.
4. Measure what users see on a syntax error inside our rule, and on DuckDB's
   autocomplete.
5. Expression capture, precisely: can a transform slice the **original query
   string** at a child's `[offset, offset+length)`? `ParseResult` carries the
   extent, but the transformer holds a `TokenIterator`, not the SQL text, and
   rebuilding from tokens loses whitespace and comments. The answer decides D4.
6. **Hard gate on D2 and D7:** duckdb-rs / `libduckdb-sys` releases for 2.0, how
   the version-pin encoding (`vX.Y.Z → 1.1XY0Z.0`) extends, `extension-ci-tools`,
   and community-extension CI against 2.0.

Exit: a written go / no-go. On no-go, the fallback is to keep `parser_override`,
which still takes the raw query string on 2.0, and its scanners. Replace only the
`parse_function` caret path with the token API.

### Phase 2: engine bump to DuckDB 2.0, behaviour unchanged

Bump `.duckdb-version`, the crate pins, the pin-encoding test and
`DuckDBVersionMonitor.yml`. Per the spike:

- Switch the ABI type from `C_STRUCT_UNSTABLE` (which 2.0 only loads through the
  new `_init_c_api_v2` handle API) to `C_STRUCT` pinned to the v1.5.6 V1 C API, and
  verify that duckdb-rs `=1.10506.0` keeps working against a 2.0 host. If it does,
  the duckdb-rs 2.0 release stops being a hard gate.
- Add a load-time guard that refuses to load when the host's DuckDB version or
  source id differs from the one our amalgamation was built from. `C_STRUCT` no
  longer enforces that, and our C++ reads host objects by layout. `parser_override` keeps its string signature on 2.0, so
only the `sv_parse_stub` caret path has to move to the token-based `parse_function`.
All tests stay green with **no** user-visible change. This separates "new engine" from "new parser"
so a regression can be bisected to one or the other. Decision D2 decides whether
1.5.x keeps a maintenance branch.

**Found on 2026-10-15: 2.0 has no amalgamation.** `scripts/amalgamation.py` on the 2.0
branch is a 97-line remnant (616 lines on v1.5.6) that no longer emits
`duckdb.{hpp,cpp}`. The spike linked 2.0's static libraries directly, so it did not
notice. `build.rs` compiles the amalgamation into the extension today, so Phase 2 also
has to change the build: compile the shim against the engine's `src/include` tree and
link DuckDB's static libraries. That touches `scripts/ensure_amalgamation.py`, the
offline fetcher, and every CI job that builds the extension.

**Staged, per Anentropic (2026-10-15).** Do what does not need a released 2.0 now, and
the rest at 2.0.0 GA (scheduled 2026-10-21), rather than building the engine from the
moving dev branch here and in CI:

1. *Now, on v1.5.6:* switch to `C_STRUCT` pinned to the v1.5.6 C API, and add the
   load-time version check (`src/load_guard.rs`; it compares version strings only, as
   `C_STRUCT_UNSTABLE` did, so the offline amalgamation's placeholder source id still
   loads). Without the check, a v1.5.6 build segfaults a 2.0 host on `LOAD`
   (`test/integration/test_load_version_guard.py`).
2. *Now:* port the shim to compile against 2.0's headers as well, behind one
   `SV_DUCKDB_2` macro, syntax-checked against the dev engine (commit 96063b9e39,
   PyPI wheel `2.0.0.dev2610011535`).
3. *At GA:* link the released static libraries, bump the pins, move the version-guard
   test's other host to 1.5.6, and run the full suite on 2.0.

Found while porting (2026-10-15):

- **The release artifacts carry no C++ headers.** `libduckdb-src.zip` and the
  `static-libs` archives ship only the C headers (`duckdb.h`, `duckdb_extension.h` and
  their v2 forms). The shim needs `src/include/**` (including
  `main/capi/capi_internal.hpp` for `DatabaseWrapper`) plus the third-party include
  directories, so the GA build fetches the engine source at the release tag alongside
  the static libraries. A cmake install tree has `src/include` but not `third_party`.
- **The port is syntax-checked only.** Three things need a real 2.0 build, each marked
  `TODO(duckdb-2.0)` in the shim: `is_interrupted` does not forward 2.0's per-query
  deadline; the `sv_query_interruptible` task loop needs its cancellation tests run; and
  2.0's `parse_function` gets tokens without offsets, so the caret path rebuilds a
  normalised statement text (caret position, the text the Rust validator sees, and the
  TECH-DEBT #85 multi-statement hint can differ from 1.5). Phase 3's grammar replaces
  that path anyway.
- **Some errors become the engine's wording**, e.g. an unknown named parameter now reads
  "Invalid named parameter %s for function %s"; expect small test-expectation changes.

**Step 3 done ahead of GA (2026-10-16), against a pre-release.** Built and tested against
`v2.0.0-alpha47600` (wheel `2.0.0.dev2610151406`, engine commit `d4355d01c5`), the
newest pre-release with a wheel, so the PR only needs its pins moved at GA (TECH-DEBT #90).
What changed from the plan:

- *Engine source.* Not the release's static libraries plus a source checkout: the engine is
  built with its own CMake from the PyPI sdist (`scripts/ensure_engine.py`, cached under
  `.engine/`), which carries the exact source the wheel was built from, `third_party/`
  included, and needs no GitHub. A harness-supplied `./duckdb` tree is built instead when
  present, under its own release label.
- *Pins split.* `.duckdb-version` is the engine; `.duckdb-capi-version` (v1.5.6) is the C API
  the binary declares, which the duckdb-rs crates and `MINIMUM_DUCKDB_VERSION` follow.
- *The three `TODO(duckdb-2.0)` items* are settled (TECH-DEBT #88): the caret is rendered by
  the shim from the query text `parser_override` saw; the interrupt loop passes its tests;
  the per-query deadline was a real gap and is now forwarded.
- *Found:* the 2.0 wheel's ADBC driver loads extensions only once `duckdb` is imported
  (TECH-DEBT #89); the deprecated `->` lambda is a binder error on 2.0 (the catalog write
  path and two tests used it); DuckDB's tokenizer now rejects bad quoting before the
  extension sees the statement.

### Phase 3: DDL and introspection as grammar productions

- CREATE (including `FROM YAML`), ALTER, DROP, DESCRIBE, every SHOW form, and
  `GET_DDL` become productions that transform into the Rust IR. This includes our
  own non-Snowflake `MATERIALIZATIONS` clause. The feature is out of scope for new
  work, but existing DDL must keep parsing.
- The scalar functions (`GET_DDL`, `read_yaml_from_semantic_view`) need a named,
  structural replacement for today's search-path handling (#25).
- The grammar is a new ingress path, so the PR enumerates the validation choke
  points it preserves (CLAUDE.md "fences"), as Phase 4 does.
- Delete `test/sql/peg_compat.test`.
- Retire `_notes/entry-point-decision.md` into the rewritten architecture note.
- Delete the §3 "delete" list in the same phase, across as many PRs as review needs.
  The grammar test corpus lands **before** the deletion PR, green against both
  implementations, so coverage provably carries over.
- Closes **#77** (SHOW/DESCRIBE as subqueries; restore the six removed doc examples),
  **#21**, **#22**, **#59**, **#60** (a DDL-text fuzz target against the grammar),
  and **#64/PARSE-9**.
- Updates MAINTAINER.md (src layout, fuzz list), `_notes/architecture.md` (currently
  describes v0.5.4; rewrite it), and TECH-DEBT accepted-decision #8 ("statement
  rewrite approach").

### Phase 4: `SEMANTIC_VIEW(...)` query construct and query-time expressions

- The grammar rule on `InnerTableRef` is
  `SEMANTIC_VIEW(<name> [METRICS|FACTS …] [DIMENSIONS …] [WHERE …] [VARIABLES …])`,
  with items `<expr> [[AS] alias]` and `tbl.*`. Clause order sets column order, as in
  Snowflake. `WHERE` takes uncorrelated subqueries.
- **#76** in full: expression items, aliases, Snowflake's same-logical-table rule for
  dimension/fact expressions, and ad-hoc aggregates of dims/facts as metrics.
- Emission gains expression GROUP BY keys on every path: base, per-grain,
  semi-additive snapshot, window, and materialization routing.
- Fences walked past (CLAUDE.md): the fan-trap checks, role-playing ambiguity and
  `USING`, the source-table reachability scanner, and the validation choke points.
  The PR enumerates each one.
- Oracles: `star_schema_proptest` and `multi_hop_join_proptest` generate expression
  items and mirror them.
- Decision D3 settles the fate of `semantic_view()` and `where_clause :=`.
- The scripted `.test` conversion PR lands here.

### Phase 5: standard-SQL querying and BI discovery

Member types (#51) move to Phase 0c.

- **`SELECT … AGG(m) … FROM sv … GROUP BY …`.** The parse is context-free (§2), so
  it cannot know `sv` is semantic. A spike picks one of:
  - (a) a grammar rule that recognises a SELECT containing `AGG(` and defers it to a
    bind-time table function holding the statement, which resolves `sv` and either
    expands or re-plans the statement as plain SQL;
  - (b) a catalog placeholder view per semantic view, plus
    `OperatorExtension::Bind(ClientContext&, Binder&, …)` to take over binding;
  - (c) a replacement scan.
  Option (c) is incompatible with (b) and D6, because replacement scans only fire
  when the name does **not** resolve.
  Snowflake's restrictions (no joins, window calls, QUALIFY or correlated subqueries
  in this form) bound the work.
- **BI discovery.** Placeholder views in `information_schema` / `duckdb_views()` with
  one typed column per public member. Decision D6 settles the namespace sharing. The
  placeholders must be created and dropped inside the same DDL transaction. Lowered
  DML can't `CREATE VIEW`, so this needs a real catalog write path. Budget for it.

### Phase 6: remaining DDL and metadata parity

- `VARIABLES` (definition plus query-time binding), inline-query logical tables, and
  `CREATE OR ALTER`.
- Multi-path `USING`.
- The `SHOW` option set (`IN ACCOUNT`, `LIMIT … FROM`), `DESCRIBE` kinds and
  properties (`DERIVED_METRIC`, `CONSTRAINT`, `LABELS`, `required`), and the
  `GET_DDL('SEMANTIC VIEW', …)` spelling.
- The metadata-only clauses (D5).
- Each item is a small grammar plus model change. The round-trip proptest catches
  omissions in GET_DDL and YAML.

### Phase 7: relationship modelling parity

`ASOF` relationships, range relationships, and many-to-many bridges. Each is a new
join shape: new emitters, new fan-trap reasoning, and a new oracle in the
differential harnesses. #71's home-table design (§6) is the base they build on. The
bridge semantic (count once per group) is Altertable's, and it matches Snowflake's
documented intent.

### Phase 8: beyond Snowflake (adopt / skip per item)

Metric `FILTER`, `OFFSET`, `CUMULATIVE`, and time spine. Each item gets a one-page
proposal with its syntax and oracle story, and lands only if adopted.

## 6. TECH-DEBT #71: before, during, or after the epic?

**It does not depend on the parser.** The defect lives entirely in `src/expand`:
`per_grain` anchors each grain's CTE at its own table, while a single-grain request
is anchored at the base table (`expand::sql_gen` / `per_grain::is_eligible`). Nothing
in the grammar work changes it, and nothing in it needs the grammar. It has a
plausible implementation on v1.5.6 today.

**Recommendation: do it early, as Phase 0b.** It is gated on D1 and must not block the spike or Phases 1 to 3, but it must land before Phases 4 and 7. Three reasons:

- Phases 4, 5 and 7 (expression items, ad-hoc aggregates, standard-SQL queries,
  bridges, ASOF) all add emission shapes. Each would otherwise be built twice, once
  for each anchoring.
- It is a numbers change, so it should be judged by the differential and metamorphic
  harnesses while those are the only thing moving. Mixing it into a parser migration
  would blur which change moved a number.
- The `#[ignore]`d `mm3_metric_value_changes_when_co_queried_across_grains` already
  states the correct invariant, so it is a ready-made red test.

**Shape of the fix:** one anchoring rule everywhere, so a metric's number never
depends on its companions. The natural candidate is the per-grain rule we already
have for multi-grain queries, which is also Altertable's design: every metric is
aggregated from its **home table** with only many-to-one joins outward, then the
blocks are merged on the dimensions. That means single-grain child metrics are
re-anchored too. It is a deliberate change to some current single-grain numbers, so
it goes in the CHANGELOG under `Changed`.

It still needs a semantic call, D1, because it changes two edge cases:

- a child row whose FK is NULL is **counted**. Today it is dropped by the
  base-anchored join;
- a parent with no children reports `NULL` for a child `COUNT`, not `0`, unless we
  coalesce.

Snowflake's docs don't settle either case. The parity plan's maintainer-run
Snowflake check (`SNOWFLAKE-PARITY-PLAN.md` §3) is the cheapest way to be sure, and
the same fixture answers both.

## 7. Decisions only Anentropic can make

| # | Question | Recommendation |
|---|---|---|
| D1 | #71 semantics: count orphan child rows (FK NULL)? Childless parent's child-COUNT = `NULL` or `0`? | **Settled 2026-10-13 by the Snowflake fixture:** home-table anchoring; orphans counted under a NULL dimension; a childless parent's child COUNT is `NULL` (no `COALESCE`) when a parent-grain metric brings its row in, and a child metric alone gives no row for it. Implemented in Phase 0b. (Earlier leaning, superseded: `0` for COUNT.) |
| D2 | Drop DuckDB 1.x when Phase 2 lands? | Yes, once spike item 6 shows that duckdb-rs and community CI are ready for 2.0. 1.5 is end of life on 2026-11-01. Tag v0.14.x as the last 1.5 line (fixes only). |
| D3 | Keep `semantic_view('v', dimensions := …)` after `SEMANTIC_VIEW(...)` ships? | Keep it one release as a documented, deprecated alias that maps onto the same IR, then remove it. `explain_semantic_view` becomes a `semantic_view_sql`-style function over any statement. |
| D4 | Store member expressions as verbatim source text (today) or as DuckDB-normalised text? | The spike shows a transform can rebuild the expression's exact code text, spacing included, from its tokens. Only comments inside an expression are lost, and they become spaces. Recommendation: keep storing that text, and document comment loss as a known divergence with a TECH-DEBT entry and a round-trip test pinning it. That preserves `GET_DDL` fidelity. Moving to AST storage would also retire #73 and IDENT-2/3/4, but it is a separate decision. |
| D5 | Snowflake metadata clauses (`AI_*`, `TAG`, `COPY GRANTS`, `SAMPLE_VALUES`, `IS_ENUM`, Cortex search, `MAX_STALENESS`): reject (today), or accept and store? | Accept, store, show in DESCRIBE, and round-trip through GET_DDL/YAML, so Snowflake DDL pastes as is. `COPY GRANTS` and `MAX_STALENESS` are accepted as no-ops. Budget it: about 9 clauses, each touching model + JSON + YAML + `render_ddl` + DESCRIBE. |
| D6 | BI placeholder views share the table/view namespace (a semantic view can't share a name with a table). OK? | Yes, behind the Phase 5 spike. It is what lets BI tools see them at all. |
| D7 | Version line for the 2.0 cut? | Ship Phase 2 as v0.15.0 (v0.14.0 shipped on 1.5.6 as the last 1.5 line, published to the registry's `variegata` key). Phases 3 and 4 ship together as v0.16.0 (Paul, 2026-10-16); versions for Phases 5 to 8 are decided as each lands. Consider v1.0.0 once Phase 4 lands **and** D4's expression storage format has settled. |

## 8. Done when

Every row in §4 is either shipped or recorded in `snowflake-comparison.rst` as a
deliberate, reasoned divergence. The §3 delete list is gone from `src/`. The only
parser-specific code left is the grammar and its transforms. No numeric oracle lost a
generator axis along the way.
