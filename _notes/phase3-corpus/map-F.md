# Case map F: statement front door (prefix detection, name slot, framing, search path, SHOW values)

Sources: every `#[test]` in `src/parse/search_path.rs`, `src/parse/show_clauses.rs`,
`src/parse/ffi.rs` and `src/parse/detect.rs`; every test and proptest property in
`tests/parse_proptest.rs`, `tests/create_front_door_proptest.rs`,
`tests/crlf_body_frontdoor.rs` and `tests/quoted_idents_regression.rs`.

Corpus files (row ids `F-nnn`, numbered across all three):

- `test/sql/ddl_corpus_frontdoor_a.test` — F-001..F-047: prefix detection, the view-name
  slot, the shape of the CREATE body.
- `test/sql/ddl_corpus_frontdoor_b.test` — F-048..F-065: body-clause properties, hostile
  bodies through the front door, statement framing (`ffi.rs`).
- `test/sql/ddl_corpus_frontdoor_c.test` — F-066..F-103: search path (`search_path.rs`) and
  SHOW filter values (`show_clauses.rs`).

Markers: `[syntax]` rows are `statement error` with an empty `----` block (no message
pinned; the runner refuses a `statement error` without the `----` line). `[text]` rows pin
text a grammar might plausibly change. `PRE-EXISTING` marks today's behaviour that looks
wrong. Fixture prefix `dcf_`, macro `dcf_ddl`.

Totals: 103 rows — 66 accept rows, 18 semantic-reject rows (message substring pinned; most
also carry an accepted control), 19 syntax-reject rows. 99 source tests/properties mapped,
6 of them N/A; 3 more are ported only in part (see notes).

## Two harness limits that shape this map

- **CR bytes.** The runner deletes every `\r` from a `.test` file, so no statement can
  contain a CR. Rows that need a CR, NUL, 0x01 or a 1 MB statement build the text with
  `chr()` / `repeat()` and hand it to `query(...)`. `query()` parses its argument through
  the same parser, with `parser_override` active (`SHOW SEMANTIC VIEWS`, which is not DuckDB
  SQL, works inside it), so these are real parser inputs, not string constants. Only
  SELECT-shaped statements (SELECT / DESCRIBE / SHOW) can go through `query()`: a CREATE
  gets "Expected a single SELECT statement". So the CRLF **CREATE bodies** of
  `crlf_body_frontdoor.rs` and the `\r` axis of the prefix-whitespace generators are not
  portable; their LF twins are ported.
- **Per-thread record in `ffi.rs`.** The "one statement per call" diagnosis reads a
  per-thread record of the last multi-statement string `parser_override` saw. The runner
  runs `select ... from duckdb_settings()` on the default connection before **every**
  statement, on the same thread, which overwrites the record. So no `.test` row can observe
  how long the record lives; rows that tried passed vacuously and were removed. Only the
  single-query halves of those tests are ported.

## src/parse/search_path.rs

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| search_path.rs::matching_close_paren_ignores_a_close_paren_inside_a_double_quoted_identifier | F-071 | the `"a)b"` is a struct-field name inside the argument list, the only place a double-quoted identifier can sit in a table-function argument |
| search_path.rs::matching_close_paren_still_ignores_single_and_dollar_quoted_literals | F-069, F-070 | nested-call arm F-069; `'...)...'` and `$$...)...$$` arms F-070 (in a `where_clause` argument; same matcher as the view-name argument) |
| search_path.rs::a_statement_with_no_read_call_is_untouched | F-085 | |
| search_path.rs::a_read_call_gains_the_named_parameter | F-066, F-067 | F-066 is the fixture's control (no path → refused) |
| search_path.rs::existing_named_parameters_are_preserved | F-067 | |
| search_path.rs::every_call_in_one_statement_is_injected | F-068 | |
| search_path.rs::a_nested_call_argument_does_not_confuse_the_paren_match | F-069 | |
| search_path.rs::a_paren_inside_a_string_literal_is_not_the_close | F-070 | |
| search_path.rs::a_function_name_inside_a_string_literal_is_not_a_call | F-085 | |
| search_path.rs::the_all_variants_are_left_alone | F-086 | injecting would be harmless (the `_all` functions accept and ignore `search_path :=`), so the SQL-visible contract pinned is "lists every schema regardless of the path" |
| search_path.rs::injection_is_idempotent | F-077 | runs the already-injected text; a second argument would be "Duplicate named argument" (control in F-076) |
| search_path.rs::a_caller_supplied_search_path_wins | F-075 | |
| search_path.rs::a_caller_supplied_search_path_wins_whatever_its_case | F-076 | |
| search_path.rs::a_view_name_that_merely_contains_the_parameter_text_is_still_injected | F-073, F-068 | |
| search_path.rs::a_where_clause_mentioning_the_parameter_is_not_an_argument | F-074 | |
| search_path.rs::the_function_name_matches_case_insensitively | F-072 | |
| search_path.rs::an_unterminated_call_is_left_alone | F-087 | |
| search_path.rs::the_expression_uses_the_explicit_search_path_not_the_implicit_one | F-092, F-093, F-094 | USE; `dcf_b,main` with a real main.dcf_sp (current_schemas(true) would pick main); empty explicit path falls back to current_schema() |
| search_path.rs::a_leading_line_comment_containing_an_apostrophe_does_not_defeat_injection | F-078 | |
| search_path.rs::a_leading_block_comment_containing_an_apostrophe_does_not_defeat_injection | F-079 | |
| search_path.rs::a_close_paren_inside_a_block_comment_does_not_end_the_call | F-080 | |
| search_path.rs::a_comment_between_the_function_head_and_its_paren_does_not_defeat_injection | F-081 | |
| search_path.rs::a_read_call_named_only_inside_a_comment_is_not_injected | F-083 | |
| search_path.rs::an_explicit_search_path_inside_a_comment_does_not_suppress_injection | F-082 | |
| search_path.rs::nested_resolving_calls_do_not_panic_and_both_get_injected | F-088 | partial: a resolving call nested in another's arguments can only be a subquery, which DuckDB refuses in a table-function argument ("cannot contain subqueries") whatever the injection did; the row pins that the shape fails cleanly, the injection itself is not SQL-observable |
| search_path.rs::injection_is_idempotent_for_the_nested_shape | F-077 | partial: idempotence ported on the flat shape; the nested shape cannot execute (see F-088) |
| search_path.rs::a_leading_line_comment_ended_by_a_bare_carriage_return_does_not_defeat_injection | F-084 | CR built with `chr(13)` and parsed via `query()` (see "harness limits"); LF control alongside |
| search_path.rs (module contract: `RESOLVING_READ_FUNCTIONS`, "What is deliberately NOT rewritten") | F-089, F-090, F-091, F-095 | not a test: rows added so the grammar's structural replacement of the injection is held to the same set of functions; F-090 and F-095 are PRE-EXISTING (#96, #97); F-091 pins the deliberate limit the module doc describes (the scalars take no search path) |

## src/parse/show_clauses.rs

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| show_clauses.rs::limit_zero_is_accepted | F-096 | |
| show_clauses.rs::limit_rejects_negative_with_non_negative_message | F-097, F-098 | `-1` is a well-formed integer out of range (message pinned); `abc` is the wrong token kind (message pinned too) ([text]) |
| show_clauses.rs::filter_suffix_escapes_single_quotes | F-100, F-101 | LIKE / STARTS WITH quotes F-100; IN SCHEMA / IN DATABASE / `<db>.<schema>` quotes F-101 |
| show_clauses.rs::filter_suffix_plain_values_and_limit_unchanged | F-099 | |
| show_clauses.rs::starts_with_prefix_is_literal_not_a_like_pattern | F-102, F-103, F-100 | `_` F-102, `%` F-103, quote F-100; LIKE keeps pattern meaning in F-102 / F-103 |
| show_clauses.rs::starts_with_discriminates_underscore_from_wildcard_in_duckdb | F-102, F-103 | |

## src/parse/ffi.rs

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| ffi.rs::valid_ddl_without_a_declined_query_reports_override_inactive | F-063 | override turned off per session on connection `con2` |
| ffi.rs::ddl_after_another_statement_says_one_statement_per_call | F-061 | a grammar is expected to accept this shape; flip the row then |
| ffi.rs::ddl_before_another_statement_says_one_statement_per_call | F-062 | as F-061 |
| ffi.rs::looks_multi_statement_ignores_trailing_semicolons | F-012, F-013 | F-013 is PRE-EXISTING (`; ;` tail) |
| ffi.rs::unrelated_or_single_declined_query_keeps_override_inactive | N/A | the record's lifetime across statements is not observable: the runner's own settings query before each statement overwrites it (see "harness limits") |
| ffi.rs::record_is_consumed_when_read | F-064 | partial: the multi-statement refusal is ported; the consumption is not observable (as above) |
| ffi.rs::record_is_consumed_by_a_statement_that_is_not_ours | F-065 | partial: `SELEC 1; <DDL>` ends with DuckDB's syntax error; the consumption is not observable (as above) |
| ffi.rs::statement_must_match_a_whole_recorded_statement | N/A | not observable (as above). A related PRE-EXISTING mis-diagnosis was reproduced outside the runner; it is the fourth bullet of TECH-DEBT #98 |
| ffi.rs::strings_without_semantic_are_not_kept | N/A | not observable (as above); the buffer-capacity half is internal |
| ffi.rs::panic_surfaces_as_an_extension_error_not_as_not_ours | N/A | no SQL input reaches the panic arm (no injectable fault) |
| ffi.rs::panic_arm_tolerates_null_out_parameters | N/A | FFI out-parameter contract; no SQL path passes null |
| ffi.rs::non_panicking_body_passes_through_untouched | N/A | guard transparency over a stubbed body; no SQL-observable consequence beyond every other row passing |

## src/parse/detect.rs

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| detect.rs::desc_abbreviation_maps_to_describe | F-006 | |
| detect.rs::describe_full_spelling_still_maps_to_describe | F-005 | |
| detect.rs::desc_requires_word_boundary | F-007 | |

## tests/parse_proptest.rs

Generator axes: keyword case (`arb_case_variant`), leading `[ \t]{0,10}`, trailing `;{0,3}`,
view name `[a-z_][a-z0-9_]{0,29}`, inter-keyword `[ \t\n\r]{1,4}`, body separators
`[ \t\n]{1,4}`, hostile quoted names (ASCII punctuation/uppercase, non-ASCII, keywords,
embedded quote).

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| parse_proptest.rs::detect_create_case_insensitive | F-001 | |
| parse_proptest.rs::detect_create_or_replace_case_insensitive | F-003 | |
| parse_proptest.rs::detect_create_if_not_exists_case_insensitive | F-004 | |
| parse_proptest.rs::detect_drop_case_insensitive | F-010 | |
| parse_proptest.rs::detect_drop_if_exists_case_insensitive | F-009 | |
| parse_proptest.rs::detect_describe_case_insensitive | F-005 | |
| parse_proptest.rs::detect_show_case_insensitive | F-008 | |
| parse_proptest.rs::detect_with_leading_whitespace | F-011 | spaces and tabs, on CREATE / SHOW / DESCRIBE / DROP IF EXISTS |
| parse_proptest.rs::detect_with_trailing_semicolons | F-012 | 1-3 `;` on CREATE / DESCRIBE / SHOW / DROP |
| parse_proptest.rs::detect_non_ddl_returns_none | F-019 | |
| parse_proptest.rs::detect_semantic_view_ddl_returns_detected | F-001, F-003, F-004, F-005, F-008, F-009, F-010 | all seven forms |
| parse_proptest.rs::detect_semantic_view_ddl_returns_not_ours | F-019 | |
| parse_proptest.rs::rewrite_create_forms | F-001, F-003, F-004, F-024 | unqualified name lands in the current schema |
| parse_proptest.rs::rewrite_name_only_forms_carry_name | F-005, F-009, F-010, F-024 | name-edge axis (leading `_`, 30 chars) in F-024 |
| parse_proptest.rs::rewrite_show_views_has_no_target_name | F-008 | |
| parse_proptest.rs::rewrite_quoted_hostile_names | F-026, F-027, F-028, F-029 | ASCII punctuation/uppercase; non-ASCII; keywords (SELECT, a DDL prefix, PRIMARY KEY (id)); embedded `""` |
| parse_proptest.rs::rewrite_unquoted_name_case | F-025 | |
| parse_proptest.rs::position_invariant_paren_body_rejected | F-040 | caret offset not asserted (corpus rule) |
| parse_proptest.rs::position_invariant_missing_name_drop | F-042 | caret offset not asserted |
| parse_proptest.rs::position_invariant_missing_as | F-041 | caret offset not asserted |
| parse_proptest.rs::valid_ddl_with_whitespace_succeeds | F-011 | |
| parse_proptest.rs::near_miss_no_false_positives | F-019 | incl. BEGIN / COMMIT / ROLLBACK |
| parse_proptest.rs::near_miss_detects_transposition | F-020 | all five generator values |
| parse_proptest.rs::near_miss_position_at_start | F-021 | caret offset not asserted |
| parse_proptest.rs::as_body_detected_for_create_forms | F-001, F-003, F-004 | |
| parse_proptest.rs::as_body_validate_and_rewrite_succeeds | F-001, F-002 | Create mode pinned by refusing a second CREATE of the same name |
| parse_proptest.rs::as_body_position_invariant_clause_typo | F-043 | caret offset not asserted |
| parse_proptest.rs::as_body_accepts_any_inter_token_whitespace | F-044 | tab, newline, `\t\n `, `\n\t\n`; an empty-line separator (`\n\n`) ends a `.test` statement, so `\n\t\n` stands in |
| parse_proptest.rs::as_body_clause_keywords_case_insensitive | F-045 | |
| parse_proptest.rs::prefix_whitespace_detection | F-014, F-015, F-016, F-017, F-018 | all seven forms over tab / newline / mixed runs; the `\r` axis is unported (harness limit) |
| parse_proptest.rs::prefix_whitespace_rewrite_roundtrip | F-014, F-015, F-016 | `\r` axis unported |
| parse_proptest.rs::test_large_input_no_panic | F-023 | 1 MB statement via `query(repeat(...))`; the <50 ms budget is not ported |
| parse_proptest.rs::test_null_byte_in_name | F-039 | via `query()` on the DESCRIBE name slot; the CREATE name slot cannot travel through `query()` and a NUL cannot be written into a text `.test` file |
| parse_proptest.rs::test_semicolon_in_name | F-033, F-034, F-035, F-036, F-037, F-038 | each pair with its control; legacy form F-038 |
| parse_proptest.rs::test_unicode_homoglyph | F-022 | |
| parse_proptest.rs::test_control_char_in_name | F-039 | as test_null_byte_in_name |
| parse_proptest.rs::facts_clause_no_panic | F-049, F-050 | the four expression shapes; undeclared alias rejected |
| parse_proptest.rs::empty_facts_clause_valid | F-048 | |
| parse_proptest.rs::derived_metric_parsing_no_panic | F-051, F-052 | distinct names; equal names collide |
| parse_proptest.rs::mixed_metrics_no_panic | F-053, F-054 | |
| parse_proptest.rs::quote_ident_never_panics | F-055 | `quote_ident` reached through the expansion with an embedded-quote member name |
| parse_proptest.rs::relationship_no_cardinality_defaults | F-056 | |
| parse_proptest.rs::metric_using_clause_roundtrip | F-057 | lower / mixed / upper `using` |
| parse_proptest.rs::parse_error_position_is_byte_offset_into_user_input_smoke | F-043 | the three prefixes (none, whitespace, `--` comment); offsets not asserted |
| parse_proptest.rs::position_byte_offset_preserved_for_arbitrary_prefix | F-043 | whitespace + line-comment prefix; offsets not asserted |

## tests/create_front_door_proptest.rs

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| create_front_door_proptest.rs::create_front_door_accepts_hostile_bodies | F-058, F-059, F-060 | axes of `arb_canonical_def`: quoted hostile identifiers + payloads with `--` / `/* */` / `\` / `''` / non-ASCII + e'' escape strings + PRIVATE / FILTER / USING / NON ADDITIVE BY / MATERIALIZATIONS (F-058); bare names, composite PK, explicit and omitted REF lists, three tables, arithmetic and aggregate shapes, OR REPLACE mode (F-059); metrics-only key-less table and dimensions-only table (F-060). The explicit REF list needs a matching UNIQUE on the target at SQL level, which the generator does not draw |

## tests/crlf_body_frontdoor.rs

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| crlf_body_frontdoor.rs::crlf_body_parses_identically_to_lf | F-046 | LF twin only; the CRLF body is unportable (harness limit) |
| crlf_body_frontdoor.rs::crlf_body_error_caret_counts_cr_bytes | F-047 | LF twin of the duplicate-DIMENSIONS reject only; the CR-counting caret is unportable and carets are out of corpus scope |

## tests/quoted_idents_regression.rs

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| quoted_idents_regression.rs::fully_quoted_fqn_normalises_to_bare_name | F-030 | |
| quoted_idents_regression.rs::quoted_fqn_short_parts_normalise_to_bare_name | F-031 | `"db"` → `"memory"` for the accept (single-catalog); the attached-database spelling is pinned as the single-catalog refusal |
| quoted_idents_regression.rs::mixed_quoting_normalises_to_bare_name | F-032 | as F-031 |
