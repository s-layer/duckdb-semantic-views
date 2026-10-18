# Case map S: `src/parse/rewrite.rs` tests (`mod tests`)

Source: all 276 `#[test]` functions in `mod tests` of `src/parse/rewrite.rs`
(the block starts at line 566; submodule tests are listed with their submodule path),
in source order. Corpus files (row ids `S-nnn`, numbered across all three):

- `test/sql/ddl_corpus_statements_a.test`: S-001..S-135: statements that are not ours, DROP,
  DESCRIBE / SHOW COLUMNS / SHOW SEMANTIC VIEWS, statement prefixes and word boundaries,
  comments around statements and names, ALTER, case folding, qualified and quoted names.
- `test/sql/ddl_corpus_statements_b.test`: S-200..S-283: SHOW filter clauses
  (LIKE / IN / STARTS WITH / LIMIT / FOR METRIC), name slots, scope case folding,
  qualified scopes, scope on the other SHOW commands.
- `test/sql/ddl_corpus_create_forms.test`: S-300..S-391: CREATE / OR REPLACE /
  IF NOT EXISTS, the CREATE front door, view COMMENT, name capture, FROM YAML (dollar
  quoting), FROM YAML FILE (fixtures written by COPY to `__TEST_DIR__/ddl_corpus_s_*.yaml`),
  relationship enrichment, and error-message rendering.

Rows tagged `[syntax]` are `statement error` with an empty `----` block (no message pinned);
rows tagged `[text]` pin text that a grammar might plausibly change; `PRE-EXISTING` comments
mark behaviour that looked wrong when the corpus was written (S-014 and S-358, since fixed; S-391,
still open under TECH-DEBT #98). The note column repeats the first
row's description. 6 of 276 tests are N/A.

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| rewrite.rs::sv_parse_function_rust_returns_2_for_select | S-001 | a plain SELECT is not intercepted |
| rewrite.rs::sv_parse_function_rust_returns_2_for_invalid_utf8 | N/A | N/A: feeds raw invalid UTF-8 bytes to the FFI entry point; SQL text in a .test file (and every DuckDB client) is valid UTF-8, so the case cannot be expressed in SQL. |
| rewrite.rs::sv_parse_function_rust_returns_1_with_position_for_malformed_create | S-315 | a misspelled clause keyword (`TABLSE`) ([syntax]) |
| rewrite.rs::sv_parse_function_rust_returns_1_for_near_miss | S-310 | `CRETAE SEMANTIC VIEW` (transposed) ([text]) |
| rewrite.rs::sv_parser_override_rust_returns_2_for_validation_failure | S-315 | a misspelled clause keyword (`TABLSE`) ([syntax]) |
| rewrite.rs::leak_and_reclaim_round_trips_arbitrary_string | N/A | N/A here: FFI heap-buffer round trip of a >64 KB string. Its SQL-level effect (a >64 KB rewritten CREATE commits intact) is pinned by test/integration/test_large_view_rewrite.py; a .test row would need a >64 KB single-line statement. |
| rewrite.rs::test_detect_create | S-300 | plain CREATE stores the AS-body; no COMMENT is stored when none is given |
| rewrite.rs::test_detect_create_or_replace | S-302 | CREATE OR REPLACE overwrites |
| rewrite.rs::test_detect_create_if_not_exists | S-303 | IF NOT EXISTS is a no-op on an existing view and creates a missing one |
| rewrite.rs::test_detect_drop | S-010 | DROP SEMANTIC VIEW removes the view |
| rewrite.rs::test_detect_drop_if_exists | S-012 | DROP IF EXISTS is a no-op on a missing view and drops an existing one |
| rewrite.rs::test_detect_describe | S-020 | DESCRIBE SEMANTIC VIEW lists the view's objects |
| rewrite.rs::test_detect_show | S-022 | SHOW SEMANTIC VIEWS with no clauses lists every view |
| rewrite.rs::test_detect_case_insensitive_all_forms | S-030, S-304 | lower-case DESCRIBE / SHOW / DROP IF EXISTS keywords |
| rewrite.rs::test_detect_whitespace_and_semicolon | S-031 | leading/trailing whitespace and a trailing `;` around DROP and SHOW |
| rewrite.rs::test_detect_non_matching | S-001, S-002, S-005 | a plain SELECT is not intercepted |
| rewrite.rs::test_detect_describe_must_have_view | S-006 | DESCRIBE <table> is DuckDB's DESCRIBE |
| rewrite.rs::test_detect_show_must_have_views | S-007 | SHOW TABLES is DuckDB's, and gets no near-miss suggestion |
| rewrite.rs::test_ddl_kind_create | S-300 | plain CREATE stores the AS-body; no COMMENT is stored when none is given |
| rewrite.rs::test_ddl_kind_create_or_replace | S-302 | CREATE OR REPLACE overwrites |
| rewrite.rs::test_ddl_kind_create_if_not_exists | S-303 | IF NOT EXISTS is a no-op on an existing view and creates a missing one |
| rewrite.rs::test_ddl_kind_drop | S-010 | DROP SEMANTIC VIEW removes the view |
| rewrite.rs::test_ddl_kind_drop_if_exists | S-012 | DROP IF EXISTS is a no-op on a missing view and drops an existing one |
| rewrite.rs::test_ddl_kind_describe | S-020 | DESCRIBE SEMANTIC VIEW lists the view's objects |
| rewrite.rs::test_ddl_kind_show | S-022 | SHOW SEMANTIC VIEWS with no clauses lists every view |
| rewrite.rs::test_ddl_kind_none | S-001 | a plain SELECT is not intercepted |
| rewrite.rs::test_plan_ddl_create_is_unreachable | N/A | N/A: #[should_panic] internal invariant (plan_ddl is never called with CREATE); not reachable from SQL. CREATE routing itself is S-300..S-303. |
| rewrite.rs::test_rewrite_drop | S-010, S-011 | DROP SEMANTIC VIEW removes the view |
| rewrite.rs::test_rewrite_drop_if_exists | S-012 | DROP IF EXISTS is a no-op on a missing view and drops an existing one |
| rewrite.rs::test_rewrite_describe | S-020 | DESCRIBE SEMANTIC VIEW lists the view's objects |
| rewrite.rs::test_rewrite_show | S-022 | SHOW SEMANTIC VIEWS with no clauses lists every view |
| rewrite.rs::test_rewrite_name_with_single_quote | S-014 | an unquoted name containing `'` is captured raw, created and dropped intact ([text]) |
| rewrite.rs::test_unquoted_names_fold_to_lowercase_across_ddl_forms | S-110, S-111, S-112, S-113 | CREATE `Sales` stores `sales` ([text]) |
| rewrite.rs::test_quoted_names_also_fold_to_lowercase_across_ddl_forms | S-114, S-115, S-116 | a quoted CREATE name folds too ([text]) |
| rewrite.rs::test_rewrite_drop_missing_name | S-013 | DROP SEMANTIC VIEW with no name ([syntax]) |
| rewrite.rs::test_prefix_requires_trailing_word_boundary | S-040, S-041, S-042, S-043, S-044, S-314 | `DROP SEMANTIC VIEWS x` / `DROP SEMANTIC VIEWS` are not DROP SEMANTIC VIEW ([syntax]) |
| rewrite.rs::test_prefix_accepts_punctuation_boundaries | S-045, S-046 | `;` and end-of-input end the SHOW keyword |
| rewrite.rs::test_trailing_comment_after_name_is_ignored | S-060 | a trailing `--` comment after the DROP name is not part of it |
| rewrite.rs::test_alter_rename_trailing_comment_not_absorbed | S-061 | a trailing `--` comment is not absorbed into the RENAME TO target |
| rewrite.rs::test_comment_between_prefix_keywords | S-063 | a block comment between the prefix keywords |
| rewrite.rs::test_comment_markers_inside_string_survive | S-062 | `--` and `/* */` inside the SET COMMENT literal are content |
| rewrite.rs::test_nested_block_comment_fully_skipped | S-064 | a nested block comment before the statement is skipped whole |
| rewrite.rs::test_comment_inside_create_body_not_stored | S-326 | a block comment after a dimension expression is not stored with it ([text]) |
| rewrite.rs::test_alter_subops_tolerate_multiple_spaces | S-089 | any run of spaces/tabs between sub-operation keywords |
| rewrite.rs::test_alter_subops_reject_trailing_garbage | S-091, S-092 | trailing tokens after each ALTER sub-operation ([syntax]) |
| rewrite.rs::test_validate_alter_agrees_with_rewriter | S-089, S-090 | any run of spaces/tabs between sub-operation keywords |
| rewrite.rs::test_for_metric_requires_boundaries | S-206, S-225, S-226 | IN <view> FOR METRIC <metric> ([syntax]) |
| rewrite.rs::test_starts_with_and_limit_require_boundaries | S-208, S-227 | SHOW SEMANTIC VIEWS STARTS WITH ... LIMIT ([syntax]) |
| rewrite.rs::test_name_only_forms_reject_trailing_garbage | S-100, S-101 | DROP / DROP IF EXISTS / DESCRIBE / SHOW COLUMNS reject tokens after the name ([syntax]) |
| rewrite.rs::test_rewrite_not_semantic | S-001 | a plain SELECT is not intercepted |
| rewrite.rs::test_basic_detection | S-316 | the retired parenthesised body is rejected ([syntax]) |
| rewrite.rs::test_case_insensitive | S-304 | CREATE keywords in any case |
| rewrite.rs::test_leading_whitespace | S-305 | leading whitespace and a trailing `;` (with or without a space before it) around CREATE |
| rewrite.rs::test_trailing_semicolon | S-305, S-318 | leading whitespace and a trailing `;` (with or without a space before it) around CREATE ([syntax]) |
| rewrite.rs::test_non_matching | S-001, S-002, S-003, S-004, S-005 | a plain SELECT is not intercepted ([syntax]) |
| rewrite.rs::test_too_short | S-312 | `create semantic vie` ([syntax]) |
| rewrite.rs::test_exact_prefix_only | S-313 | the bare prefix with no view name ([syntax]) |
| rewrite.rs::test_validate_and_rewrite_rejects_paren_body | S-316 | the retired parenthesised body is rejected ([syntax]) |
| rewrite.rs::test_validate_and_rewrite_not_ours | S-001 | a plain SELECT is not intercepted |
| rewrite.rs::test_validate_and_rewrite_drop | S-010 | DROP SEMANTIC VIEW removes the view |
| rewrite.rs::test_validate_and_rewrite_show | S-022 | SHOW SEMANTIC VIEWS with no clauses lists every view |
| rewrite.rs::test_validate_and_rewrite_describe | S-020 | DESCRIBE SEMANTIC VIEW lists the view's objects |
| rewrite.rs::test_validate_and_rewrite_drop_if_exists | S-012 | DROP IF EXISTS is a no-op on a missing view and drops an existing one |
| rewrite.rs::test_near_miss_creat | S-311 | `CREAT SEMANTIC VIEW` (dropped letter) ([text]) |
| rewrite.rs::test_near_miss_drop_semantc | S-050 | `DROP SEMANTC VIEW` is rejected with a suggestion ([text]) |
| rewrite.rs::test_near_miss_show_semantic_view | S-051 | `SHOW SEMANTIC VIEW` (singular) is rejected ([syntax]) |
| rewrite.rs::test_near_miss_select | S-001 | a plain SELECT is not intercepted |
| rewrite.rs::test_near_miss_show_tables | S-007 | SHOW TABLES is DuckDB's, and gets no near-miss suggestion |
| rewrite.rs::test_near_miss_position_zero | S-311 | `CREAT SEMANTIC VIEW` (dropped letter) ([text]) |
| rewrite.rs::test_parse_error_position_paren_body_rejected | S-316 | the retired parenthesised body is rejected ([syntax]) |
| rewrite.rs::test_parse_error_position_structural | S-313 | the bare prefix with no view name ([syntax]) |
| rewrite.rs::parse_error_position_points_at_offending_token | S-087, S-091, S-100, S-129, S-221, S-225 | an unsupported ALTER sub-operation ([text]) |
| rewrite.rs::parse_error_position_survives_leading_comment | S-071 | leading comment, then an unknown SHOW clause token ([syntax]) |
| rewrite.rs::phase25_parse_tests::as_body_create_rewrites_to_from_json | S-300, S-301 | plain CREATE stores the AS-body; no COMMENT is stored when none is given |
| rewrite.rs::phase25_parse_tests::as_body_create_or_replace_rewrites_to_from_json | S-302 | CREATE OR REPLACE overwrites |
| rewrite.rs::phase25_parse_tests::as_body_create_if_not_exists_rewrites_to_from_json | S-303 | IF NOT EXISTS is a no-op on an existing view and creates a missing one |
| rewrite.rs::phase25_parse_tests::old_paren_body_is_rejected | S-316 | the retired parenthesised body is rejected ([syntax]) |
| rewrite.rs::phase25_parse_tests::drop_still_rewrites_unchanged | S-010 | DROP SEMANTIC VIEW removes the view |
| rewrite.rs::phase33_inference_tests::resolves_ref_columns_to_target_pk | S-380 | `REFERENCES c` with no column list resolves to c's PRIMARY KEY |
| rewrite.rs::phase33_inference_tests::keeps_explicit_ref_columns | S-381 | an explicit `REFERENCES c(email)` column list is kept |
| rewrite.rs::phase33_inference_tests::errors_when_target_has_no_pk_and_no_explicit_ref | S-386 | `REFERENCES c` where c declares no key is refused |
| rewrite.rs::phase33_inference_tests::enrich_rejects_cross_kind_name_collision | S-388 | a metric named like a dimension (differing only in case) collides |
| rewrite.rs::phase33_inference_tests::infers_one_to_one_from_pk_match | S-383 | an FK equal to the source's PRIMARY KEY is one-to-one: the same query is allowed |
| rewrite.rs::phase33_inference_tests::infers_one_to_one_from_unique_match | S-384 | an FK equal to a UNIQUE key of the source is one-to-one |
| rewrite.rs::phase33_inference_tests::infers_many_to_one_when_fk_is_bare | S-382 | an FK that is not the source's PK/UNIQUE is many-to-one: a metric on the target fans out under a source dimension |
| rewrite.rs::phase33_inference_tests::case_insensitive_column_matching | S-385 | PRIMARY KEY (ID) matches FK (id) regardless of case ([text]) |
| rewrite.rs::phase33_inference_tests::fk_ref_column_count_mismatch_error | S-387 | a one-column FK against a two-column PRIMARY KEY is refused |
| rewrite.rs::phase33_inference_tests::rewrite_produces_json_with_ref_columns_and_cardinality | S-380 | `REFERENCES c` with no column list resolves to c's PRIMARY KEY |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_normal | S-093 | the SET COMMENT literal: plain, and empty |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_escaped_quotes | S-082 | `''` in the comment literal stores one `'` |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_empty | S-093 | the SET COMMENT literal: plain, and empty |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_unterminated | S-098, S-229 | an unterminated SET COMMENT literal ([syntax]) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_no_opening_quote | S-099, S-229 | SET COMMENT = <bare word> ([syntax]) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_utf8_cyrillic | S-094 | a Cyrillic SET COMMENT round-trips byte-exact |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_utf8_cjk_and_emoji | S-095 | CJK and emoji in SET COMMENT |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_utf8_em_dash_and_smart_quotes | S-096 | em dash and smart quotes in SET COMMENT |
| rewrite.rs::phase34_1_1_show_filter_tests::test_extract_quoted_string_utf8_escaped_quotes_around_nonascii | S-097 | `''` escapes next to non-ASCII text |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_like_only | S-200 | LIKE with no IN filters dimensions across every view |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_starts_with_only | S-202 | STARTS WITH then LIMIT 1 across views |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_limit_only | S-203 | LIMIT alone |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_all_three | S-209 | LIKE, STARTS WITH and LIMIT on SHOW SEMANTIC VIEWS |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_none | S-022 | SHOW SEMANTIC VIEWS with no clauses lists every view |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_reescapes_quotes | S-216 | a `''` in the LIKE pattern matches a name containing `'` |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_in_schema | S-210 | IN SCHEMA keeps only that schema (`dcs_sch.sch_sv` is excluded) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_in_database | S-211 | IN DATABASE |
| rewrite.rs::phase34_1_1_show_filter_tests::test_build_filter_suffix_like_and_schema | S-215 | LIKE before IN SCHEMA |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_dims_like_cross_view | S-200 | LIKE with no IN filters dimensions across every view |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_dims_like_in_starts_with_limit | S-201 | LIKE, IN <view>, STARTS WITH and LIMIT compose (LIMIT cuts 3 matches to 2) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_metrics_starts_with_limit | S-202 | STARTS WITH then LIMIT 1 across views |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_facts_limit | S-203 | LIMIT alone |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_dims_for_metric_with_all_clauses | S-204 | LIKE, IN, FOR METRIC, STARTS WITH, LIMIT all together |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_dims_like_after_in_error | S-220 | LIKE after IN is out of clause order ([syntax]) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_metrics_limit_non_numeric | S-221 | a non-numeric LIMIT ([syntax]) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_for_metric_on_metrics_error | S-222 | FOR METRIC on SHOW SEMANTIC METRICS ([syntax]) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_case_insensitive | S-205 | lower-case SHOW keywords and clauses |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_like | S-207 | SHOW SEMANTIC VIEWS LIKE |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_starts_with_limit | S-208 | SHOW SEMANTIC VIEWS STARTS WITH ... LIMIT |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_all_clauses | S-209 | LIKE, STARTS WITH and LIMIT on SHOW SEMANTIC VIEWS |
| rewrite.rs::phase34_1_1_show_filter_tests::test_show_views_non_ascii_trailing_tokens_error_not_panic | S-228 | non-ASCII trailing tokens on every SHOW form are a clean error ([syntax]) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_show_views_like_non_ascii_pattern_roundtrips | S-217 | a non-ASCII LIKE pattern round-trips |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_in_requires_schema_or_database | S-224 | SHOW SEMANTIC VIEWS IN <name> (no SCHEMA/DATABASE/ACCOUNT) ([syntax]) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_in_schema | S-210 | IN SCHEMA keeps only that schema (`dcs_sch.sch_sv` is excluded) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_in_database | S-211 | IN DATABASE |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_terse | S-212 | SHOW TERSE SEMANTIC VIEWS (no comment column) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_terse_like | S-213 | SHOW TERSE ... LIKE |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_terse_in_schema | S-214 | SHOW TERSE ... IN SCHEMA |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_in_schema_like | S-215 | LIKE before IN SCHEMA |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_columns_in_semantic_view | S-021 | SHOW COLUMNS IN SEMANTIC VIEW lists dimensions and metrics |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_for_metric_error | S-223 | FOR METRIC on SHOW SEMANTIC VIEWS ([syntax]) |
| rewrite.rs::phase34_1_1_show_filter_tests::test_rewrite_show_views_no_clauses_regression | S-022 | SHOW SEMANTIC VIEWS with no clauses lists every view |
| rewrite.rs::detect_show_materializations | S-218 | SHOW SEMANTIC MATERIALIZATIONS across views |
| rewrite.rs::detect_show_materializations_in_view | S-219 | SHOW SEMANTIC MATERIALIZATIONS IN <view> |
| rewrite.rs::rewrite_show_materializations_all | S-218 | SHOW SEMANTIC MATERIALIZATIONS across views |
| rewrite.rs::rewrite_show_materializations_in_view | S-219 | SHOW SEMANTIC MATERIALIZATIONS IN <view> |
| rewrite.rs::near_miss_show_materialization | S-052 | `SHOW SEMANTIC MATERIALIZATION` (singular) is rejected ([text]) |
| rewrite.rs::phase43_view_comment_tests::test_view_comment_parsed | S-320 | COMMENT = '...' before AS |
| rewrite.rs::phase43_view_comment_tests::test_view_without_comment | S-300 | plain CREATE stores the AS-body; no COMMENT is stored when none is given |
| rewrite.rs::phase43_view_comment_tests::test_view_comment_escaped_quotes | S-321 | `''` in the view COMMENT stores one `'` |
| rewrite.rs::phase43_view_comment_tests::test_view_comment_with_create_or_replace | S-322 | OR REPLACE with a COMMENT replaces the comment too |
| rewrite.rs::test_detect_ddl_kind_alter_set_comment | S-081 | ALTER ... SET COMMENT |
| rewrite.rs::test_detect_ddl_kind_alter_if_exists_set_comment | S-084 | ALTER IF EXISTS ... SET COMMENT sets it on an existing view and is a no-op on a missing one |
| rewrite.rs::test_detect_ddl_kind_alter_unset_comment | S-083 | ALTER ... UNSET COMMENT |
| rewrite.rs::test_detect_ddl_kind_alter_if_exists_unset_comment | S-085 | ALTER IF EXISTS ... UNSET COMMENT on an existing and a missing view |
| rewrite.rs::test_detect_ddl_kind_alter_rename_backwards_compat | S-080 | ALTER ... RENAME TO |
| rewrite.rs::test_validate_rewrite_alter_set_comment | S-081, S-086 | ALTER ... SET COMMENT |
| rewrite.rs::test_validate_rewrite_alter_unset_comment | S-083, S-086 | ALTER ... UNSET COMMENT |
| rewrite.rs::test_validate_rewrite_alter_if_exists_set_comment | S-084 | ALTER IF EXISTS ... SET COMMENT sets it on an existing view and is a no-op on a missing one |
| rewrite.rs::test_validate_rewrite_alter_if_exists_unset_comment | S-085 | ALTER IF EXISTS ... UNSET COMMENT on an existing and a missing view |
| rewrite.rs::test_validate_rewrite_alter_rename_unchanged | S-080 | ALTER ... RENAME TO |
| rewrite.rs::test_validate_rewrite_alter_unsupported_operation | S-087 | an unsupported ALTER sub-operation ([text]) |
| rewrite.rs::test_validate_rewrite_alter_set_comment_escaped_quotes | S-082 | `''` in the comment literal stores one `'` |
| rewrite.rs::test_validate_rewrite_alter_missing_operation | S-088 | ALTER with a name but no sub-operation ([syntax]) |
| rewrite.rs::test_extract_dollar_quoted_untagged | S-340 | CREATE ... FROM YAML $$ <multi-line YAML> $$ |
| rewrite.rs::test_extract_dollar_quoted_tagged | S-347 | a tagged dollar quote `$yaml$ ... $yaml$` |
| rewrite.rs::test_extract_dollar_quoted_empty_content | S-350 | `$$$$` is an empty YAML document, rejected as a definition |
| rewrite.rs::test_extract_dollar_quoted_no_leading_dollar | S-353 | FROM YAML not followed by a dollar quote ([syntax]) |
| rewrite.rs::test_extract_dollar_quoted_unterminated_opening | S-354 | `$tag` with no closing `$` on the opening delimiter ([syntax]) |
| rewrite.rs::test_extract_dollar_quoted_unterminated_body | S-355 | `$$` body with no closing `$$` ([syntax]) |
| rewrite.rs::test_extract_dollar_quoted_rejects_invalid_tag | S-356 | a digit-led tag (`$1$`) and a tag with a space (`$ta g$`) ([text]) |
| rewrite.rs::test_extract_dollar_quoted_inner_dollar | S-352 | the first closing `$$` ends the body, so a later `$$ text$$` is trailing content ([syntax]) |
| rewrite.rs::test_extract_dollar_quoted_multiline | S-340 | CREATE ... FROM YAML $$ <multi-line YAML> $$ |
| rewrite.rs::test_yaml_rewrite_basic_create | S-340 | CREATE ... FROM YAML $$ <multi-line YAML> $$ |
| rewrite.rs::test_yaml_rewrite_create_or_replace | S-343, S-359 | CREATE OR REPLACE ... FROM YAML replaces ([syntax]) |
| rewrite.rs::test_yaml_rewrite_create_if_not_exists | S-344 | CREATE IF NOT EXISTS ... FROM YAML is a no-op on an existing view and creates a missing one |
| model.rs::yaml_with_no_tables_is_rejected | S-358 | an empty YAML definition (`tables: []`) is rejected like `TABLES ()` (TECH-DEBT #94) |
| rewrite.rs::test_yaml_rewrite_trailing_content_rejected | S-351 | text after the closing `$$` ([syntax]) |
| rewrite.rs::test_yaml_rewrite_invalid_yaml | S-349 | malformed YAML content names the view in the error |
| rewrite.rs::test_yaml_rewrite_comment_override | S-346 | the DDL COMMENT wins over a `comment:` key in the YAML; without one the YAML comment is used |
| rewrite.rs::test_yaml_rewrite_base_table_populated | S-348 | an empty `base_table` is filled from the first table entry |
| rewrite.rs::test_yaml_rewrite_tagged_dollar_quote | S-347 | a tagged dollar quote `$yaml$ ... $yaml$` |
| rewrite.rs::test_from_yaml_detection_via_rewrite_ddl | S-340 | CREATE ... FROM YAML $$ <multi-line YAML> $$ |
| rewrite.rs::test_from_yaml_case_insensitive | S-341 | `from yaml` and `From Yaml` |
| rewrite.rs::test_from_yaml_mixed_case | S-341 | `from yaml` and `From Yaml` |
| rewrite.rs::test_from_yaml_flexible_whitespace | S-342 | two spaces, a tab, a block comment, or a newline between FROM and YAML |
| rewrite.rs::test_error_message_mentions_from_yaml | S-317 | neither AS nor FROM YAML after the name ([text]) |
| rewrite.rs::test_create_or_replace_from_yaml | S-343 | CREATE OR REPLACE ... FROM YAML replaces |
| rewrite.rs::test_create_if_not_exists_from_yaml | S-344 | CREATE IF NOT EXISTS ... FROM YAML is a no-op on an existing view and creates a missing one |
| rewrite.rs::test_comment_with_from_yaml | S-345 | COMMENT = '...' before FROM YAML |
| rewrite.rs::test_extract_single_quoted_basic | S-360 | CREATE ... FROM YAML FILE '<path>' |
| rewrite.rs::test_extract_single_quoted_escaped | S-365 | `''` in the file path is one `'` |
| rewrite.rs::test_extract_single_quoted_empty | S-369 | an empty file path is refused |
| rewrite.rs::test_extract_single_quoted_no_quote | S-371 | an unquoted file path ([syntax]) |
| rewrite.rs::test_extract_single_quoted_unterminated | S-372 | an unterminated file path literal ([syntax]) |
| rewrite.rs::test_extract_single_quoted_utf8_cjk_path | S-366 | a CJK file path round-trips |
| rewrite.rs::test_extract_single_quoted_utf8_accented_path | S-367 | an accented file path round-trips |
| rewrite.rs::test_extract_single_quoted_utf8_emoji_path | S-368 | an emoji file path round-trips |
| rewrite.rs::test_extract_view_comment_utf8_cyrillic | S-323 | a Cyrillic view COMMENT |
| rewrite.rs::test_extract_view_comment_utf8_emoji_and_em_dash | S-324 | emoji and em dash in the view COMMENT, followed by AS |
| rewrite.rs::test_extract_view_comment_utf8_escaped_quotes_around_nonascii | S-325 | `''` escapes next to non-ASCII text in the view COMMENT |
| rewrite.rs::test_rewrite_ddl_yaml_file_body_create | S-360, S-364 | CREATE ... FROM YAML FILE '<path>' |
| rewrite.rs::test_rewrite_ddl_yaml_file_body_replace | S-362 | OR REPLACE ... COMMENT ... FROM YAML FILE replaces, and the DDL comment wins over the file's |
| rewrite.rs::test_rewrite_ddl_yaml_file_body_if_not_exists | S-363 | IF NOT EXISTS ... FROM YAML FILE is a no-op on an existing view and creates a missing one |
| rewrite.rs::test_rewrite_ddl_yaml_file_body_with_comment | S-362 | OR REPLACE ... COMMENT ... FROM YAML FILE replaces, and the DDL comment wins over the file's |
| rewrite.rs::test_rewrite_ddl_yaml_file_body_empty_path | S-369 | an empty file path is refused |
| rewrite.rs::test_rewrite_ddl_yaml_file_body_trailing_content | S-370 | text after the file path ([syntax]) |
| rewrite.rs::test_validate_and_rewrite_yaml_file | S-360 | CREATE ... FROM YAML FILE '<path>' |
| rewrite.rs::test_validate_and_rewrite_yaml_file_case_insensitive | S-361 | `from yaml file` in lower case |
| rewrite.rs::test_validate_and_rewrite_yaml_inline_still_works | S-340 | CREATE ... FROM YAML $$ <multi-line YAML> $$ |
| rewrite.rs::test_error_message_mentions_from_yaml_file | S-317 | neither AS nor FROM YAML after the name ([text]) |
| rewrite.rs::skip_lws_empty | N/A | N/A: byte offset returned by the skip_leading_whitespace helper; the user-visible effect (leading whitespace before a statement) is S-031 / S-305. |
| rewrite.rs::skip_lws_only_whitespace | N/A | N/A: byte offset returned by the skip_leading_whitespace helper; a whitespace-only statement never reaches the hook with a body (see S-005 for the empty statement). |
| rewrite.rs::skip_lws_no_leading_match | N/A | N/A: byte offset returned by the skip_leading_whitespace helper (offset 0 with no leading whitespace); every unindented statement in the corpus exercises it. |
| rewrite.rs::skip_lws_blanked_comment_is_whitespace | S-307 | a leading line comment on its own line before CREATE |
| rewrite.rs::detect_create_with_leading_block_comment | S-306 | a leading block comment before CREATE |
| rewrite.rs::detect_create_with_leading_line_comment | S-307 | a leading line comment on its own line before CREATE |
| rewrite.rs::detect_create_or_replace_with_dbt_style_annotation | S-308 | a dbt-style JSON annotation comment before CREATE OR REPLACE keeps the OR REPLACE mode |
| rewrite.rs::detect_other_ddl_forms_with_leading_comment | S-065, S-066, S-067, S-068 | a leading block comment before DESCRIBE / SHOW / SHOW ... IN |
| rewrite.rs::comment_only_is_not_semantic_view_ddl | S-069 | a statement that is only a comment is not ours and is a no-op |
| rewrite.rs::validate_and_rewrite_with_leading_comment_succeeds | S-067 | a leading block comment before DROP |
| rewrite.rs::error_position_accounts_for_leading_comment | S-070 | leading comment, then DROP with no name ([syntax]) |
| rewrite.rs::write_error_to_buffer_truncates_at_char_boundary | S-391 | an error message longer than the 1024-byte hook buffer is cut at a UTF-8 character boundary, so it still decodes (the dimension name is 600 two-byte `é`; tod... |
| rewrite.rs::write_error_to_buffer_handles_short_string | S-390 | a short validation error reaches the user whole |
| rewrite.rs::phase64_quoted_ident_tests::drop_with_quoted_fqn | S-122 | DROP with a quoted three-part name drops only that schema's view |
| rewrite.rs::phase64_quoted_ident_tests::drop_with_quoted_bare | S-126 | DROP with a quoted one-part name |
| rewrite.rs::phase64_quoted_ident_tests::drop_with_unquoted_fqn | S-123, S-124 | DROP with an unquoted three-part name keeps its qualifier |
| rewrite.rs::phase64_quoted_ident_tests::drop_with_partial_quoting | S-127 | DROP with an unquoted schema and quoted name |
| rewrite.rs::phase64_quoted_ident_tests::drop_with_quoted_whitespace_name | S-128 | a quoted name with an inner space is captured whole |
| rewrite.rs::phase64_quoted_ident_tests::drop_if_exists_with_quoted_fqn | S-125 | DROP IF EXISTS with a quoted three-part name |
| rewrite.rs::phase64_quoted_ident_tests::describe_with_quoted_fqn | S-120 | DESCRIBE with a quoted three-part name reads that schema's view |
| rewrite.rs::phase64_quoted_ident_tests::show_columns_with_quoted_fqn | S-121 | SHOW COLUMNS with a quoted three-part name |
| rewrite.rs::phase64_quoted_ident_tests::drop_with_unterminated_quote_errors | S-129 | an unterminated quoted DROP name ([syntax]) |
| rewrite.rs::phase64_quoted_ident_tests::create_with_quoted_fqn_captures_schema_and_bare_name | S-330, S-336 | a quoted three-part name lands in that schema under the bare name |
| rewrite.rs::phase64_quoted_ident_tests::create_or_replace_with_quoted_fqn_captures_schema_and_bare_name | S-331 | OR REPLACE with a quoted three-part name replaces that schema's view |
| rewrite.rs::phase64_quoted_ident_tests::create_if_not_exists_with_quoted_fqn_captures_schema_and_bare_name | S-332 | IF NOT EXISTS with a quoted three-part name sees that schema's view and does nothing |
| rewrite.rs::phase64_quoted_ident_tests::create_with_partial_quoting_captures_schema_and_bare_name | S-333 | unquoted schema, quoted name: a second `orders_sv`, in `main` |
| rewrite.rs::phase64_quoted_ident_tests::create_with_quoted_whitespace_name_extracts_intact | S-335 | one quoted part with a space is the name, not a qualifier |
| rewrite.rs::phase64_quoted_ident_tests::create_with_unterminated_quote_errors | S-337 | an unterminated quoted CREATE name ([syntax]) |
| rewrite.rs::phase64_quoted_ident_tests::alter_rename_source_quoted | S-130 | ALTER with a quoted source name |
| rewrite.rs::phase64_quoted_ident_tests::alter_rename_target_quoted | S-131 | a quoted three-part RENAME TO target moves the view into that schema |
| rewrite.rs::phase64_quoted_ident_tests::alter_rename_both_quoted | S-132 | quoted three-part source and target |
| rewrite.rs::phase64_quoted_ident_tests::alter_set_comment_with_quoted_source | S-133 | SET COMMENT with a quoted source name |
| rewrite.rs::phase64_quoted_ident_tests::alter_unset_comment_with_quoted_source | S-134 | UNSET COMMENT with a quoted source name |
| rewrite.rs::phase64_quoted_ident_tests::alter_rename_target_unterminated_quote_errors | S-135 | an unterminated quoted RENAME TO target ([syntax]) |
| rewrite.rs::phase64_quoted_ident_tests::create_quoted_fqn_captures_bare_name | S-330 | a quoted three-part name lands in that schema under the bare name |
| rewrite.rs::phase64_quoted_ident_tests::create_mixed_quoting_captures_bare_name | S-334 | mixed quoting across three parts |
| rewrite.rs::phase64_quoted_ident_tests::create_with_four_part_name_errors | S-338 | a four-part name is not truncated to its last part ([text]) |
| rewrite.rs::show_name_slot_quoted_ident_tests::in_schema_accepts_a_quoted_name_containing_whitespace | S-230 | IN SCHEMA "<name with space>" |
| rewrite.rs::show_name_slot_quoted_ident_tests::in_database_accepts_a_quoted_name_containing_whitespace | S-231 | IN DATABASE "<name with space>" parses; no such database, so no rows |
| rewrite.rs::show_name_slot_quoted_ident_tests::in_view_accepts_a_quoted_name_containing_whitespace | S-232 | IN "<view with space>" |
| rewrite.rs::show_name_slot_quoted_ident_tests::for_metric_accepts_a_quoted_name_containing_whitespace | S-233 | FOR METRIC "<metric with space>" |
| rewrite.rs::show_name_slot_quoted_ident_tests::a_quoted_view_name_containing_a_dot_keeps_its_quotes | S-234 | IN "a.b" names the view literally called `a.b`, not view `b` ([text]) |
| rewrite.rs::show_name_slot_quoted_ident_tests::an_unquoted_dot_is_a_qualifier_but_a_quoted_one_is_not | S-235 | an unquoted dot in IN <view> is a schema qualifier |
| rewrite.rs::show_name_slot_quoted_ident_tests::in_schema_strips_quotes_before_emitting_the_literal | S-236 | a quoted IN SCHEMA name ("dcs_sch") matches the bare schema name |
| rewrite.rs::show_name_slot_quoted_ident_tests::in_database_strips_quotes_before_emitting_the_literal | S-237 | IN DATABASE "memory" matches database `memory` |
| rewrite.rs::show_name_slot_quoted_ident_tests::a_doubled_quote_inside_a_quoted_schema_name_unescapes | S-238 | `""` inside a quoted schema name is one `"` ([text]) |
| rewrite.rs::show_name_slot_quoted_ident_tests::a_quoted_schema_name_may_contain_a_semicolon | S-239 | `;` inside a quoted schema name |
| rewrite.rs::show_name_slot_quoted_ident_tests::a_quoted_schema_name_survives_a_single_quote_escape | S-240 | `'` inside a quoted schema name |
| rewrite.rs::show_name_slot_quoted_ident_tests::a_quoted_schema_name_cannot_break_out_of_its_literal | S-241 | a quoted schema name carrying `'; DROP TABLE ...; --` stays a name |
| rewrite.rs::show_name_slot_quoted_ident_tests::an_unquoted_schema_name_is_unchanged | S-210 | IN SCHEMA keeps only that schema (`dcs_sch.sch_sv` is excluded) |
| rewrite.rs::show_name_slot_quoted_ident_tests::an_unquoted_view_and_metric_are_unchanged | S-206 | IN <view> FOR METRIC <metric> |
| rewrite.rs::show_name_slot_quoted_ident_tests::a_quoted_name_still_composes_with_the_other_clauses | S-242 | LIKE before and LIMIT after a quoted IN SCHEMA name |
| rewrite.rs::show_name_slot_quoted_ident_tests::an_unterminated_quote_in_a_schema_name_still_errors | S-243 | IN SCHEMA "<unterminated> ([syntax]) |
| rewrite.rs::show_scope_case_folding_tests::in_schema_folds_case_on_both_sides | S-250 | a mixed-case IN SCHEMA name finds views created under either spelling ([text]) |
| rewrite.rs::show_scope_case_folding_tests::in_database_folds_case_on_both_sides | S-252 | a mixed-case IN DATABASE name ([text]) |
| rewrite.rs::show_scope_case_folding_tests::a_quoted_scope_name_folds_exactly_like_an_unquoted_one | S-251 | the quoted spelling returns the same rows ([text]) |
| rewrite.rs::show_scope_case_folding_tests::the_fold_is_left_to_sql_rather_than_applied_in_rust | S-253 | a non-ASCII schema name folds Unicode-aware (`É` matches `é`) ([text]) |
| rewrite.rs::show_scope_qualified_name_tests::a_qualified_schema_name_filters_on_database_and_schema | S-260 | IN SCHEMA memory.dcs_sch |
| rewrite.rs::show_scope_qualified_name_tests::a_quoted_qualified_schema_name_splits_on_the_unquoted_dot | S-261 | IN SCHEMA "memory"."dcs my schema" |
| rewrite.rs::show_scope_qualified_name_tests::a_three_part_schema_name_is_rejected | S-263 | IN SCHEMA a.b.c ([text]) |
| rewrite.rs::show_scope_qualified_name_tests::a_qualified_database_name_is_rejected | S-264 | IN DATABASE a.b ([text]) |
| rewrite.rs::show_scope_qualified_name_tests::an_unqualified_schema_name_emits_no_database_predicate | S-210 | IN SCHEMA keeps only that schema (`dcs_sch.sch_sv` is excluded) |
| rewrite.rs::show_scope_qualified_name_tests::a_quoted_dot_is_not_a_qualifier | S-262 | IN SCHEMA "dcs_a.b" is one schema name ([text]) |
| rewrite.rs::show_scope_other_commands_tests::a_schema_named_startswith_is_not_the_starts_with_clause | S-270 | a schema named `startswith` is a name, not STARTS WITH |
| rewrite.rs::show_scope_other_commands_tests::a_real_starts_with_clause_after_a_bare_scope_still_parses | S-271 | bare IN SCHEMA then STARTS WITH |
| rewrite.rs::show_scope_other_commands_tests::in_schema_scopes_show_dimensions | S-272 | SHOW SEMANTIC DIMENSIONS IN SCHEMA |
| rewrite.rs::show_scope_other_commands_tests::in_schema_scopes_show_metrics | S-273 | SHOW SEMANTIC METRICS IN SCHEMA |
| rewrite.rs::show_scope_other_commands_tests::in_schema_scopes_show_facts | S-274 | SHOW SEMANTIC FACTS IN SCHEMA |
| rewrite.rs::show_scope_other_commands_tests::in_database_scopes_show_dimensions | S-275 | SHOW SEMANTIC DIMENSIONS IN DATABASE |
| rewrite.rs::show_scope_other_commands_tests::a_qualified_schema_scopes_show_dimensions | S-276 | SHOW SEMANTIC DIMENSIONS IN SCHEMA <db>.<schema> |
| rewrite.rs::show_scope_other_commands_tests::in_account_emits_no_scope_predicate | S-277 | IN ACCOUNT lists everything |
| rewrite.rs::show_scope_other_commands_tests::in_account_scopes_show_dimensions_too | S-278 | SHOW SEMANTIC DIMENSIONS IN ACCOUNT |
| rewrite.rs::show_scope_other_commands_tests::a_bare_in_schema_scopes_to_the_current_schema | S-279 | bare IN SCHEMA is the current schema |
| rewrite.rs::show_scope_other_commands_tests::a_bare_in_database_scopes_to_the_current_database | S-282 | bare IN DATABASE is the current database |
| rewrite.rs::show_scope_other_commands_tests::a_bare_in_schema_on_dimensions_is_the_scope_not_a_view_named_schema | S-280 | bare IN SCHEMA on DIMENSIONS is the current schema, not a view called `schema` |
| rewrite.rs::show_scope_other_commands_tests::a_quoted_view_named_schema_is_still_reachable | S-281 | quoting forces the view-name reading |
| rewrite.rs::show_scope_other_commands_tests::an_ordinary_view_name_still_selects_the_single_view_function | S-283 | IN <ordinary view name> |
| rewrite.rs::show_scope_other_commands_tests::for_metric_still_composes_with_a_view_name | S-206 | IN <view> FOR METRIC <metric> |
