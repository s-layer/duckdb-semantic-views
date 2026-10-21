# Phase 3 corpus: case map, slice C

Rust unit tests in `src/body_parser/mod.rs` (the `mod tests` block from line 2570,
`references_multi_column_list`, to the end of the file), `annotations.rs`,
`clause_bounds.rs`, `cursor.rs`, `lexer.rs` and `window.rs`, in source order, with the
SQL corpus rows that carry each case. Rows C-001..C-079 are in
`test/sql/ddl_corpus_body_b1.test`; C-080 onwards are in `test/sql/ddl_corpus_body_b2.test`.

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| `src/body_parser/mod.rs::references_multi_column_list` | C-001 |  |
| `src/body_parser/mod.rs::references_target_no_space_before_paren` | C-002 |  |
| `src/body_parser/mod.rs::references_target_space_before_paren` | C-003 |  |
| `src/body_parser/mod.rs::parse_relationships_quoted_paren_in_from_alias` | C-004 |  |
| `src/body_parser/mod.rs::parse_relationships_multi_token_from_alias_rejected` | C-005, C-006 |  |
| `src/body_parser/mod.rs::parse_relationships_junk_before_references_rejected` | C-007 |  |
| `src/body_parser/mod.rs::unique_constraint_parsing` | C-008 |  |
| `src/body_parser/mod.rs::multiple_unique_constraints` | C-009 |  |
| `src/body_parser/mod.rs::table_without_primary_key` | C-010 |  |
| `src/body_parser/mod.rs::table_with_unique_no_pk` | C-011 |  |
| `src/body_parser/mod.rs::parse_qualified_entries_simple` | C-012 | Ported into DIMENSIONS as written. PRE-EXISTING: the aggregate dimension is accepted at CREATE but unqueryable. |
| `src/body_parser/mod.rs::parse_qualified_entries_nested_parens` | C-013 | Row-level `ROUND(price * (1 - discount))` in FACTS stands in for the aggregate (same nesting). |
| `src/body_parser/mod.rs::parse_qualified_entries_trailing_comma` | C-014 |  |
| `src/body_parser/mod.rs::parse_qualified_entries_multiple_with_trailing_comma` | C-015 |  |
| `src/body_parser/mod.rs::parse_qualified_entries_error_missing_dot` | C-016 |  |
| `src/body_parser/mod.rs::parse_keyword_body_basic` | C-017 |  |
| `src/body_parser/mod.rs::parse_keyword_body_large_metrics_clause` | C-018 |  |
| `src/body_parser/mod.rs::parse_keyword_body_deeply_nested_parens` | C-019 |  |
| `src/body_parser/mod.rs::parse_keyword_body_with_relationships` | C-020 |  |
| `src/body_parser/mod.rs::parse_keyword_body_empty_relationships` | C-021 |  |
| `src/body_parser/mod.rs::parse_keyword_body_error_missing_tables` | C-022 |  |
| `src/body_parser/mod.rs::parse_keyword_body_newlines_between_clauses` | C-023 |  |
| `src/body_parser/mod.rs::parse_keyword_body_tabs_between_tokens` | C-024 |  |
| `src/body_parser/mod.rs::parse_keyword_body_extra_spaces` | C-025 |  |
| `src/body_parser/mod.rs::parse_keyword_body_mixed_whitespace_multientry` | C-026 | A relationship is added so the two tables are connected. |
| `src/body_parser/mod.rs::parse_tables_extra_whitespace_around_tokens` | C-027 |  |
| `src/body_parser/mod.rs::parse_relationships_newline_separated` | C-028 |  |
| `src/body_parser/mod.rs::parse_qualified_entries_newline_separated` | C-029 | Ported into FACTS (row-level expressions). |
| `src/body_parser/mod.rs::parse_keyword_body_lowercase_clause_keywords` | C-030 |  |
| `src/body_parser/mod.rs::parse_keyword_body_mixedcase_clause_keywords` | C-031 |  |
| `src/body_parser/mod.rs::parse_tables_lowercase_as_and_primary_key` | C-032 |  |
| `src/body_parser/mod.rs::parse_tables_primary_key_with_newline` | C-033 |  |
| `src/body_parser/mod.rs::parse_tables_primary_key_with_tab` | C-034 |  |
| `src/body_parser/mod.rs::parse_relationships_lowercase_as_and_references` | C-035 |  |
| `src/body_parser/mod.rs::parse_qualified_entries_lowercase_as` | C-036 |  |
| `src/body_parser/mod.rs::parse_keyword_body_all_lowercase_full_round_trip` | C-037 | Relationship target table declared. |
| `src/body_parser/mod.rs::parse_keyword_body_with_facts_single` | C-038 |  |
| `src/body_parser/mod.rs::parse_keyword_body_with_facts_multiple` | C-039 |  |
| `src/body_parser/mod.rs::parse_keyword_body_with_facts_trailing_comma` | C-040 |  |
| `src/body_parser/mod.rs::parse_keyword_body_with_empty_facts` | C-041 |  |
| `src/body_parser/mod.rs::parse_keyword_body_without_facts_still_works` | C-017 |  |
| `src/body_parser/mod.rs::parse_keyword_body_fact_without_source_table_rejected` | C-042 |  |
| `src/body_parser/mod.rs::parse_keyword_body_facts_after_dimensions_rejected` | C-043 |  |
| `src/body_parser/mod.rs::parse_metrics_clause_qualified_entry` | C-044 |  |
| `src/body_parser/mod.rs::parse_metrics_clause_unqualified_entry` | C-045 |  |
| `src/body_parser/mod.rs::parse_metrics_clause_mixed_entries` | C-046 |  |
| `src/body_parser/mod.rs::parse_metrics_clause_trailing_comma` | C-047 |  |
| `src/body_parser/mod.rs::parse_metrics_clause_newline_separated` | C-048 |  |
| `src/body_parser/mod.rs::parse_keyword_body_with_derived_metrics` | C-049 |  |
| `src/body_parser/mod.rs::parse_keyword_body_only_derived_metrics` | C-050 | Derived expressions reference constants / each other so the view resolves. |
| `src/body_parser/mod.rs::parse_qualified_entries_still_rejects_unqualified` | C-016 |  |
| `src/body_parser/mod.rs::parse_metrics_clause_empty_body` | C-051 |  |
| `src/body_parser/mod.rs::parse_metrics_using_single_relationship` | C-052 |  |
| `src/body_parser/mod.rs::parse_metrics_using_multiple_relationships` | C-053 |  |
| `src/body_parser/mod.rs::parse_metrics_using_on_derived_produces_error` | C-054 |  |
| `src/body_parser/mod.rs::parse_metrics_without_using_backward_compat` | C-017 |  |
| `src/body_parser/mod.rs::parse_metrics_using_case_insensitive` | C-055 |  |
| `src/body_parser/mod.rs::parse_keyword_body_with_using_metrics` | C-052 | Same body as C-052. |
| `src/body_parser/mod.rs::test_dimension_with_comment` | C-056 |  |
| `src/body_parser/mod.rs::test_dimension_with_synonyms` | C-057 |  |
| `src/body_parser/mod.rs::test_comment_and_synonyms_either_order` | C-058 |  |
| `src/body_parser/mod.rs::test_private_metric` | C-059 |  |
| `src/body_parser/mod.rs::test_explicit_public_metric` | C-060 |  |
| `src/body_parser/mod.rs::test_default_public_metric` | C-017, C-060 |  |
| `src/body_parser/mod.rs::test_private_fact` | C-061 |  |
| `src/body_parser/mod.rs::test_private_dimension_rejected` | C-062 |  |
| `src/body_parser/mod.rs::test_escaped_quotes_in_comment` | C-063 |  |
| `src/body_parser/mod.rs::test_comment_identifier_not_confused` | C-064 |  |
| `src/body_parser/mod.rs::test_table_with_comment_and_synonyms` | C-065 |  |
| `src/body_parser/mod.rs::test_metric_with_using_and_comment` | C-066 |  |
| `src/body_parser/mod.rs::test_private_keyword_disambiguation` | C-067 |  |
| `src/body_parser/mod.rs::test_full_keyword_body_with_all_metadata` | C-068 |  |
| `src/body_parser/mod.rs::parse_metrics_non_additive_by_single_dim_defaults` | C-069 |  |
| `src/body_parser/mod.rs::parse_metrics_non_additive_by_desc_nulls_first` | C-070 |  |
| `src/body_parser/mod.rs::parse_metrics_non_additive_by_multiple_dims` | C-071 |  |
| `src/body_parser/mod.rs::parse_metrics_non_additive_by_on_derived_produces_error` | C-072 |  |
| `src/body_parser/mod.rs::parse_metrics_non_additive_by_with_using` | C-073 |  |
| `src/body_parser/mod.rs::parse_keyword_body_non_additive_by_integration` | C-070 |  |
| `src/body_parser/mod.rs::parse_keyword_body_non_additive_by_invalid_dim_error` | C-074 |  |
| `src/body_parser/mod.rs::parse_keyword_body_non_additive_by_valid_dim_success` | C-075 |  |
| `src/body_parser/mod.rs::parse_keyword_body_non_additive_by_case_insensitive_dim_match` | C-076 |  |
| `src/body_parser/mod.rs::parse_metrics_window_over_basic` | C-080 |  |
| `src/body_parser/mod.rs::parse_metrics_window_over_with_frame_clause` | C-081 |  |
| `src/body_parser/mod.rs::parse_metrics_window_lag_with_extra_args` | C-082 |  |
| `src/body_parser/mod.rs::parse_metrics_window_over_on_derived_produces_error` | C-083 |  |
| `src/body_parser/mod.rs::parse_metrics_window_over_with_non_additive_by_produces_error` | C-084 |  |
| `src/body_parser/mod.rs::parse_metrics_window_over_no_over_returns_none` | C-017 |  |
| `src/body_parser/mod.rs::parse_metrics_window_over_order_by_desc_nulls` | C-085 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_metric_integration` | C-086 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_excluding_invalid_dim_error` | C-087 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_quoted_inner_metric_accepted` | C-090 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_quoted_case_varied_inner_metric_accepted` | C-091 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_quoted_order_by_dim_accepted` | C-092 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_quoted_unknown_inner_metric_still_rejected` | C-093 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_quoted_excluding_dim_accepted` | C-094 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_quoted_partition_dim_accepted` | C-095 |  |
| `src/body_parser/mod.rs::parse_keyword_body_materialization_quoted_dimension_accepted` | C-117 |  |
| `src/body_parser/mod.rs::parse_keyword_body_materialization_quoted_metric_accepted` | C-118 |  |
| `src/body_parser/mod.rs::parse_keyword_body_quoted_non_additive_by_dim_accepted` | C-077 |  |
| `src/body_parser/mod.rs::parse_keyword_body_quoted_dotted_non_additive_by_dim_accepted` | C-078 |  |
| `src/body_parser/mod.rs::parse_keyword_body_quoted_unknown_non_additive_by_dim_still_rejected` | C-079 |  |
| `src/body_parser/mod.rs::parse_keyword_body_quoted_unknown_names_still_rejected_at_every_site` | C-096, C-097, C-119, C-120 | One row per site: EXCLUDING, PARTITION BY, materialization DIMENSIONS, materialization METRICS. |
| `src/body_parser/mod.rs::parse_keyword_body_window_inner_metric_invalid_error` | C-088 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_order_by_invalid_dim_error` | C-089 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_valid_refs_succeed` | C-086 |  |
| `src/body_parser/mod.rs::parse_metrics_window_partition_by_explicit` | C-098 |  |
| `src/body_parser/mod.rs::parse_metrics_window_partition_by_multiple_dims` | C-099 |  |
| `src/body_parser/mod.rs::parse_metrics_window_partition_by_no_order` | C-100 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_partition_by_integration` | C-098 |  |
| `src/body_parser/mod.rs::parse_keyword_body_window_partition_by_invalid_dim_error` | C-101 |  |
| `src/body_parser/mod.rs::parse_materializations_clause_single_entry` | C-102 |  |
| `src/body_parser/mod.rs::parse_materializations_clause_multiple_entries` | C-103 |  |
| `src/body_parser/mod.rs::parse_materializations_clause_dimensions_only` | C-104 |  |
| `src/body_parser/mod.rs::parse_materializations_clause_metrics_only` | C-105 |  |
| `src/body_parser/mod.rs::parse_materializations_clause_rejects_empty` | C-106 |  |
| `src/body_parser/mod.rs::parse_materializations_junk_before_first_subclause_rejected` | C-107 |  |
| `src/body_parser/mod.rs::parse_materializations_duplicate_table_rejected` | C-108 |  |
| `src/body_parser/mod.rs::parse_materializations_duplicate_dimensions_rejected` | C-109 |  |
| `src/body_parser/mod.rs::parse_materializations_junk_after_list_rejected` | C-110 |  |
| `src/body_parser/mod.rs::parse_keyword_body_with_materializations` | C-102 |  |
| `src/body_parser/mod.rs::parse_keyword_body_materializations_before_metrics_rejected` | C-111 |  |
| `src/body_parser/mod.rs::parse_keyword_body_materialization_bad_dim_ref` | C-112 |  |
| `src/body_parser/mod.rs::parse_keyword_body_materialization_bad_dim_ref_with_suggestion` | C-113 |  |
| `src/body_parser/mod.rs::parse_keyword_body_materialization_bad_metric_ref` | C-114 |  |
| `src/body_parser/mod.rs::parse_keyword_body_materialization_duplicate_name` | C-115 |  |
| `src/body_parser/mod.rs::parse_keyword_body_without_materializations_has_empty_vec` | C-017 |  |
| `src/body_parser/mod.rs::parse_materializations_qualified_table_name` | C-116 |  |
| `src/body_parser/mod.rs::f7_tables_bare_name_defaults_alias` | C-122 |  |
| `src/body_parser/mod.rs::f7_tables_bare_name_no_constraint` | C-123 |  |
| `src/body_parser/mod.rs::f7_tables_explicit_alias_still_parses` | C-017 |  |
| `src/body_parser/mod.rs::f7_tables_quoted_bare_name` | C-124 |  |
| `src/body_parser/mod.rs::f7_alias_less_qualified_name_uses_last_component` | C-125, C-126 |  |
| `src/body_parser/mod.rs::f9_dimension_multi_token_name_rejected` | C-128 |  |
| `src/body_parser/mod.rs::f9_metric_multi_token_name_rejected` | C-129 |  |
| `src/body_parser/mod.rs::f9_derived_metric_multi_token_name_rejected` | C-130 |  |
| `src/body_parser/mod.rs::f9_relationship_multi_token_name_rejected` | C-131 |  |
| `src/body_parser/mod.rs::f9_quoted_name_with_space_still_valid` | C-127 |  |
| `src/body_parser/mod.rs::f11_empty_quoted_table_alias_rejected` | C-132 | Today DuckDB's tokenizer rejects `""` (zero-length delimited identifier) before the extension's own check runs. |
| `src/body_parser/mod.rs::f11_empty_quoted_dimension_alias_rejected` | C-133 | As above: rejected by DuckDB's tokenizer today. |
| `src/body_parser/mod.rs::f12_public_on_dimension_accepted` | C-134 |  |
| `src/body_parser/mod.rs::f12_private_on_dimension_still_rejected` | C-062 |  |
| `src/body_parser/mod.rs::f12_with_synonyms_without_equals_accepted` | C-135 |  |
| `src/body_parser/mod.rs::f12_with_synonyms_with_equals_still_accepted` | C-057 |  |
| `src/body_parser/mod.rs::f14_unqualified_name_reports_qualifier_not_missing_as` | C-137 |  |
| `src/body_parser/mod.rs::f14_qualified_entry_with_dotted_expression_still_parses` | C-136 |  |
| `src/body_parser/mod.rs::f6_trailing_view_comment_extracted` | C-138 |  |
| `src/body_parser/mod.rs::f6_no_trailing_comment_is_none` | C-017 |  |
| `src/body_parser/mod.rs::f6_entry_level_comment_not_mistaken_for_view_comment` | C-139 |  |
| `src/body_parser/mod.rs::f6_view_level_synonyms_rejected` | C-140 |  |
| `src/body_parser/annotations.rs::labels_filter_sets_the_flag_and_is_stripped_from_the_expression` | C-141 |  |
| `src/body_parser/annotations.rs::labels_is_case_insensitive_and_the_equals_is_optional` | C-142 |  |
| `src/body_parser/annotations.rs::absent_labels_leaves_the_flag_false` | C-141 |  |
| `src/body_parser/annotations.rs::labels_composes_with_comment_and_synonyms_in_any_order` | C-143 |  |
| `src/body_parser/annotations.rs::an_unsupported_label_is_rejected_rather_than_ignored` | C-144 |  |
| `src/body_parser/annotations.rs::an_empty_label_list_is_rejected` | C-145 |  |
| `src/body_parser/annotations.rs::a_duplicate_labels_clause_is_rejected` | C-146 |  |
| `src/body_parser/annotations.rs::labels_as_a_quoted_column_name_is_not_an_annotation` | C-147 |  |
| `src/body_parser/clause_bounds.rs::unexpected_multibyte_char_reported_verbatim` | C-149 | The Rust test pins the verbatim character in the message; syntax-error wording is not pinned in this corpus (rule 4). |
| `src/body_parser/clause_bounds.rs::expected_paren_multibyte_char_reported_verbatim` | C-150 | As above: wording not pinned. |
| `src/body_parser/clause_bounds.rs::char_at_decodes_codepoint_and_handles_eof` | N/A | Internal UTF-8 decode helper for error text; its only effect is error wording, covered by the syntax rows C-149/C-150 (wording not pinned). |
| `src/body_parser/clause_bounds.rs::unexpected_ascii_char_reported` | C-151 |  |
| `src/body_parser/clause_bounds.rs::keyword_list_errors_include_materializations` | C-151, C-152 | The keyword list in the message is wording, not pinned (rule 4); both error arms are exercised. |
| `src/body_parser/clause_bounds.rs::all_two_clause_order_inversions_rejected_with_caret` | C-148, C-153, C-154, C-155, C-156, C-157, C-158, C-159, C-160, C-161, C-162, C-163, C-164, C-165, C-166, C-167 | C-148 is the six-clause accepted base; C-153..C-167 are the 15 pairwise swaps. Carets not asserted. |
| `src/body_parser/clause_bounds.rs::out_of_order_caret_honours_base_offset` | C-165 | Caret position not asserted (corpus rule 4); the METRICS-before-DIMENSIONS rejection itself is C-165. |
| `src/body_parser/cursor.rs::peek_bump_and_kw` | C-017, C-032 | Case-insensitive keyword match (`as`, `primary key`). |
| `src/body_parser/cursor.rs::abs_carets_are_base_relative` | N/A | Checks the byte offset of an internal error caret; corpus rule 4 does not assert carets (error_caret_*.test cover rendering). |
| `src/body_parser/cursor.rs::find_kw_pair_only_matches_adjacent_words` | C-148, C-168 |  |
| `src/body_parser/cursor.rs::find_kw_seq_matches_consecutive_words` | C-171, C-172, C-173 |  |
| `src/body_parser/cursor.rs::find_all_kw_depth0_tiles_and_ignores_nested_and_quoted` | C-121 |  |
| `src/body_parser/cursor.rs::find_kw_seq_depth0_ignores_nested_run` | C-174 | The nested run becomes one USING item and is reported as an unknown relationship. |
| `src/body_parser/cursor.rs::find_kw_skips_quoted_and_strings` | C-175 |  |
| `src/body_parser/cursor.rs::take_parens_is_quote_and_nest_aware` | C-175 |  |
| `src/body_parser/cursor.rs::take_parens_unclosed_is_none` | C-170 |  |
| `src/body_parser/cursor.rs::unterminated_tail_reports_open_quote_kind` | C-178, C-179, C-180 |  |
| `src/body_parser/cursor.rs::take_parens_not_a_paren_is_none` | C-169 |  |
| `src/body_parser/cursor.rs::rest_returns_remaining_source` | N/A | Internal cursor accessor (remaining source slice); no behaviour of its own beyond the clause parsers that use it. |
| `src/body_parser/lexer.rs::bare_idents_and_symbols` | C-017 | Plain bare-identifier tokenization; exercised by every accept row. |
| `src/body_parser/lexer.rs::dotted_name_is_ident_dot_ident` | C-027 |  |
| `src/body_parser/lexer.rs::quoted_ident_keeps_quotes_and_inner_bytes` | C-124 |  |
| `src/body_parser/lexer.rs::quoted_ident_doubled_quote_escape` | C-176 | C-176 also carries a PRE-EXISTING note on DESCRIBE base-table naming. |
| `src/body_parser/lexer.rs::keyword_inside_quoted_ident_is_one_token` | C-175 |  |
| `src/body_parser/lexer.rs::string_literal_with_escape_and_keywords` | C-175 |  |
| `src/body_parser/lexer.rs::dollar_string_is_one_opaque_token` | C-177 |  |
| `src/body_parser/lexer.rs::tagged_dollar_string_closes_on_matching_tag_only` | C-177 |  |
| `src/body_parser/lexer.rs::unterminated_dollar_string_spans_to_eof` | C-180 |  |
| `src/body_parser/lexer.rs::lone_dollar_and_positional_param_are_symbols` | C-185 | PRE-EXISTING: `$1` is accepted in a stored expression but no query can bind it. |
| `src/body_parser/lexer.rs::unterminated_quote_and_string` | C-178, C-179, C-181 |  |
| `src/body_parser/lexer.rs::escape_string_with_backslash_escaped_quote_is_terminated` | C-177 |  |
| `src/body_parser/lexer.rs::unterminated_escape_string_ending_in_a_backslash_stays_in_bounds` | C-182 |  |
| `src/body_parser/lexer.rs::escape_string_with_escaped_final_quote_is_unterminated_and_in_bounds` | C-183 |  |
| `src/body_parser/lexer.rs::escape_string_with_escaped_backslash_is_terminated` | C-177 |  |
| `src/body_parser/lexer.rs::typed_literal_is_not_an_escape_string` | C-177 |  |
| `src/body_parser/lexer.rs::non_ascii_is_one_bare_ident_no_midcodepoint_span` | C-184 | PRE-EXISTING (TECH-DEBT #96): DESCRIBE keeps the quotes in BASE_TABLE_NAME |
| `src/body_parser/lexer.rs::paren_group_tokens` | C-009 |  |
| `src/body_parser/lexer.rs::well_formed_over_hostile_inputs` | C-175, C-176, C-177, C-178, C-184, C-185 | Each curated hostile input maps to the row that feeds the same shape through CREATE. |
| `src/body_parser/lexer.rs::tiling_holds_over_hostile_alphabet` | C-176, C-178, C-181 | Representative rows per generator axis: doubled/unterminated quotes (C-176, C-178, C-181). |
| `src/body_parser/lexer.rs::tiling_holds_over_arbitrary_unicode` | C-184 | Representative row: non-ASCII identifiers (C-184). |
| `src/body_parser/lexer.rs::every_token_span_is_sliceable` | C-177, C-182, C-183 | Representative rows over the e/'/backslash/$ alphabet: C-177 (terminated), C-182 and C-183 (unterminated, must not panic). |
| `src/body_parser/window.rs::trailing_arithmetic_after_the_over_clause_is_rejected` | C-187 |  |
| `src/body_parser/window.rs::an_expression_prefix_before_the_window_function_is_rejected` | C-188 |  |
| `src/body_parser/window.rs::a_second_over_term_is_rejected` | C-189 |  |
| `src/body_parser/window.rs::a_plain_window_metric_still_parses` | C-080 |  |
| `src/body_parser/window.rs::qualified_quoted_and_spaced_function_names_still_parse` | C-186 |  |
| `src/body_parser/window.rs::an_expression_without_over_is_untouched` | C-190 |  |
| `src/body_parser/window.rs::a_window_function_without_arguments_is_still_rejected` | C-191 |  |
