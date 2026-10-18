# Case map B: `src/body_parser/mod.rs` tests (part 1 of the `mod tests` block)

Source: the first 150 `#[test]` functions of `mod tests` in `src/body_parser/mod.rs`
(attributes at lines 309-2560). Corpus files: `test/sql/ddl_corpus_body_a1.test` and
`test/sql/ddl_corpus_body_a2.test` (row ids `B-nnn`, numbered across both files; a1 ends
at B-103). Rows marked `[syntax]` are `statement error` with an empty `----` block
(no message pinned); rows marked `[text]` pin text the grammar might plausibly change;
`PRE-EXISTING` comments mark today's behaviour that looks wrong.

Totals: 102 accept rows, 27 rejects that pin their message (`[text]`), 55 syntax rejects
(empty `----` block).

| source::test_fn | corpus row id(s) | note |
|---|---|---|
| body_parser/mod.rs::split_simple_three_entries | B-001 |  |
| body_parser/mod.rs::split_nested_parens_not_split | B-002 |  |
| body_parser/mod.rs::split_string_literal_comma_not_split | B-003 |  |
| body_parser/mod.rs::split_dollar_quoted_comma_not_split | B-004 |  |
| body_parser/mod.rs::split_tagged_dollar_quoted_comma_not_split | B-005 |  |
| body_parser/mod.rs::split_dollar_paren_does_not_change_depth | B-006 |  |
| body_parser/mod.rs::split_lone_dollar_is_live_not_a_tag | B-007 | PRE-EXISTING (TECH-DEBT #95): a `$1` dimension expression is stored |
| body_parser/mod.rs::dollar_quoted_dimension_expr_is_one_entry | B-015 |  |
| body_parser/mod.rs::dollar_quoted_metric_keyword_is_inert | B-016 |  |
| body_parser/mod.rs::dollar_quoted_dimension_close_paren_is_inert | B-017 |  |
| body_parser/mod.rs::unterminated_dollar_in_clause_reports_precise_error | B-018 |  |
| body_parser/mod.rs::non_additive_by_after_as_is_rejected | B-019, B-020 |  |
| body_parser/mod.rs::using_after_as_is_rejected | B-021, B-022 |  |
| body_parser/mod.rs::a_parenthesised_expression_does_not_hide_a_misplaced_clause | B-023, B-024, B-025 |  |
| body_parser/mod.rs::a_genuinely_nested_using_is_not_a_misplaced_clause | B-026 |  |
| body_parser/mod.rs::a_missing_metric_name_is_reported_before_a_misplaced_clause | B-027 |  |
| body_parser/mod.rs::access_modifier_plus_empty_using_is_not_a_metric_name | B-029 |  |
| body_parser/mod.rs::empty_using_alone_is_not_a_metric_name | B-030 |  |
| body_parser/mod.rs::empty_non_additive_by_alone_is_not_a_metric_name | B-031 |  |
| body_parser/mod.rs::empty_using_and_non_additive_by_together_are_not_a_metric_name | B-032 |  |
| body_parser/mod.rs::a_named_metric_with_empty_clauses_still_parses | B-028 |  |
| body_parser/mod.rs::over_after_as_is_still_accepted | B-033 |  |
| body_parser/mod.rs::clause_keywords_inside_a_string_literal_are_not_clauses | B-034 |  |
| body_parser/mod.rs::labels_is_rejected_on_a_metric | B-037 |  |
| body_parser/mod.rs::labels_is_rejected_on_a_table | B-038 |  |
| body_parser/mod.rs::labels_is_rejected_at_the_view_level | B-036, B-039 |  |
| body_parser/mod.rs::labels_is_still_accepted_on_a_fact_and_a_dimension | B-035 |  |
| body_parser/mod.rs::split_trailing_comma_discarded | B-008 |  |
| body_parser/mod.rs::split_empty_body | B-049 |  |
| body_parser/mod.rs::split_leading_and_interior_empty_entries_rejected | B-009, B-010, B-011, B-012, B-051 |  |
| body_parser/mod.rs::split_offset_points_at_trimmed_entry | N/A | asserts only the byte offset returned for each split entry (used for caret placement); the corpus does not pin carets (error_caret_*.test does), and the split itself is covered by the splitting rows |
| body_parser/mod.rs::t13_stray_comma_in_clause_rejected_end_to_end | B-013 |  |
| body_parser/mod.rs::find_clause_bounds_basic_tables_dimensions_metrics | B-040 |  |
| body_parser/mod.rs::f16_leading_as_requires_word_boundary | B-041, B-042 | the `AS(` half (a `(` right after AS is a boundary, so the error is not 'Expected AS') has no SQL-distinguishable outcome: `AS(` is rejected either way; the front door rejects `ASTABLES` before the body parser sees it |
| body_parser/mod.rs::find_clause_bounds_with_relationships | B-043 |  |
| body_parser/mod.rs::find_clause_bounds_rejects_unknown_keyword | B-044 |  |
| body_parser/mod.rs::find_clause_bounds_rejects_missing_tables | B-045 |  |
| body_parser/mod.rs::find_clause_bounds_rejects_duplicate_dimensions | B-046 |  |
| body_parser/mod.rs::find_clause_bounds_rejects_duplicate_tables | B-047 |  |
| body_parser/mod.rs::empty_dimensions_and_metrics_are_rejected | B-049, B-050 |  |
| body_parser/mod.rs::an_empty_tables_clause_is_rejected | B-054, B-055, B-056 |  |
| body_parser/mod.rs::an_empty_metrics_clause_alone_is_rejected | B-052 |  |
| body_parser/mod.rs::neither_dimensions_nor_metrics_present_is_still_rejected | B-053 |  |
| body_parser/mod.rs::empty_materializations_clause_parses | B-057 |  |
| body_parser/mod.rs::block_comment_between_clauses_is_inert | B-058 |  |
| body_parser/mod.rs::block_comment_inside_clause_parens_is_inert | B-059 |  |
| body_parser/mod.rs::caret_after_in_body_comment_is_honest | B-060 |  |
| body_parser/mod.rs::find_clause_bounds_nested_pk_not_confused | B-048 |  |
| body_parser/mod.rs::parse_tables_single_pk | B-061 |  |
| body_parser/mod.rs::parse_tables_schema_qualified | B-062 |  |
| body_parser/mod.rs::parse_tables_composite_pk | B-063 |  |
| body_parser/mod.rs::parse_tables_two_words_before_constraint_rejected | B-064 |  |
| body_parser/mod.rs::parse_tables_without_primary_key_is_valid | B-065 |  |
| body_parser/mod.rs::test_parse_single_table_entry_quoted_with_internal_whitespace | B-066 |  |
| body_parser/mod.rs::test_parse_single_table_entry_quoted_containing_primary_key_substring | B-067 |  |
| body_parser/mod.rs::test_parse_single_table_entry_3part_quoted_fqn_with_whitespace | B-068 |  |
| body_parser/mod.rs::test_parse_single_table_entry_regression_no_whitespace | B-069 |  |
| body_parser/mod.rs::test_parse_single_table_entry_quoted_with_unique_no_pk | B-070 |  |
| body_parser/mod.rs::test_parse_single_table_entry_comment_before_pk_rejected | B-074, B-075 |  |
| body_parser/mod.rs::test_parse_single_table_entry_junk_before_pk_rejected | B-076 |  |
| body_parser/mod.rs::test_parse_single_table_entry_junk_between_pk_and_unique_rejected | B-077 |  |
| body_parser/mod.rs::test_parse_single_table_entry_junk_before_pk_caret_position | N/A | asserts only the caret position of the `banana` error; the reject itself is the junk-before-PRIMARY-KEY row |
| body_parser/mod.rs::test_parse_single_table_entry_annotation_before_unique_rejected | B-078 |  |
| body_parser/mod.rs::test_parse_single_table_entry_full_form_still_parses | B-073 |  |
| body_parser/mod.rs::test_parse_single_table_entry_unique_inside_comment_string_ok | B-079 |  |
| body_parser/mod.rs::test_parse_single_table_entry_reserved_keyword_after_as_primary | B-084 |  |
| body_parser/mod.rs::test_parse_single_table_entry_reserved_keyword_after_as_unique | B-085 |  |
| body_parser/mod.rs::test_parse_single_table_entry_reserved_keyword_after_as_foreign | B-086 |  |
| body_parser/mod.rs::test_parse_single_table_entry_reserved_keyword_after_as_references | B-087 |  |
| body_parser/mod.rs::test_parse_single_table_entry_reserved_keyword_after_as_not | B-088 |  |
| body_parser/mod.rs::test_parse_single_table_entry_reserved_keyword_after_as_lowercase | B-089 |  |
| body_parser/mod.rs::test_parse_single_table_entry_unterminated_quote | B-096 | from SQL the open quote runs to the end of the statement, so DuckDB's own tokenizer rejects it before the TABLES scanner |
| body_parser/mod.rs::test_parse_single_table_entry_quoted_with_doubled_escape_balanced | B-097 |  |
| body_parser/mod.rs::test_parse_single_table_entry_unbalanced_after_doubled_escape | B-098 | as above: rejected by DuckDB's tokenizer first |
| body_parser/mod.rs::test_underscore_joined_primary_in_name_is_not_a_constraint | B-095 |  |
| body_parser/mod.rs::test_keyword_scan_over_non_ascii_no_panic | B-102, B-103 |  |
| body_parser/mod.rs::test_leading_symbol_in_name_slot_is_missing_name | B-090, B-091, B-092 |  |
| body_parser/mod.rs::test_bare_paren_immediately_after_name_terminates_name | B-093, B-094 | the `;` half cannot reach the TABLES scanner from SQL: DuckDB's statement split / the clause-paren matcher reject it first ("Unclosed '('"); row pins that it is rejected |
| body_parser/mod.rs::test_non_ascii_name_then_primary_key_no_panic | B-100, B-101 |  |
| body_parser/mod.rs::test_comment_annotation_non_ascii_payload_survives | B-104 |  |
| body_parser/mod.rs::test_synonyms_annotation_non_ascii_payload_survives | B-105 |  |
| body_parser/mod.rs::test_annotation_scan_non_ascii_expression_no_panic | B-106 |  |
| body_parser/mod.rs::test_annotation_both_orders_still_parse | B-107 |  |
| body_parser/mod.rs::test_annotation_duplicate_comment_rejected | B-108 |  |
| body_parser/mod.rs::test_annotation_duplicate_synonyms_rejected | B-109 |  |
| body_parser/mod.rs::test_annotation_trailing_garbage_rejected | B-110 |  |
| body_parser/mod.rs::test_annotation_keyword_word_boundary_preserved | B-113 |  |
| body_parser/mod.rs::test_annotation_with_without_synonyms_rejected | B-111 |  |
| body_parser/mod.rs::f15_annotation_errors_carry_carets | B-108, B-110, B-112 | the test asserts caret positions only; the three errors it carets on are pinned as rejects (trailing junk, duplicate COMMENT, malformed synonym) |
| body_parser/mod.rs::test_primary_key_inside_comment_string_not_fabricated | B-080 |  |
| body_parser/mod.rs::test_unique_inside_comment_string_not_fabricated | B-081 |  |
| body_parser/mod.rs::test_no_pk_table_comment_and_synonyms_preserved | B-082 |  |
| body_parser/mod.rs::test_table_entry_trailing_garbage_errors | B-083 |  |
| body_parser/mod.rs::test_split_commas_ignores_comma_inside_quoted_ident | B-014 |  |
| body_parser/mod.rs::test_quoted_ident_with_paren_does_not_close_clause | B-099 |  |
| body_parser/mod.rs::test_dot_inside_quoted_name_not_a_qualifier | B-114 |  |
| body_parser/mod.rs::test_private_fact_structural_error_caret_after_modifier | B-116, B-117 |  |
| body_parser/mod.rs::test_quoted_dot_in_alias_not_a_qualifier | B-115 |  |
| body_parser/mod.rs::test_keyword_not_matched_before_non_ascii_continuation | B-118 |  |
| body_parser/mod.rs::test_unquoted_qualified_comment_column_is_not_an_annotation | B-119 |  |
| body_parser/mod.rs::test_unquoted_qualified_labels_column_is_not_an_annotation | B-120 |  |
| body_parser/mod.rs::test_spaced_qualified_comment_column_is_not_an_annotation | B-121 |  |
| body_parser/mod.rs::test_bare_comment_keyword_still_parses_as_annotation | B-122 |  |
| body_parser/mod.rs::test_quoted_comment_column_usable_in_expression | B-123 |  |
| body_parser/mod.rs::test_escape_string_with_escaped_quote_parses_in_a_metric_entry | B-124 |  |
| body_parser/mod.rs::test_escape_string_does_not_swallow_the_entry_separator | B-125 |  |
| body_parser/mod.rs::test_typed_literal_entry_still_splits_normally | B-126 |  |
| body_parser/mod.rs::test_unterminated_quote_in_dimension_entry_errors | B-127 | from SQL the open quote runs to the end of the statement, so DuckDB's own tokenizer rejects it first |
| body_parser/mod.rs::test_parse_single_table_entry_mixed_quoted_and_bare | B-071, B-072 |  |
| body_parser/mod.rs::test_parse_non_additive_dims_quoted_identifier_with_whitespace | B-129 |  |
| body_parser/mod.rs::parse_metrics_using_nested_as_splits_at_structural_as | B-140 | no relationship can be named `a AS b`, so SQL cannot accept this; the semantic error message pins the parsed name and USING list |
| body_parser/mod.rs::test_parse_non_additive_dims_dotted_path | B-130 |  |
| body_parser/mod.rs::test_keyword_boundaries_reject_underscore_and_non_ascii_continuation | B-137, B-147, B-148, B-149, B-150 |  |
| body_parser/mod.rs::metric_non_additive_missing_paren_caret_accounts_for_access_modifier | B-134, B-135 |  |
| body_parser/mod.rs::test_metric_junk_after_non_additive_by_rejected | B-136 |  |
| body_parser/mod.rs::test_metric_junk_after_using_rejected | B-139 |  |
| body_parser/mod.rs::test_metric_using_then_non_additive_by_still_parses | B-138 |  |
| body_parser/mod.rs::test_as_keyword_requires_boundary_in_tables_and_materializations | B-162, B-163, B-164, B-165, B-166 |  |
| body_parser/mod.rs::test_over_clause_error_position_is_expression_relative | B-151 |  |
| body_parser/mod.rs::test_over_partition_and_order_by_with_interior_whitespace | B-143, B-144 |  |
| body_parser/mod.rs::test_over_partition_dim_named_like_frame_keyword | B-145, B-146 |  |
| body_parser/mod.rs::test_over_order_without_by_rejected | B-152 |  |
| body_parser/mod.rs::test_over_order_junk_before_by_rejected | B-153 |  |
| body_parser/mod.rs::test_over_junk_content_rejected_as_frame | B-154 |  |
| body_parser/mod.rs::test_over_order_by_frame_keyword_name_rejected | B-155 |  |
| body_parser/mod.rs::test_over_order_by_before_partition_by_rejected | B-156 |  |
| body_parser/mod.rs::test_over_junk_before_partition_by_rejected | B-157 |  |
| body_parser/mod.rs::test_over_well_formed_partition_order_still_parses | B-141 |  |
| body_parser/mod.rs::test_over_frame_only_still_parses | B-142 |  |
| body_parser/mod.rs::test_non_additive_by_flexible_spacing | B-131, B-132 |  |
| body_parser/mod.rs::test_parse_non_additive_dims_unterminated_quote | B-133 | from SQL the open quote runs to the end of the statement, so DuckDB's own tokenizer rejects it first |
| body_parser/mod.rs::test_parse_window_spec_quoted_order_by | B-159 |  |
| body_parser/mod.rs::test_parse_window_spec_dotted_order_by | B-160 |  |
| body_parser/mod.rs::test_parse_window_spec_unterminated_quote_order_by | B-161 | from SQL the open quote runs to the end of the statement, so DuckDB's own tokenizer rejects it first |
| body_parser/mod.rs::test_parse_window_spec_regression_bare_order_by | B-158 |  |
| body_parser/mod.rs::test_parse_non_additive_dims_regression_bare_no_whitespace | B-128 |  |
| body_parser/mod.rs::parse_relationships_empty_body | B-173 |  |
| body_parser/mod.rs::parse_relationships_single_entry | B-174 |  |
| body_parser/mod.rs::parse_relationships_composite_fk | B-175 |  |
| body_parser/mod.rs::parse_relationships_quoted_paren_in_fk_column | B-176 |  |
| body_parser/mod.rs::parse_materializations_quoted_specials_do_not_split | B-167 |  |
| body_parser/mod.rs::parse_materializations_unquoted_keyword_name_nested_not_a_subkeyword | B-168 |  |
| body_parser/mod.rs::parse_materializations_quoted_name_with_space | B-169 |  |
| body_parser/mod.rs::parse_materializations_leading_symbol_is_missing_name | B-170 |  |
| body_parser/mod.rs::parse_materializations_unterminated_name_rejected_cleanly | B-171, B-172 | from SQL the open quote runs to the end of the statement, so DuckDB's own tokenizer rejects both forms first |
| body_parser/mod.rs::parse_relationships_error_missing_name | B-177 |  |
| body_parser/mod.rs::parse_relationship_without_cardinality_defaults | B-178 | cardinality is not shown by GET_DDL or DESCRIBE; the stored YAML join pins that no explicit cardinality was recorded |
| body_parser/mod.rs::old_cardinality_keywords_rejected | B-179, B-180, B-181 |  |
| body_parser/mod.rs::trailing_text_after_references_rejected | B-182 |  |
| body_parser/mod.rs::references_with_column_list | B-183 | uses a UNIQUE column rather than the PK: an explicit list naming the PK is stored identically to no list (see references_without_column_list) |
| body_parser/mod.rs::references_without_column_list | B-184 | the parser's empty ref_columns is resolved to the PK before storage, so SQL sees REF_KEY ["id"]; GET_DDL shows the list-less form |
