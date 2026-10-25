# PEG parser epic: research notes (2026-10-12)

This note backs `.planning/PEG-PARSER-EPIC.md`. Sources were read on 2026-10-12.

- **VERIFIED** means it was read in source, or on the cited page.
- **INFERENCE** means it is our own reasoning.

## A. DuckDB PEG parser and grammar extensions

The sources are shallow clones of duckdb/duckdb at `v1.5.6` (069cc9f) and at `main`.
On that date `main` was the same commit as the `v2.0-cyanoptera` branch (b554c96).

### Timeline (VERIFIED)

- **v1.2:** PEG grammar used only for CLI autocomplete.
- **v1.5:** the full PEG parser, opt-in via `CALL enable_peg_parser()`.
  - This call only sets `allow_parser_override_extension='strict'`.
  - The parser lives in `extension/autocomplete/{grammar,parser,transformer}`.
  - It is plugged in as a `ParserExtension` whose `parser_override` is `PEGParser`.
  - **There is no grammar-extension API in 1.5.6.**
  - Sources: https://duckdb.org/docs/current/sql/peg_parser and `extension/autocomplete/autocomplete_extension.cpp`.
- **v2.0:** PEG is the default parser (https://duckdb.org/2026/08/20/duckdb-20-peg-parser).
  - It is now in core: `src/parser/peg/` and `src/include/duckdb/parser/peg/`.
  - The grammar is in `grammar/statements/*.gram`, with keywords in `grammar/keywords/*.list`.
  - `third_party/libpg_query` is removed.
- **Release calendar** (https://duckdb.org/release_calendar):
  - 2.0.0 is tentatively 2026-10-21, and 2.0.1 is 2026-11-18.
  - 1.5.x reaches end of life on 2026-11-01.
  - There is no `v2.0.0` tag yet.
- **Build model:** `.gram` files are inlined at build time into `INLINED_PEG_GRAMMAR`.
  - The script is `scripts/parser/build_grammar.sh`.
  - Transformer trampolines are code-generated.
  - `ParsedGrammar::ParseSingleRule` parses single rules at runtime, and the extension API uses it.

### Extension API on 2.0 (VERIFIED)

Headers: `src/include/duckdb/parser/grammar_extension.hpp` and `grammar_change.hpp`.

```cpp
class MyGrammar : public GrammarExtension {
  vector<GrammarChange> GetChanges() const override;   // AddRule / AddChoice / PrependChoice /
};                                                      // RemoveChoice / ReplaceChoice / ReplaceRule /
GrammarExtension::Register(db, make_shared_ptr<MyGrammar>(...));  // SetTransformProcess / AddTerminalRuleOverride
```

- **Transforms.** A transform is a `grammar_transform_process_function_t(PEGTransformer&, ParseResult&)`.
  - It pulls children by position.
  - It can call `transformer.Transform<T>()` on stock rules.
  - It returns DuckDB AST nodes.
- **Keywords** are added with e.g. `AddChoice("UnreservedKeyword", "'SEMANTIC'")`.
- **Worked example:** `test/extension/loadable_grammar_extension_demo.cpp` (pipe SQL, via `PrependChoice("SelectAtom", ...)`).
  - Its test is `test/extension/test_loadable_grammar_extension.test`.
- **Activation.** Activation is `SET active_grammar_extensions = ['name']`.
  - The setting has **local (per-connection) scope**: `src/common/settings.json` and `ClientConfig::active_grammar_extensions`.
  - duckdb-psql activates it on load and offers `CALL psql_enable()` for other connections: https://github.com/ywelsch/duckdb-psql/pull/12 (merged 2026-10-06, against v2.0-cyanoptera).
  - That is the pattern to follow, not proof it works: it can't cover connections opened before `LOAD` either.
- **Rules we would hook,** from `inlined_grammar.gram`:
  - `Statement`
  - `CreateStatementVariation`
  - `DescribeStatement` / `ShowOrDescribe`
  - `DropEntries`
  - `InnerTableRef <- ValuesRef / TableFunction / TableSubquery / BaseTableRef / ParensTableRef`
  - `SelectAtom`
- **Heavier alternative:** `DialectExtension` (`dialect_extension.hpp`) replaces the tokenizer, matcher and keywords. It is selected with `SET current_dialect`. We do not need it.
- **Autocomplete** builds from `CompiledGrammar::Get(context)`, which includes the active extensions.
- **Old hooks on 2.0:**
  - `parser_override` still exists.
  - The fallback `parse_function` now takes `(ParserExtensionInfo*, const vector<SimpleToken>&)` and returns `consumed_tokens`.
  - That is a breaking change from 1.x's raw-string signature, and it matters for the epic's Phase 2.
- **C API:** none of this is in the C API.
  - `duckdb_v2.h` only exposes tokenize and parse-only functions.
  - `GrammarExtension` is C++ `DUCKDB_API`.
- **API drift:** the blog post's preview API (`extension.grammar_extension.select_atom_rule`, `loader.RegisterKeyword`) no longer matches `main`.
- **No context during parsing:** `PEGTransformer` carries no `ClientContext`, so catalog lookups during parse are impossible. **INFERENCE.**

### ABI

- **VERIFIED:** `CPP` and `C_STRUCT_UNSTABLE` both require an exact version match. `C_STRUCT` needs only equal-or-higher (`src/include/duckdb/main/extension.hpp`).
- **INFERENCE:** we already build `C_STRUCT_UNSTABLE` against the amalgamation and pin DuckDB exactly (TECH-DEBT Known Limitation #2, and "Entry point and C++ linkage" in `_notes/architecture.md`). A C++ `GrammarExtension` in the shim therefore adds no new pinning cost.
- **Risk to check in the spike:** our amalgamation copy's objects, such as the `GrammarExtension` vtable and the transform functions calling `PEGTransformer` methods, would interact with the host's `DatabaseInstance`. Today's `ParserExtension` registration already does the same thing.

### Gotchas (VERIFIED unless marked)

- Ordered choice: `PrependChoice` versus `AddChoice` matters.
- Negative lookahead `!` is parsed but ignored by the matcher (`src/parser/peg/README.md`).
- A new reserved keyword can break identifiers that use that word, so keep ours unreserved.
- Positional transforms are fragile when a rule's shape changes.
- The grammar is still churning on the 2.0 branch: tamnd/firepanda#941 reports 18 files and 199 lines changed since `00ef652`.
- A PEG failure inside our rule gives DuckDB's generic syntax error. **INFERENCE:** we need to check whether a rule can raise its own positioned message.

## B. Altertable `semantic_views`

- Our only source is the README Sylvain sent: `/mnt/project-files/duckdb-semantic-views/altertable-README.md` in the S-LAYER project files.
- The research pass found **no public repo**:
  - The `semantic_views` community-extension entry is ours.
  - Altertable's public extensions are `altertable`, `databricks` and `clickhouse_scanner`.
- So "a vendored copy of DuckDB's PEG parser on 1.5.5, under `third_party/duckdb_peg_parser`" is what the README says, not something we have read.
- **Their expansion design** (README "How it works"), relevant to TECH-DEBT #71:
  1. Metrics are decomposed into aggregate atoms. Each atom has a home table.
  2. There is one block per home table, built as `FROM home LEFT JOIN <many-to-one only>`, then filtered and grouped.
  3. The blocks are merged with `UNION ALL` plus `GROUP BY` on the dimensions, where NULLs match.
  4. Derived metrics are computed on the merged rows.
  5. Bridges (many-to-many) are handled through `SELECT DISTINCT (key, dims)` subqueries.

## C. Snowflake semantic-view surface (2026-10-12)

These are WebFetch summaries of docs.snowflake.com, so check exact grammar wording against the pages.

### CREATE

https://docs.snowflake.com/en/sql-reference/sql/create-semantic-view

- **Statement shape:** `CREATE [OR REPLACE] SEMANTIC VIEW [IF NOT EXISTS] n`
  - Clauses in this order: `TABLES`, `[RELATIONSHIPS]`, `[VARIABLES]`, `[FACTS]`, `[DIMENSIONS]`, `[METRICS]`.
  - Then `COMMENT`, `MAX_STALENESS`, `AI_SQL_GENERATION`, `AI_QUESTION_CATEGORIZATION`, `AI_VERIFIED_QUERIES(...)`, `[WITH] TAG(...)`, `COPY GRANTS`.
- **Logical tables:**
  - `[alias AS] table [PRIMARY KEY] [UNIQUE…] [CONSTRAINT [n] DISTINCT RANGE BETWEEN s AND e EXCLUSIVE] [WITH SYNONYMS] [COMMENT] [TAG]`.
  - Also `alias AS (<query>)`, GA 2026-06-26: https://docs.snowflake.com/en/user-guide/views-semantic/inline-view.
- **Relationships:** `[n AS] a(cols) REFERENCES b [( [ASOF] col,… | BETWEEN s AND e EXCLUSIVE )]`.
  - Range relationships are in preview (2026-02-25).
  - Bridge tables and role-playing are supported.
  - Source: https://docs.snowflake.com/en/user-guide/views-semantic/sql.
- **Facts and dimensions:** `[PRIVATE|PUBLIC] a.n [LABELS = (FILTER)] AS expr [WITH SYNONYMS] [TAG] [COMMENT]`.
  - Dimensions can't be PRIVATE.
  - Dimensions also take `WITH CORTEX SEARCH SERVICE`.
  - Both take `SAMPLE_VALUES (…) [IS_ENUM]` (2026-06-25).
- **Metrics:** `[PRIVATE|PUBLIC] [a.]n [USING (rel,…)] [NON ADDITIVE BY (…)] AS expr …`. A metric with no table prefix is a derived metric.
- **`VARIABLES (n [type] [DEFAULT|:= expr] [COMMENT])`:** GA 2026-06-26. https://docs.snowflake.com/en/user-guide/views-semantic/variables
- **`CREATE OR ALTER`:** preview, 2026-05-04. It keeps grants and materializations. https://docs.snowflake.com/en/release-notes/2026/other/2026-05-04-create-or-alter-semantic-view

### ALTER, SHOW, DESCRIBE and functions

- **ALTER:** `RENAME TO`, `SET|UNSET COMMENT`, `SET|UNSET MAX_STALENESS`, `SET|UNSET TAG`, and the `… MATERIALIZATION` family (limited access). Members can't be added through ALTER. https://docs.snowflake.com/en/sql-reference/sql/alter-semantic-view
- **SHOW SEMANTIC VIEWS:** `SHOW [TERSE] SEMANTIC VIEWS [LIKE] [IN ACCOUNT|DATABASE|SCHEMA] [STARTS WITH] [LIMIT n [FROM '…']]`.
- **SHOW SEMANTIC {DIMENSIONS|METRICS|FACTS}:** same options.
  - Columns: `database_name`, `schema_name`, `semantic_view_name`, `table_name`, `name`, `data_type`, `synonyms`, `comment`.
- **`… FOR METRIC m`:** adds a `required` column.
- **DESCRIBE:** kinds `TABLE`, `RELATIONSHIP`, `DIMENSION`, `FACT`, `METRIC`, `DERIVED_METRIC`, `CUSTOM_INSTRUCTIONS`, `AI_VERIFIED_QUERY`, `CONSTRAINT`.
- **GET_DDL:** `GET_DDL('SEMANTIC VIEW', …)`, with a space in the object type.
- **YAML:**
  - Import is `SYSTEM$CREATE_SEMANTIC_VIEW_FROM_YAML(schema, yaml [, verify_only] [, create_or_alter])`.
  - Export is `SYSTEM$READ_YAML_FROM_SEMANTIC_VIEW`.
  - Snowflake has **no** `CREATE … FROM YAML` SQL form, so ours is an extension.

### Query surface

- **`SEMANTIC_VIEW(...)` construct:** https://docs.snowflake.com/en/sql-reference/constructs/semantic_view
  - Form: `SEMANTIC_VIEW(sv [METRICS …|FACTS …] [DIMENSIONS …] [WHERE …] [VARIABLES v => val])`.
  - The written order of the clauses sets the column order.
  - Items are `expr [[AS] alias]` or `tbl.*`.
  - METRICS items can be scalar expressions over metrics, or ad-hoc aggregates of dimensions or facts.
  - WHERE can reference dimensions and facts only, and is applied before aggregation. Uncorrelated subqueries are allowed.
  - The construct can be used in JOINs, PIVOT, UNPIVOT and CTEs.
- **Standard SQL form:** GA 2026-03-02. https://docs.snowflake.com/en/user-guide/views-semantic/querying
  - Form: `SELECT … AGG(m) … FROM sv [WHERE] [GROUP BY] [HAVING] [ORDER BY] [LIMIT]`.
  - GROUP BY items become dimensions. Select items that are not grouped become facts.
  - Not allowed in this form: joins, PIVOT, LATERAL, window calls, QUALIFY, and correlated subqueries.

### Rules and release dates

- **Rules:** https://docs.snowflake.com/en/user-guide/views-semantic/validation-rules
  - A dimension is valid with a metric only if its table is related and has equal-or-coarser grain.
  - FACTS combined with DIMENSIONS must come from the same table. BCR 2026_07 makes this a hard error.
  - `USING` must start at the metric's own table.
- **Release dates:**
  - Derived metrics: 2025-09-30
  - Standard SQL: 2026-03-02
  - Multi-path USING: 2026-03-13
  - Entity filters: 2026-05-05
  - Inline views and variables: 2026-06-26

## D. Current code: how a statement flows (v0.13.0)

1. `semantic_views_init_c_api` (`src/lib.rs`) runs the `REGISTRATIONS` table. Its last entry is `sv_register_parser_hooks` (`cpp/src/shim.cpp`), which installs `parser_override` and `parse_function` and sets `allow_parser_override_extension=FALLBACK`.
2. **Every statement** passes through the following chain:
   1. `sv_parser_override`
   2. `sv_parser_override_rust` (`src/parse/ffi.rs`)
   3. `rewrite_to_native_sql` (`src/parse/native_sql.rs`)
   4. `plan_rewrite` (`src/parse/rewrite.rs`), which blanks comments and calls `detect_ddl_prefix`
   5. Then one of:
      - no match: search-path injection into read-TF calls, or a pass-through;
      - CREATE: `create_body` → `body_parser::parse_keyword_body` → `RewriteAction::Create` → `enrich_definition_for_create` → `INSERT … json_merge_patch` with guard SELECTs;
      - DROP or ALTER: `DELETE` or `UPDATE` with guards;
      - SHOW or DESCRIBE: a `SELECT * FROM show_*(…)` string plus a text filter suffix.
3. C++ re-parses the returned SQL text and DuckDB runs it on the caller's connection, inside the caller's transaction.
4. **Errors:** Rust returns rc=2, so the default parser runs and fails. Then `sv_parse_stub` re-runs the whole rewrite to produce a positioned `DISPLAY_EXTENSION_ERROR` (TECH-DEBT #22).
5. **Queries:** `semantic_view(...)` binds in C++, crosses the wire format into Rust, which reads the catalog, expands, runs a LIMIT 0 type probe and streams the result.

`_notes/architecture.md` still describes v0.5.4. Phase 3 of the epic rewrites it.
