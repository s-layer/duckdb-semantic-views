# Phase 3 DDL corpus

The DDL language our parser accepts and rejects today, written down as sqllogictest
rows, so the Phase 3 grammar (`.planning/PEG-PARSER-EPIC.md`, Phase 3) can be checked
against it. The corpus lands before the grammar and runs green against the parser
override; each later PR keeps it green, or changes a row on purpose and says why.

## Files

| Slice | Source tests | Corpus files | Row ids | Rows | Case map |
|---|---|---|---|---|---|
| B | `src/body_parser/mod.rs`, first 150 tests | `ddl_corpus_body_a1.test`, `ddl_corpus_body_a2.test` | B-001.. | 184 | [map-B.md](map-B.md) |
| C | rest of `src/body_parser/mod.rs`, `annotations.rs`, `clause_bounds.rs`, `cursor.rs`, `lexer.rs`, `window.rs` | `ddl_corpus_body_b1.test`, `ddl_corpus_body_b2.test` | C-001.. | 191 | [map-C.md](map-C.md) |
| S | `src/parse/rewrite.rs` (276 tests) | `ddl_corpus_statements_a.test`, `ddl_corpus_statements_b.test`, `ddl_corpus_create_forms.test` | S-001.. | 230 | [map-S.md](map-S.md) |
| F | `src/parse/{search_path,show_clauses,ffi,detect}.rs`, `tests/parse_proptest.rs`, `tests/create_front_door_proptest.rs`, `tests/crlf_body_frontdoor.rs`, `tests/quoted_idents_regression.rs` | `ddl_corpus_frontdoor_{a,b,c}.test` | F-001.. | 103 | [map-F.md](map-F.md) |

Every case map lists each source test in source order with the rows that carry it, or
`N/A` with the reason (FFI buffer handling, invalid UTF-8, byte offsets of internal
helpers, `#[should_panic]` invariants: nothing a SQL statement can observe).

## Conventions

- A row is a `# <id>: <source>::<test> — <what>` comment followed by its statements.
- Accepted DDL is checked through catalog state (each file's `<prefix>_ddl` GET_DDL
  macro and `list_semantic_views()`), not through the DML row counts the override
  produces.
- `[syntax]` marks a reject whose message is not pinned (an empty `----` block):
  the grammar will word syntax errors its own way. Semantic rejects pin their message.
- `[text]` marks a row that pins text the grammar may plausibly change (error wording,
  whitespace in stored expressions, quoting in GET_DDL output).
- `PRE-EXISTING (TECH-DEBT #n)` marks today's behaviour pinned although it looks wrong;
  the TECH-DEBT entry says what would fix it. Entries #94 to #98 came from this corpus.
- Caret positions are not pinned.

## What is not here yet

The sqllogictests that already exist (`error_caret_*`, `cr*_parser_*`, `phase2*`,
`peg_compat.test`) and the fuzz targets `fuzz_ddl_parse`, `fuzz_keyword_body` and
`fuzz_parser_override_ffi` are carried over in the deletion PR, where the code they
test goes away.
