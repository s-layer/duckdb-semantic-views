//! SQL keyword body parser for CREATE SEMANTIC VIEW.
//!
//! Parses: `AS TABLES (...) RELATIONSHIPS (...) DIMENSIONS (...) METRICS (...)`
//! into a `SemanticViewDefinition`.

mod annotations;
mod clause_bounds;
pub(crate) mod cross_refs;
mod cursor;
mod entries;
mod lexer;
mod materializations;
mod metrics;
mod relationships;
mod scan;
mod tables;
mod window;

use crate::errors::ParseError;
use crate::model::{
    AccessModifier, Dimension, Fact, Join, Materialization, Metric, NonAdditiveDim, TableRef,
    WindowSpec,
};

use clause_bounds::find_clause_bounds;

pub(crate) use annotations::find_annotation_start;
pub(crate) use entries::parse_qualified_entries;
pub(crate) use materializations::parse_materializations_clause;
pub(crate) use metrics::{misplaced_metric_clause, parse_metrics_clause};
pub(crate) use relationships::parse_relationships_clause;
pub(crate) use scan::{
    column_roundtrips_verbatim, identifier_slot_error, identifier_slot_roundtrips_verbatim,
    source_table_roundtrips_verbatim, split_at_depth0_commas,
};
pub(crate) use tables::parse_tables_clause;

/// Re-parse a rendered window expression, returning just the `WindowSpec`.
///
/// MODEL-1 (code-review 2026-08-08): the YAML choke point needs to check that a
/// `WindowSpec` it is about to store SURVIVES the render → parse round trip,
/// because `emit_window_expr` writes every one of its fields out raw. Doing
/// that with the real parser (rather than a second copy of its rules here) is
/// what makes the check complete: whatever the parser would read back is what
/// gets compared. Positions are irrelevant to that caller, so the offset is 0.
pub(crate) fn reparse_window_expr(expr: &str) -> Result<Option<WindowSpec>, ParseError> {
    window::parse_window_over_clause(expr, 0).map(|(_, ws)| ws)
}

/// Parsed DIMENSIONS / FACTS entry (R-4: named fields, was a 6-tuple).
///
/// Shared shape for both clauses; DIMENSIONS ignores `access` (a leading
/// PRIVATE/PUBLIC is rejected earlier for dimensions). Fields map onto
/// [`Fact`] / [`Dimension`] in `parse_keyword_body`.
#[derive(Debug)]
pub(super) struct ParsedQualifiedEntry {
    pub(super) source_alias: String,
    pub(super) name: String,
    pub(super) expr: String,
    pub(super) comment: Option<String>,
    pub(super) synonyms: Vec<String>,
    /// `LABELS = (FILTER)` — the member is a named filter.
    pub(super) is_filter: bool,
    pub(super) access: AccessModifier,
}

/// Parsed METRICS entry (R-4: named fields, was a 9-tuple with `// tuple
/// index N` comments and a 9-way closure destructuring).
///
/// Fields map 1:1 onto [`Metric`]; `output_type` is assigned during
/// expansion, not at parse time, so it has no field here.
#[derive(Debug)]
pub(super) struct ParsedMetric {
    pub(super) source_alias: Option<String>,
    pub(super) name: String,
    pub(super) expr: String,
    pub(super) using_relationships: Vec<String>,
    pub(super) comment: Option<String>,
    pub(super) synonyms: Vec<String>,
    pub(super) access: AccessModifier,
    pub(super) non_additive_by: Vec<NonAdditiveDim>,
    pub(super) window_spec: Option<WindowSpec>,
}

/// Result of parsing the keyword body (everything after "AS").
#[derive(Debug)]
pub struct KeywordBody {
    pub tables: Vec<TableRef>,
    pub relationships: Vec<Join>,
    pub facts: Vec<Fact>,
    pub dimensions: Vec<Dimension>,
    pub metrics: Vec<Metric>,
    pub materializations: Vec<Materialization>,
    /// A trailing view-level `COMMENT = '...'` after the last clause
    /// (Snowflake's comment position, F-6). `None` when absent; the caller
    /// merges it with any comment written between the name and `AS`.
    pub view_comment: Option<String>,
}

/// Parse the keyword body after "AS" into structured clause data.
///
/// `text` is the full text starting with "AS", trimmed.
/// `base_offset` is the byte offset of `text[0]` in the original query string.
#[allow(clippy::too_many_lines)]
pub fn parse_keyword_body(text: &str, base_offset: usize) -> Result<KeywordBody, ParseError> {
    // Strip leading "AS" (case-insensitive). F-16 (code-review 2026-07-16):
    // require a word boundary after "AS" so `ASTABLES(...)` is not silently
    // read as `AS TABLES(...)`. The front door (`create_body.rs`) already
    // whitespace-delimits this, but `parse_keyword_body` is `pub` and
    // fuzz-facing, so it must self-guard against a following identifier byte.
    let trimmed = text.trim();
    let after_as = if trimmed
        .get(..2)
        .is_some_and(|s| s.eq_ignore_ascii_case("AS"))
        && trimmed
            .as_bytes()
            .get(2)
            .is_none_or(|&b| !crate::util::is_ident_byte(b))
    {
        trimmed[2..].trim_start()
    } else {
        return Err(ParseError {
            message: "Expected 'AS' keyword at start of semantic view body.".to_string(),
            position: Some(base_offset),
        });
    };

    // Offset of after_as within the original query
    let as_offset = base_offset + (text.len() - text.trim_start().len()) + 2;
    let after_as_offset = as_offset + (text.trim_start()[2..].len() - after_as.len());

    // F-6 (code-review 2026-07-16): peel an optional trailing view-level
    // `COMMENT = '...'` (Snowflake places it after the last clause) before the
    // clause scan, so the COMMENT keyword is not read as an unknown clause.
    let (after_as, view_comment) = split_trailing_view_comment(after_as, after_as_offset)?;

    let bounds = find_clause_bounds(after_as, after_as_offset)?;

    let mut tables: Vec<TableRef> = Vec::new();
    let mut relationships: Vec<Join> = Vec::new();
    let mut facts_raw: Vec<ParsedQualifiedEntry> = Vec::new();
    let mut dimensions_raw: Vec<ParsedQualifiedEntry> = Vec::new();
    let mut metrics_raw: Vec<ParsedMetric> = Vec::new();
    let mut materializations: Vec<Materialization> = Vec::new();

    for bound in &bounds {
        match bound.keyword {
            "tables" => {
                tables = parse_tables_clause(bound.content, bound.content_offset)?;
            }
            "relationships" => {
                relationships = parse_relationships_clause(bound.content, bound.content_offset)?;
            }
            "facts" => {
                facts_raw =
                    parse_qualified_entries(bound.content, bound.content_offset, true, "facts")?;
            }
            "dimensions" => {
                dimensions_raw = parse_qualified_entries(
                    bound.content,
                    bound.content_offset,
                    false,
                    "dimensions",
                )?;
            }
            "metrics" => {
                metrics_raw = parse_metrics_clause(bound.content, bound.content_offset)?;
            }
            "materializations" => {
                materializations =
                    parse_materializations_clause(bound.content, bound.content_offset)?;
            }
            _ => {}
        }
    }

    // TECH-DEBT #94 (Snowflake parity): the clause scanner requires the
    // clauses to be present; a view also needs something in them. `TABLES ()`
    // and an empty DIMENSIONS / METRICS pair are refused here, at the keyword.
    if tables.is_empty() {
        return Err(ParseError {
            message: "A semantic view needs at least one table in TABLES.".to_string(),
            position: bounds
                .iter()
                .find(|b| b.keyword == "tables")
                .map(|b| b.keyword_offset),
        });
    }
    if dimensions_raw.is_empty() && metrics_raw.is_empty() {
        return Err(ParseError {
            message: "A semantic view needs at least one dimension or metric.".to_string(),
            position: bounds
                .iter()
                .find(|b| matches!(b.keyword, "dimensions" | "metrics"))
                .map(|b| b.keyword_offset),
        });
    }

    // Map parsed clause entries onto the Fact / Dimension / Metric model types.
    let facts = facts_raw
        .into_iter()
        .map(|e| Fact {
            name: e.name,
            expr: e.expr,
            source_table: Some(e.source_alias),
            output_type: None,
            comment: e.comment,
            synonyms: e.synonyms,
            is_filter: e.is_filter,
            access: e.access,
        })
        .collect();

    let dimensions: Vec<Dimension> = dimensions_raw
        .into_iter()
        // Dimensions carry no access modifier — `e.access` is intentionally
        // dropped (a leading PRIVATE/PUBLIC is rejected earlier for DIMENSIONS).
        .map(|e| Dimension {
            name: e.name,
            expr: e.expr,
            source_table: Some(e.source_alias),
            output_type: None,
            comment: e.comment,
            synonyms: e.synonyms,
            is_filter: e.is_filter,
        })
        .collect();

    let metrics: Vec<Metric> = metrics_raw
        .into_iter()
        .map(|m| Metric {
            name: m.name,
            expr: m.expr,
            source_table: m.source_alias,
            output_type: None,
            using_relationships: m.using_relationships,
            comment: m.comment,
            synonyms: m.synonyms,
            access: m.access,
            non_additive_by: m.non_additive_by,
            window_spec: m.window_spec,
        })
        .collect();

    // MODEL-1 (code-review 2026-08-08): the semantic cross-reference checks
    // that used to be inlined here (NON ADDITIVE BY, window EXCLUDING /
    // PARTITION BY / ORDER BY / inner metric, materialization duplicate names
    // and references) now live in `cross_refs`, so the YAML import path runs
    // exactly the same checks instead of skipping them. Messages are unchanged;
    // they were positionless here too.
    crate::body_parser::cross_refs::validate_cross_references(
        &dimensions,
        &metrics,
        &materializations,
    )
    .map_err(|message| ParseError {
        message,
        position: None,
    })?;

    Ok(KeywordBody {
        tables,
        relationships,
        facts,
        dimensions,
        metrics,
        materializations,
        view_comment,
    })
}

/// Peel an optional trailing view-level `COMMENT = '...'` off the clause region.
///
/// Snowflake places the view-level comment AFTER the last clause
/// (`... METRICS (...) COMMENT = '...'`), whereas this parser historically only
/// accepted it between the name and `AS`. Without peeling it here the trailing
/// `COMMENT` keyword is read as an unknown clause (F-6, code-review 2026-07-16).
///
/// The comment is the region from the first depth-0 `COMMENT` keyword to the end
/// of `after_as` — a `COMMENT` inside a clause's `(...)` sits at depth > 0 and is
/// inert. Returns the clause region with the comment sliced off, plus the parsed
/// comment. `WITH SYNONYMS` is not a view-level annotation and is rejected.
fn split_trailing_view_comment(
    after_as: &str,
    base_offset: usize,
) -> Result<(&str, Option<String>), ParseError> {
    let cur = cursor::Cursor::new(after_as, base_offset);
    let Some(comment_tok) = cur.find_kw_depth0("COMMENT") else {
        return Ok((after_as, None));
    };
    let trailing = &after_as[comment_tok.start..];
    let (leftover, ann) = annotations::parse_trailing_annotations(trailing, cur.abs_of(trailing))?;
    // `trailing` begins at the COMMENT keyword, so the "expression" prefix the
    // annotation parser peels off must be empty.
    if !leftover.trim().is_empty() {
        return Err(ParseError {
            message: format!(
                "Unexpected text '{}' before the trailing view-level COMMENT.",
                leftover.trim()
            ),
            position: Some(base_offset + comment_tok.start),
        });
    }
    if !ann.synonyms.is_empty() {
        return Err(ParseError {
            message: "WITH SYNONYMS is not valid at the view level; it applies to tables, \
                      dimensions, facts, and metrics."
                .to_string(),
            position: Some(base_offset + comment_tok.start),
        });
    }
    if ann.is_filter {
        return Err(ParseError {
            message: "LABELS is not valid at the view level; it applies to facts and dimensions."
                .to_string(),
            position: Some(base_offset + comment_tok.start),
        });
    }
    Ok((&after_as[..comment_tok.start], ann.comment))
}

#[cfg(test)]
mod tests {
    use super::annotations::parse_trailing_annotations;
    use super::*;
    use crate::model::{Cardinality, NullsOrder, SortOrder};

    // -----------------------------------------------------------------------
    // split_at_depth0_commas tests
    // -----------------------------------------------------------------------

    #[test]
    fn split_simple_three_entries() {
        let result = split_at_depth0_commas("a, b, c").unwrap();
        assert_eq!(result.len(), 3, "Expected 3 entries, got {:?}", result);
        assert_eq!(result[0].1, "a");
        assert_eq!(result[1].1, "b");
        assert_eq!(result[2].1, "c");
    }

    #[test]
    fn split_nested_parens_not_split() {
        // The comma inside SUM(a, b) is at depth 1 — must not split
        let result = split_at_depth0_commas("SUM(a, b), COUNT(*)").unwrap();
        assert_eq!(result.len(), 2, "Expected 2 entries, got {:?}", result);
        assert_eq!(result[0].1, "SUM(a, b)");
        assert_eq!(result[1].1, "COUNT(*)");
    }

    #[test]
    fn split_string_literal_comma_not_split() {
        // Comma inside single-quoted string must not split
        let result = split_at_depth0_commas("a, 'x, y', b").unwrap();
        assert_eq!(result.len(), 3, "Expected 3 entries, got {:?}", result);
        assert_eq!(result[0].1, "a");
        assert_eq!(result[1].1, "'x, y'");
        assert_eq!(result[2].1, "b");
    }

    #[test]
    fn split_dollar_quoted_comma_not_split() {
        // PARSE-1 (code-review 2026-07-18): a comma inside a $$...$$
        // dollar-quoted string is inert, exactly like one inside '...'. The
        // dollar-quoted body is one opaque region between the two identical tags.
        let result = split_at_depth0_commas("a, $$x, y$$, b").unwrap();
        assert_eq!(result.len(), 3, "Expected 3 entries, got {result:?}");
        assert_eq!(result[0].1, "a");
        assert_eq!(result[1].1, "$$x, y$$");
        assert_eq!(result[2].1, "b");
    }

    #[test]
    fn split_tagged_dollar_quoted_comma_not_split() {
        // Tagged form $t$...$t$; only the MATCHING tag closes, so a different
        // inner tag ($z$) and its comma stay inside the region.
        let result = split_at_depth0_commas("a, $t$x, $z$ y$t$, b").unwrap();
        assert_eq!(result.len(), 3, "Expected 3 entries, got {result:?}");
        assert_eq!(result[1].1, "$t$x, $z$ y$t$");
    }

    #[test]
    fn split_dollar_paren_does_not_change_depth() {
        // A `(` / `)` inside $$...$$ must not shift the depth counter, or a real
        // depth-0 comma after the literal would be mis-counted.
        let result = split_at_depth0_commas("$$a)b(c$$, d").unwrap();
        assert_eq!(result.len(), 2, "Expected 2 entries, got {result:?}");
        assert_eq!(result[0].1, "$$a)b(c$$");
        assert_eq!(result[1].1, "d");
    }

    #[test]
    fn split_lone_dollar_is_live_not_a_tag() {
        // `$1` is a positional parameter, not a dollar-quote opener, and a lone
        // `$` never opens a region — the comma still splits normally.
        let result = split_at_depth0_commas("$1, a").unwrap();
        assert_eq!(result.len(), 2, "Expected 2 entries, got {result:?}");
        assert_eq!(result[0].1, "$1");
        assert_eq!(result[1].1, "a");
    }

    #[test]
    fn dollar_quoted_dimension_expr_is_one_entry() {
        // PARSE-1 end-to-end: DIMENSIONS (o.a AS $$p,q.r AS s$$) must parse as
        // ONE dimension whose expr is the whole dollar-quoted literal — the
        // inner comma and `AS` are inert. Previously the inner comma split it
        // into two garbage dimensions (`o.a AS $$p` and `q.r AS s$$`).
        let body = "AS TABLES (o AS orders) DIMENSIONS (o.a AS $$p,q.r AS s$$)";
        let parsed = parse_keyword_body(body, 0).expect("should parse");
        assert_eq!(
            parsed.dimensions.len(),
            1,
            "dollar-quoted expr must be one dimension, got: {:?}",
            parsed.dimensions
        );
        assert_eq!(parsed.dimensions[0].name, "a");
        assert_eq!(parsed.dimensions[0].expr, "$$p,q.r AS s$$");
    }

    #[test]
    fn dollar_quoted_metric_keyword_is_inert() {
        // PARSE-1: a structural keyword (USING / NON ADDITIVE BY) inside a
        // metric expression's $$...$$ must be inert — the entry has no real
        // USING clause, so it parses as a plain derived metric.
        let body = "AS TABLES (o AS orders) METRICS (m AS $$a USING b$$)";
        let parsed = parse_keyword_body(body, 0).expect("should parse");
        assert_eq!(parsed.metrics.len(), 1, "got: {:?}", parsed.metrics);
        assert_eq!(parsed.metrics[0].name, "m");
        assert_eq!(parsed.metrics[0].expr, "$$a USING b$$");
        assert!(
            parsed.metrics[0].using_relationships.is_empty(),
            "USING inside a dollar string must not be parsed as a USING clause"
        );
    }

    #[test]
    fn dollar_quoted_dimension_close_paren_is_inert() {
        // PARSE-1: a `)` inside $$...$$ must not close the DIMENSIONS (...) early.
        let body = "AS TABLES (o AS orders) DIMENSIONS (o.a AS $$x)y$$)";
        let parsed = parse_keyword_body(body, 0).expect("should parse");
        assert_eq!(parsed.dimensions.len(), 1, "got: {:?}", parsed.dimensions);
        assert_eq!(parsed.dimensions[0].expr, "$$x)y$$");
    }

    #[test]
    fn unterminated_dollar_in_clause_reports_precise_error() {
        // PARSE-1 diagnostic (Copilot review of #144): an unterminated
        // dollar-quoted literal in a clause reports "Unterminated dollar-quoted
        // string", not a misleading "Unclosed '('" — the literal swallowed the
        // clause's closing paren, so the lexer, not the paren matcher, has the
        // precise cause. Matches the entry-level `unterminated_quote_error`.
        let body = "AS TABLES (o AS orders) DIMENSIONS (o.a AS $$unterminated)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("Unterminated dollar-quoted string"),
            "expected a precise dollar-quote error, got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // LABELS is valid ONLY on a fact or dimension (PR #174 review).
    //
    // `parse_trailing_annotations` is shared by four call sites. Adding
    // `is_filter` to `ParsedAnnotations` made the three that cannot store it
    // parse `LABELS = (FILTER)` and then silently drop it — a definition would
    // round-trip having lost what the user wrote, the exact failure that
    // rejecting an unrecognised label exists to prevent. Each case below was
    // confirmed to CREATE successfully (with LABELS absent from the resulting
    // GET_DDL) before the guards landed.
    //
    // One test per site: the sqllogictest equivalents live in one file and the
    // runner stops at the first failure, so a regression in the second or third
    // guard would be masked until the first is repaired. These report
    // independently under `cargo test`.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // TECH-DEBT #38: a pre-`AS` clause written AFTER `AS` is swallowed into the
    // metric expression, so its semantics are silently discarded and the view
    // only fails much later at query time.
    // -----------------------------------------------------------------------

    #[test]
    fn non_additive_by_after_as_is_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    DIMENSIONS (o.d AS o.d) \
                    METRICS (o.m AS sum(o.v) NON ADDITIVE BY (d))";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("NON ADDITIVE BY"),
            "the error must name the misplaced clause, got: {}",
            err.message
        );
    }

    #[test]
    fn using_after_as_is_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id), c AS customers PRIMARY KEY (id)) \
                    RELATIONSHIPS (r AS o(cid) REFERENCES c(id)) \
                    METRICS (o.m AS sum(o.v) USING (r))";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("USING"),
            "the error must name the misplaced clause, got: {}",
            err.message
        );
    }

    /// Copilot review catch on #181: wrapping the whole expression in parens
    /// keeps the keywords at depth 1, so the depth-0 scan missed them and the
    /// SAME bug survived through another door — `CREATE` succeeded silently and
    /// the view failed only at query time. Redundant outer parens are stripped
    /// before scanning.
    #[test]
    fn a_parenthesised_expression_does_not_hide_a_misplaced_clause() {
        for metrics in [
            "o.m AS (sum(o.v) USING (r))",
            "o.m AS (sum(o.v) NON ADDITIVE BY (d))",
            "o.m AS ((sum(o.v) USING (r)))",
        ] {
            let body = format!(
                "AS TABLES (o AS orders PRIMARY KEY (id), c AS customers PRIMARY KEY (id)) \
                 RELATIONSHIPS (r AS o(cid) REFERENCES c(id)) \
                 DIMENSIONS (o.d AS o.d) \
                 METRICS ({metrics})"
            );
            let err = parse_keyword_body(&body, 0).unwrap_err();
            assert!(
                err.message.contains("must come BEFORE 'AS'"),
                "parenthesising must not hide the misplaced clause in {metrics:?}, got: {}",
                err.message
            );
        }
    }

    /// Guard for the stripping above: a `USING` that is genuinely nested — here
    /// inside a subquery's JOIN, which is valid SQL in a metric expression —
    /// must NOT be rejected. The parens around it are structural, not a
    /// redundant wrapper, so stripping leaves it at depth >= 1.
    #[test]
    fn a_genuinely_nested_using_is_not_a_misplaced_clause() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    DIMENSIONS (o.d AS o.d) \
                    METRICS (o.m AS sum(CASE WHEN o.id IN \
                             (SELECT a.id FROM x a JOIN y b USING (id)) THEN o.v ELSE 0 END))";
        let res = parse_keyword_body(body, 0);
        assert!(
            res.is_ok(),
            "a nested USING inside a subquery is not a misplaced clause, got: {:?}",
            res.err()
        );
    }

    /// Copilot review catch on #181: the misplaced-clause scan must not
    /// pre-empt the more basic "you forgot the metric name" diagnostic.
    #[test]
    fn a_missing_metric_name_is_reported_before_a_misplaced_clause() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    DIMENSIONS (o.d AS o.d) \
                    METRICS (AS sum(o.v) NON ADDITIVE BY (d))";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("Missing metric name"),
            "the primary defect is the missing name, got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // A metric whose name is EATEN by a peeled clause (CI fuzz_render_roundtrip,
    // 2026-08-08).
    //
    // The `before_as.is_empty()` check runs BEFORE `USING (...)` and
    // `NON ADDITIVE BY (...)` are peeled off the prefix, so an entry consisting
    // of nothing but those clauses cleared it with a non-empty `before_as` and
    // reached the name slot empty. `identifier_slot_error` returns `None` for an
    // empty slot, so the metric was stored NAMELESS.
    //
    // That is not merely untidy: `render_ddl` emits an empty name as `""`, which
    // this parser rejects as an empty quoted identifier — so the nameless metric
    // rendered to DDL that no longer parses, breaking the render fixpoint
    // `render(parse(render(d))) == render(d)` that `fuzz_render_roundtrip`
    // asserts with no escape.
    //
    // One test per reachable shape: the four differ in WHICH clause eats the
    // name, and a single combined test would stop at the first and leave the
    // rest unproven.
    // -----------------------------------------------------------------------

    /// Shape 1: an access modifier plus `USING ()` — the exact entry CI's
    /// fuzzer reduced to.
    #[test]
    fn access_modifier_plus_empty_using_is_not_a_metric_name() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (PRIVATE USING () AS 1)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("Missing metric name"),
            "a metric with no name must be rejected, got: {}",
            err.message
        );
    }

    /// Shape 2: `USING ()` alone, with no access modifier ahead of it.
    #[test]
    fn empty_using_alone_is_not_a_metric_name() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (USING () AS 1)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("Missing metric name"),
            "a metric with no name must be rejected, got: {}",
            err.message
        );
    }

    /// Shape 3: `NON ADDITIVE BY ()` is peeled off before `USING`, so it eats
    /// the name through a different door.
    #[test]
    fn empty_non_additive_by_alone_is_not_a_metric_name() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (NON ADDITIVE BY () AS 1)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("Missing metric name"),
            "a metric with no name must be rejected, got: {}",
            err.message
        );
    }

    /// Shape 4: both clauses present, each empty — neither peel leaves a name.
    #[test]
    fn empty_using_and_non_additive_by_together_are_not_a_metric_name() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (USING () NON ADDITIVE BY () AS 1)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("Missing metric name"),
            "a metric with no name must be rejected, got: {}",
            err.message
        );
    }

    /// Control for the four above: a REAL name in front of the same empty
    /// clauses still parses, and keeps its name. Without this, the fix could
    /// pass by rejecting every `USING ()` entry outright.
    #[test]
    fn a_named_metric_with_empty_clauses_still_parses() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (PRIVATE o.total USING () NON ADDITIVE BY () AS sum(o.v))";
        let kb = parse_keyword_body(body, 0).expect("a named metric must still parse");
        assert_eq!(kb.metrics.len(), 1);
        assert_eq!(kb.metrics[0].name, "total");
    }

    /// Control: `OVER (...)` legitimately follows `AS` — `parse_window_over_clause`
    /// runs on the post-`AS` text. The new scan must not reject it.
    #[test]
    fn over_after_as_is_still_accepted() {
        // A window metric's inner must be a declared METRIC, not a raw column.
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    DIMENSIONS (o.d AS o.d) \
                    METRICS (o.total AS sum(o.v), \
                             o.m AS SUM(total) \
                             OVER (PARTITION BY EXCLUDING d ORDER BY d ASC NULLS LAST))";
        let res = parse_keyword_body(body, 0);
        assert!(
            res.is_ok(),
            "a window clause after AS must still parse, got: {:?}",
            res.err()
        );
    }

    /// Guard against over-rejection: the keywords appear inside a STRING
    /// LITERAL, where they are data rather than clause syntax. The scan is
    /// quote-aware, so this must still parse — rejecting it would break valid
    /// DDL, a worse outcome than the bug being fixed.
    #[test]
    fn clause_keywords_inside_a_string_literal_are_not_clauses() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (o.m AS sum(CASE WHEN o.note = 'USING (x) NON ADDITIVE BY (y)' \
                             THEN o.v ELSE 0 END))";
        let res = parse_keyword_body(body, 0);
        assert!(
            res.is_ok(),
            "clause keywords inside a string are data, not syntax, got: {:?}",
            res.err()
        );
    }

    #[test]
    fn labels_is_rejected_on_a_metric() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (o.revenue AS sum(o.amount) LABELS = (FILTER))";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("LABELS is not valid on a metric"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn labels_is_rejected_on_a_table() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id) LABELS = (FILTER)) \
                    METRICS (o.revenue AS sum(o.amount))";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("LABELS is not valid on a table"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn labels_is_rejected_at_the_view_level() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (o.revenue AS sum(o.amount)) COMMENT = 'v' LABELS = (FILTER)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message
                .contains("LABELS is not valid at the view level"),
            "got: {}",
            err.message
        );
    }

    /// The guards must not cost FACTS / DIMENSIONS their label — the one place
    /// it is meant to work. Guards this pair against an over-broad rejection.
    #[test]
    fn labels_is_still_accepted_on_a_fact_and_a_dimension() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    FACTS (o.is_large AS o.amount > 100 LABELS = (FILTER)) \
                    DIMENSIONS (o.is_domestic AS o.country = 'US' LABELS = (FILTER)) \
                    METRICS (o.revenue AS sum(o.amount))";
        let parsed = parse_keyword_body(body, 0).expect("should parse");
        assert!(parsed.facts[0].is_filter, "fact filter flag lost");
        assert!(parsed.dimensions[0].is_filter, "dimension filter flag lost");
    }

    #[test]
    fn split_trailing_comma_discarded() {
        let result = split_at_depth0_commas("a,").unwrap();
        assert_eq!(
            result.len(),
            1,
            "Trailing comma must not produce extra entry"
        );
        assert_eq!(result[0].1, "a");
    }

    #[test]
    fn split_empty_body() {
        let result = split_at_depth0_commas("").unwrap();
        assert_eq!(result.len(), 0, "Empty body must produce 0 entries");
    }

    #[test]
    fn split_leading_and_interior_empty_entries_rejected() {
        // T-13 (code-review 2026-07-16): a stray leading (`,a`) or interior
        // (`a,,b`) comma is rejected rather than silently dropped. A single
        // trailing comma stays tolerated (covered above).
        assert!(split_at_depth0_commas(",a").is_err());
        assert!(split_at_depth0_commas("a,,b").is_err());
        assert!(split_at_depth0_commas("a, , b").is_err());
        assert!(split_at_depth0_commas(",").is_err());
        // A comma inside parens / quotes is still not a top-level split, so an
        // empty-looking interior there does not trip the check.
        assert!(split_at_depth0_commas("SUM(a,,b)").is_ok());
        assert!(split_at_depth0_commas("'a,,b'").is_ok());
    }

    #[test]
    fn split_offset_points_at_trimmed_entry() {
        // P-4 (code-review 2026-07-11): the returned offset is the trimmed
        // entry's first byte, not the position right after the comma, so an
        // error caret computed from it lands on the entry rather than drifting
        // left into the inter-entry whitespace.
        let result = split_at_depth0_commas("a,   b_bad").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], (0, "a"));
        assert_eq!(result[1], (5, "b_bad"));
        // Leading whitespace on the first entry is skipped too.
        assert_eq!(split_at_depth0_commas("   x, y").unwrap()[0], (3, "x"));
    }

    #[test]
    fn t13_stray_comma_in_clause_rejected_end_to_end() {
        // The stray-comma rejection surfaces through the full body parser, not
        // just the splitter unit.
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    DIMENSIONS (o.a AS o.a,, o.b AS o.b) \
                    METRICS (o.rev AS SUM(o.amount))";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("stray or leading"),
            "got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // find_clause_bounds tests (via parse_keyword_body integration)
    // -----------------------------------------------------------------------

    #[test]
    fn find_clause_bounds_basic_tables_dimensions_metrics() {
        // Smoke test: parsing a well-formed AS body finds all 3 clauses
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_ok(), "Expected Ok, got {:?}", result.map(|_| ()));
        let kb = result.unwrap();
        assert_eq!(kb.tables.len(), 1);
        assert_eq!(kb.dimensions.len(), 1);
        assert_eq!(kb.metrics.len(), 1);
    }

    #[test]
    fn f16_leading_as_requires_word_boundary() {
        // F-16 (code-review 2026-07-16): the leading-"AS" strip must not fire
        // when "AS" is only a prefix of a longer identifier run. `ASTABLES(...)`
        // must NOT be read as `AS TABLES(...)`.
        let glued = "ASTABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.x AS x)";
        let err = parse_keyword_body(glued, 0).unwrap_err();
        assert!(
            err.message.contains("Expected 'AS'"),
            "glued ASTABLES should be rejected as a missing 'AS', got: {}",
            err.message
        );
        // A genuine word boundary (space, or end-of-input right after AS) still
        // strips as before.
        assert!(parse_keyword_body(
            "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.x AS x) METRICS (o.y AS SUM(y))",
            0,
        )
        .is_ok());
        // Lower-case and a non-identifier byte (`(`) immediately after AS are
        // both boundaries, so the strip fires (then fails later for other
        // reasons — the point is it is NOT rejected as "Expected 'AS'").
        let paren_after = parse_keyword_body("AS(", 0).unwrap_err();
        assert!(
            !paren_after.message.contains("Expected 'AS'"),
            "`(` after AS is a word boundary; got: {}",
            paren_after.message
        );
    }

    #[test]
    fn find_clause_bounds_with_relationships() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id), c AS customers PRIMARY KEY (id)) RELATIONSHIPS (o_to_c AS o(customer_id) REFERENCES c) DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result.map(|_| ()));
        let kb = result.unwrap();
        assert_eq!(kb.relationships.len(), 1);
    }

    #[test]
    fn find_clause_bounds_rejects_unknown_keyword() {
        // "TABLSE" is close to "TABLES" — should get "did you mean?" error
        let body = "AS TABLSE (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.x AS x)";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err(), "Expected error for unknown keyword TABLSE");
        let err = result.unwrap_err();
        assert!(
            err.message.contains("TABLES") || err.message.contains("TABLSE"),
            "Error should mention TABLES or TABLSE: {}",
            err.message
        );
    }

    #[test]
    fn find_clause_bounds_rejects_missing_tables() {
        let body = "AS DIMENSIONS (o.x AS x) METRICS (o.y AS SUM(y))";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err(), "Expected error for missing TABLES clause");
        let err = result.unwrap_err();
        assert!(
            err.message.contains("TABLES"),
            "Error should mention TABLES: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // T-2 (code-review 2026-07-11): the duplicate-clause-keyword branch in
    // clause_bounds.rs had zero test coverage. Pin the message and that the
    // caret points at the SECOND occurrence.
    // -----------------------------------------------------------------------

    #[test]
    fn find_clause_bounds_rejects_duplicate_dimensions() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.a AS o.a) DIMENSIONS (o.b AS o.b)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message
                .contains("Duplicate clause keyword 'DIMENSIONS'"),
            "got: {}",
            err.message
        );
        // Caret must point at the second DIMENSIONS keyword.
        assert_eq!(err.position, Some(body.rfind("DIMENSIONS").unwrap()));
    }

    #[test]
    fn find_clause_bounds_rejects_duplicate_tables() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) TABLES (c AS customers PRIMARY KEY (id)) DIMENSIONS (o.a AS o.a)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("Duplicate clause keyword 'TABLES'"),
            "got: {}",
            err.message
        );
        assert_eq!(err.position, Some(body.rfind("TABLES").unwrap()));
    }

    // -----------------------------------------------------------------------
    // T-3 (code-review 2026-07-11): pin the behaviour of empty clause bodies.
    // The clause scanner checks that TABLES and one of DIMENSIONS / METRICS
    // are present; `parse_keyword_body` then rejects them empty (TECH-DEBT
    // #94), at the keyword.
    // -----------------------------------------------------------------------

    /// TECH-DEBT #94 (Snowflake parity): a view needs at least one
    /// dimension or metric. Both clauses present but empty is rejected, at
    /// the first of them, like neither clause present.
    #[test]
    fn empty_dimensions_and_metrics_are_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS () METRICS ()";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message
                .contains("needs at least one dimension or metric"),
            "got: {}",
            err.message
        );
        assert_eq!(err.position, body.find("DIMENSIONS"));
    }

    #[test]
    fn an_empty_metrics_clause_alone_is_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) METRICS ()";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message
                .contains("needs at least one dimension or metric"),
            "got: {}",
            err.message
        );
        assert_eq!(err.position, body.find("METRICS"));
    }

    /// Facts alone are not enough: they are not queryable on their own.
    #[test]
    fn facts_with_empty_dimensions_are_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) FACTS (o.f AS o.amount) DIMENSIONS ()";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message
                .contains("needs at least one dimension or metric"),
            "got: {}",
            err.message
        );
    }

    /// TECH-DEBT #94: a view needs at least one table.
    #[test]
    fn an_empty_tables_clause_is_rejected() {
        let body = "AS TABLES () DIMENSIONS (o.a AS o.a)";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("needs at least one table"),
            "got: {}",
            err.message
        );
        assert_eq!(err.position, body.find("TABLES"));
    }

    #[test]
    fn neither_dimensions_nor_metrics_present_is_still_rejected() {
        let body_none = "AS TABLES (o AS orders PRIMARY KEY (id))";
        let err = parse_keyword_body(body_none, 0).unwrap_err();
        assert!(
            err.message
                .contains("At least one of 'DIMENSIONS' or 'METRICS' is required"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn empty_materializations_clause_parses() {
        let body =
            "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.a AS o.a) MATERIALIZATIONS ()";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert!(kb.materializations.is_empty());
    }

    // -----------------------------------------------------------------------
    // T-4 (code-review 2026-07-11): comments INSIDE the AS body work only by
    // construction (whole-query blanking runs before the body parser). Pin
    // that block comments between clauses and inside clause parens are inert,
    // and that a typo after an in-body comment still carets correctly.
    // -----------------------------------------------------------------------

    #[test]
    fn block_comment_between_clauses_is_inert() {
        // The parse layer sees comments already blanked to spaces (length
        // preserving), so this must parse identically to the comment-free
        // form. NOTE: parse_keyword_body itself does not blank — callers
        // (plan_create) do — so exercise the blank-then-parse pipeline.
        let raw = "AS TABLES (o AS orders PRIMARY KEY (id)) /* between */ DIMENSIONS (o.a AS o.a)";
        let blanked = crate::util::blank_sql_comments(raw);
        let kb = parse_keyword_body(&blanked, 0).unwrap();
        assert_eq!(kb.tables.len(), 1);
        assert_eq!(kb.dimensions.len(), 1);
    }

    #[test]
    fn block_comment_inside_clause_parens_is_inert() {
        let raw =
            "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.a AS o.a /* c, ) inside */)";
        let blanked = crate::util::blank_sql_comments(raw);
        let kb = parse_keyword_body(&blanked, 0).unwrap();
        assert_eq!(kb.dimensions.len(), 1);
        assert_eq!(kb.dimensions[0].name, "a");
    }

    #[test]
    fn caret_after_in_body_comment_is_honest() {
        // blank_sql_comments is length-preserving, so a typo AFTER an in-body
        // comment must still caret at its true byte offset. Here `TABLSE`
        // follows a block comment; its reported position must equal the raw
        // byte index of `TABLSE`, not be shifted by the comment's length.
        let raw =
            "AS /* leading note */ TABLSE (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.a AS o.a)";
        let blanked = crate::util::blank_sql_comments(raw);
        let err = parse_keyword_body(&blanked, 0).unwrap_err();
        assert!(err.message.contains("TABLSE") || err.message.contains("TABLES"));
        assert_eq!(
            err.position,
            Some(raw.find("TABLSE").unwrap()),
            "caret must point at the raw offset of TABLSE despite the preceding comment"
        );
    }

    #[test]
    fn find_clause_bounds_nested_pk_not_confused() {
        // PRIMARY KEY (...) creates nested depth — closing paren must not end TABLES clause early
        let body =
            "AS TABLES (o AS orders PRIMARY KEY (o_orderkey, o_custkey)) DIMENSIONS (o.x AS x)";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result.map(|_| ()));
        let kb = result.unwrap();
        assert_eq!(kb.tables[0].pk_columns.len(), 2);
    }

    // -----------------------------------------------------------------------
    // parse_tables_clause tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_tables_single_pk() {
        let result = parse_tables_clause("o AS orders PRIMARY KEY (o_orderkey)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "orders");
        assert_eq!(result[0].pk_columns, vec!["o_orderkey"]);
    }

    #[test]
    fn parse_tables_schema_qualified() {
        let result = parse_tables_clause("o AS main.orders PRIMARY KEY (o_orderkey)", 0).unwrap();
        assert_eq!(result[0].table, "main.orders");
        assert_eq!(result[0].alias, "o");
    }

    #[test]
    fn parse_tables_composite_pk() {
        let result =
            parse_tables_clause("l AS lineitem PRIMARY KEY (l_orderkey, l_linenumber)", 0).unwrap();
        assert_eq!(result[0].pk_columns, vec!["l_orderkey", "l_linenumber"]);
    }

    #[test]
    fn parse_tables_two_words_before_constraint_rejected() {
        // F-7 (code-review 2026-07-16): the table alias is optional, so `o` is
        // read as the table name; the stray second word `orders` before
        // PRIMARY KEY is then unexpected text (a likely `o AS orders` typo with
        // a missing `AS`), reported rather than silently taken.
        let err = parse_tables_clause("o orders PRIMARY KEY (id)", 0).unwrap_err();
        assert!(
            err.message.contains("Unexpected text 'orders'"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_tables_without_primary_key_is_valid() {
        // Phase 33: PRIMARY KEY is optional (fact tables)
        let result = parse_tables_clause("o AS orders", 0).unwrap();
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "orders");
        assert!(result[0].pk_columns.is_empty());
    }

    // -----------------------------------------------------------------------
    // Phase 67 Plan 02 / TECH-DEBT #24: identifier-aware tokenisation of the
    // source-table-name slot. Quoted identifiers with internal whitespace —
    // including ones that contain the literal `PRIMARY KEY` substring — must
    // survive intact through `parse_single_table_entry`.
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_single_table_entry_quoted_with_internal_whitespace() {
        // Quoted name with embedded space; trailing PRIMARY KEY clause.
        let result = parse_tables_clause("o AS \"my orders\" PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "\"my orders\"");
        assert_eq!(result[0].pk_columns, vec!["id"]);
    }

    #[test]
    fn test_parse_single_table_entry_quoted_containing_primary_key_substring() {
        // Canonical TECH-DEBT #24 bug: a quoted source-table name that
        // contains the literal `PRIMARY KEY` substring must NOT be split by
        // the case-insensitive PRIMARY-KEY substring search. The fix is to
        // capture the identifier FIRST using `find_identifier_end`, then run
        // PRIMARY KEY detection only on the post-name slice.
        let result =
            parse_tables_clause("o AS \"weird PRIMARY KEY name\" PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "\"weird PRIMARY KEY name\"");
        assert_eq!(result[0].pk_columns, vec!["id"]);
    }

    #[test]
    fn test_parse_single_table_entry_3part_quoted_fqn_with_whitespace() {
        // 3-part fully-qualified name with internal whitespace in two
        // segments. The dot-separated walk in the new identifier-aware
        // tokeniser must traverse all three segments and preserve the
        // verbatim byte sequence.
        let result =
            parse_tables_clause("o AS \"my db\".\"schema\".\"my table\" PRIMARY KEY (id)", 0)
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "\"my db\".\"schema\".\"my table\"");
        assert_eq!(result[0].pk_columns, vec!["id"]);
    }

    #[test]
    fn test_parse_single_table_entry_regression_no_whitespace() {
        // Regression baseline for the happy path: unquoted dot-qualified
        // name with no whitespace anywhere in the source-table slot.
        // Byte-for-byte identical to the pre-fix `parse_tables_schema_qualified`
        // assertion shape.
        let result = parse_tables_clause("o AS schema.t PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "schema.t");
        assert_eq!(result[0].pk_columns, vec!["id"]);
    }

    #[test]
    fn test_parse_single_table_entry_quoted_with_unique_no_pk() {
        // No PK, trailing UNIQUE clause after a quoted-with-whitespace name.
        // Exercises the no-PK branch of the post-name keyword search.
        let result = parse_tables_clause("f AS \"fact stage\" UNIQUE (email)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "f");
        assert_eq!(result[0].table, "\"fact stage\"");
        assert!(result[0].pk_columns.is_empty());
        assert_eq!(
            result[0].unique_constraints,
            vec![vec!["email".to_string()]]
        );
    }

    // -----------------------------------------------------------------------
    // P-1 (code-review 2026-07-11): text between the source-table name and a
    // PRIMARY KEY / UNIQUE constraint must be rejected, not silently
    // discarded. The anywhere-scan for the constraint keywords previously
    // dropped everything before the match — including a naturally-misplaced
    // COMMENT annotation (silent data loss).
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_single_table_entry_comment_before_pk_rejected() {
        let err = parse_tables_clause(
            "o AS orders COMMENT = 'load-bearing doc' PRIMARY KEY (id)",
            0,
        )
        .unwrap_err();
        assert!(
            err.message
                .contains("between source table name and PRIMARY KEY"),
            "got: {}",
            err.message
        );
        assert!(err.position.is_some(), "error must carry a position");
    }

    #[test]
    fn test_parse_single_table_entry_junk_before_pk_rejected() {
        let err = parse_tables_clause("o AS orders banana PRIMARY KEY (id)", 0).unwrap_err();
        assert!(
            err.message.contains("Unexpected text 'banana'"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_parse_single_table_entry_junk_between_pk_and_unique_rejected() {
        let entry = "o AS orders PRIMARY KEY (id) junk UNIQUE (email)";
        let err = parse_tables_clause(entry, 0).unwrap_err();
        assert!(
            err.message.contains("before UNIQUE"),
            "got: {}",
            err.message
        );
        // Caret must land on the junk token, not the entry start (Copilot
        // review, PR #71).
        assert_eq!(
            err.position,
            Some(entry.find("junk").unwrap()),
            "position should point at 'junk'"
        );
    }

    #[test]
    fn test_parse_single_table_entry_junk_before_pk_caret_position() {
        // Sibling assertion for the PRIMARY KEY guard's caret.
        let entry = "o AS orders banana PRIMARY KEY (id)";
        let err = parse_tables_clause(entry, 0).unwrap_err();
        assert_eq!(
            err.position,
            Some(entry.find("banana").unwrap()),
            "position should point at 'banana'"
        );
    }

    #[test]
    fn test_parse_single_table_entry_annotation_before_unique_rejected() {
        let err = parse_tables_clause("o AS orders COMMENT = 'doc' UNIQUE (email)", 0).unwrap_err();
        assert!(
            err.message.contains("before UNIQUE"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_parse_single_table_entry_full_form_still_parses() {
        // The complete legal ordering is unaffected by the P-1 guards.
        let result = parse_tables_clause(
            "o AS orders PRIMARY KEY (id) UNIQUE (email) COMMENT = 'doc' WITH SYNONYMS = ('ord')",
            0,
        )
        .unwrap();
        assert_eq!(result[0].pk_columns, vec!["id"]);
        assert_eq!(
            result[0].unique_constraints,
            vec![vec!["email".to_string()]]
        );
        assert_eq!(result[0].comment.as_deref(), Some("doc"));
        assert_eq!(result[0].synonyms, vec!["ord".to_string()]);
    }

    #[test]
    fn test_parse_single_table_entry_unique_inside_comment_string_ok() {
        // The constraint scans are quote-aware: keyword text INSIDE the
        // annotation string must not trip the P-1 guards.
        let result = parse_tables_clause(
            "o AS orders PRIMARY KEY (id) COMMENT = 'has UNIQUE and PRIMARY KEY inside'",
            0,
        )
        .unwrap();
        assert_eq!(
            result[0].comment.as_deref(),
            Some("has UNIQUE and PRIMARY KEY inside")
        );
    }

    // -----------------------------------------------------------------------
    // Phase 68 A1 (D-03): bare reserved keywords (PRIMARY, UNIQUE, FOREIGN,
    // REFERENCES, NOT) appearing in the source-table-name slot must surface
    // the pre-Phase-67 literal error message. The keyword set is authoritative
    // per Phase 68 CONTEXT.md D-03; the REVIEW.md draft list is informational.
    // -----------------------------------------------------------------------
    const A1_EXPECTED_MESSAGE: &str =
        "Missing physical table name after AS for alias 'o' in TABLES clause.";

    #[test]
    fn test_parse_single_table_entry_reserved_keyword_after_as_primary() {
        let err = parse_tables_clause("o AS PRIMARY KEY (id)", 0).unwrap_err();
        assert_eq!(err.message, A1_EXPECTED_MESSAGE);
    }

    #[test]
    fn test_parse_single_table_entry_reserved_keyword_after_as_unique() {
        let err = parse_tables_clause("o AS UNIQUE (id)", 0).unwrap_err();
        assert_eq!(err.message, A1_EXPECTED_MESSAGE);
    }

    #[test]
    fn test_parse_single_table_entry_reserved_keyword_after_as_foreign() {
        let err = parse_tables_clause("o AS FOREIGN KEY (id)", 0).unwrap_err();
        assert_eq!(err.message, A1_EXPECTED_MESSAGE);
    }

    #[test]
    fn test_parse_single_table_entry_reserved_keyword_after_as_references() {
        let err = parse_tables_clause("o AS REFERENCES other(id)", 0).unwrap_err();
        assert_eq!(err.message, A1_EXPECTED_MESSAGE);
    }

    #[test]
    fn test_parse_single_table_entry_reserved_keyword_after_as_not() {
        let err = parse_tables_clause("o AS NOT NULL", 0).unwrap_err();
        assert_eq!(err.message, A1_EXPECTED_MESSAGE);
    }

    #[test]
    fn test_parse_single_table_entry_reserved_keyword_after_as_lowercase() {
        // Guard is case-insensitive — `primary` must trigger it just like
        // `PRIMARY` (Phase 68 D-03).
        let err = parse_tables_clause("o AS primary KEY (id)", 0).unwrap_err();
        assert_eq!(err.message, A1_EXPECTED_MESSAGE);
    }

    // -----------------------------------------------------------------------
    // Phase 68 A4: unterminated quoted source-table identifier in TABLES
    // clause must surface a structured ParseError, never silently flow
    // through as a malformed name. Doubled-quote `""` is an escape and must
    // NOT trip the balanced-quote check.
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_single_table_entry_unterminated_quote() {
        let err = parse_tables_clause("o AS \"unclosed", 0).unwrap_err();
        assert!(
            err.message.contains("Unterminated quoted identifier"),
            "expected unterminated-quote error, got: {}",
            err.message
        );
    }

    #[test]
    fn test_parse_single_table_entry_quoted_with_doubled_escape_balanced() {
        // `"a""b"` is balanced — doubled-quote is an escape inside the quoted
        // region, so this must parse successfully.
        let result = parse_tables_clause("o AS \"a\"\"b\" PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "\"a\"\"b\"");
        assert_eq!(result[0].pk_columns, vec!["id"]);
    }

    #[test]
    fn test_parse_single_table_entry_unbalanced_after_doubled_escape() {
        // `"a""b` opens, escapes, then never closes — odd unescaped quote
        // count, must be rejected.
        let err = parse_tables_clause("o AS \"a\"\"b PRIMARY KEY (id)", 0).unwrap_err();
        assert!(
            err.message.contains("Unterminated quoted identifier"),
            "expected unterminated-quote error, got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // Phase 68 A7, re-pinned at the clause level after the §6.1 TABLES
    // migration (code-review 2026-07-11): a constraint keyword is now a token,
    // so word-boundary correctness is structural. A source-table name whose
    // bytes merely CONTAIN `PRIMARY` (`my_primary`, underscore-joined) is a
    // single identifier token and must not be read as a PRIMARY KEY constraint.
    // -----------------------------------------------------------------------

    #[test]
    fn test_underscore_joined_primary_in_name_is_not_a_constraint() {
        // `my_primary` is one identifier token — no PRIMARY KEY here, so the
        // table is a bare (no-PK) table, not a table named `my` with a PK.
        let result = parse_tables_clause("o AS my_primary", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].table, "my_primary");
        assert!(result[0].pk_columns.is_empty());
    }

    // -----------------------------------------------------------------------
    // PA-1 / PA-2 regressions (code-review 2026-07-02): byte-indexed keyword
    // scanning panicked mid-codepoint on non-ASCII input, and the local
    // single-quote extractor Latin-1-ized non-ASCII COMMENT / SYNONYMS
    // payloads (`'café'` → `cafÃ©`).
    // -----------------------------------------------------------------------

    #[test]
    fn test_keyword_scan_over_non_ascii_no_panic() {
        // PA-1/PA-2: keyword scanning over non-ASCII input must not panic and
        // must still find a keyword after a multi-byte run. This is now
        // structural — the lexer consumes each multi-byte codepoint whole and
        // the cursor matches keywords by token — so it is exercised via the
        // cursor (replacing the retired `find_keyword_ci` scan test).
        use super::cursor::Cursor;
        assert!(Cursor::new("ÉÉÉ AS x", 0).find_kw("AS").is_some());
        assert!(Cursor::new("東京東京", 0).find_kw("AS").is_none());
    }

    #[test]
    fn test_leading_symbol_in_name_slot_is_missing_name() {
        // §6.1 (PR #100 review): when the name slot begins with a non-`(`/`;`
        // symbol (`.foo`, `=x`, `-1`), the token cursor surfaces the missing-name
        // error at parse time. The pre-migration `find_identifier_end` folded a
        // leading `.`/`=`/`-` INTO the table name (a latent bug — no SQL
        // identifier starts with one), so this is a deliberate, stricter
        // behaviour on input the old code left undefined-by-test. Pinned here so
        // it stays intentional rather than drifting.
        for entry in ["o AS .foo", "o AS =x", "o AS -1"] {
            let err = parse_tables_clause(entry, 0).unwrap_err();
            assert!(
                err.message.contains("Missing physical table name"),
                "{entry}: got {}",
                err.message
            );
        }
    }

    #[test]
    fn test_bare_paren_immediately_after_name_terminates_name() {
        // §6.1 (PR #100 review): the name-capture loop stops at a contiguous
        // `(`, mirroring `find_identifier_end(.., allow_paren = true)`. The stop
        // is expressed as the or-pattern `Symbol(b'(' | b';')` — this test would
        // fail (name greedily swallowing `(foo)`, parsing to Ok) if that arm
        // ever stopped matching `(`. Expected: name is `v`, and the trailing
        // `(foo)` is reported as unexpected text, not folded into the name.
        let err = parse_tables_clause("o AS v(foo)", 0).unwrap_err();
        assert!(
            err.message.contains("after table declaration"),
            "got: {}",
            err.message
        );
        // A contiguous `;` likewise ends the name (the other or-pattern arm).
        let err2 = parse_tables_clause("o AS v;junk", 0).unwrap_err();
        assert!(
            err2.message.contains("after table declaration"),
            "got: {}",
            err2.message
        );
    }

    #[test]
    fn test_non_ascii_name_then_primary_key_no_panic() {
        // A non-ASCII quoted source-table name followed by PRIMARY KEY must
        // tokenize without a mid-codepoint panic and still resolve the
        // constraint (was `find_primary_key`'s PA-1 byte-scan guarantee).
        let result = parse_tables_clause("o AS \"CAFÉ\" PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].table, "\"CAFÉ\"");
        assert_eq!(result[0].pk_columns, vec!["id"]);
        // Pure non-ASCII with no constraint keyword: bare table, no panic.
        let bare = parse_tables_clause("o AS \"ΩΩ Ω\"", 0).unwrap();
        assert!(bare[0].pk_columns.is_empty());
    }

    #[test]
    fn test_comment_annotation_non_ascii_payload_survives() {
        // PA-2: the pre-fix extractor stored 'café et plus' as mojibake.
        let (expr, ann) =
            parse_trailing_annotations("SUM(o.amount) COMMENT = 'café et plus'", 0).unwrap();
        assert_eq!(expr, "SUM(o.amount)");
        assert_eq!(ann.comment.as_deref(), Some("café et plus"));
    }

    #[test]
    fn test_synonyms_annotation_non_ascii_payload_survives() {
        let (expr, ann) =
            parse_trailing_annotations("o.city WITH SYNONYMS = ('ciudad', 'stadt', '都市')", 0)
                .unwrap();
        assert_eq!(expr, "o.city");
        assert_eq!(ann.synonyms, vec!["ciudad", "stadt", "都市"]);
    }

    #[test]
    fn test_annotation_scan_non_ascii_expression_no_panic() {
        // Multi-byte chars ahead of the annotation keywords exercise the
        // depth-0 scanner's byte loop.
        let (expr, ann) =
            parse_trailing_annotations("concat(city, ' – ') COMMENT = 'ok'", 0).unwrap();
        assert_eq!(expr, "concat(city, ' – ')");
        assert_eq!(ann.comment.as_deref(), Some("ok"));
    }

    // -----------------------------------------------------------------------
    // P-2 (code-review 2026-07-11): the annotation region must be tiled
    // exactly — duplicate clauses, malformed clauses, and trailing junk are
    // rejected instead of being silently dropped/accepted.
    // -----------------------------------------------------------------------

    #[test]
    fn test_annotation_both_orders_still_parse() {
        // Regression: valid single-clause and both-order forms are unaffected.
        let (_, a) =
            parse_trailing_annotations("x COMMENT = 'c' WITH SYNONYMS = ('s')", 0).unwrap();
        assert_eq!(a.comment.as_deref(), Some("c"));
        assert_eq!(a.synonyms, vec!["s"]);
        let (_, b) =
            parse_trailing_annotations("x WITH SYNONYMS = ('s') COMMENT = 'c'", 0).unwrap();
        assert_eq!(b.comment.as_deref(), Some("c"));
        assert_eq!(b.synonyms, vec!["s"]);
    }

    #[test]
    fn test_annotation_duplicate_comment_rejected() {
        // Previously the second COMMENT was silently dropped.
        let err = parse_trailing_annotations("x COMMENT = 'a' COMMENT = 'b'", 0).unwrap_err();
        assert!(
            err.message.contains("Duplicate COMMENT"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_annotation_duplicate_synonyms_rejected() {
        let err = parse_trailing_annotations("x WITH SYNONYMS = ('a') WITH SYNONYMS = ('b')", 0)
            .unwrap_err();
        assert!(
            err.message.contains("Duplicate WITH SYNONYMS"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_annotation_trailing_garbage_rejected() {
        // Previously `banana` was silently accepted and discarded.
        let err = parse_trailing_annotations("x COMMENT = 'a' banana", 0).unwrap_err();
        assert!(
            err.message.contains("Unexpected text in annotations"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_annotation_keyword_word_boundary_preserved() {
        // A column-ish token that merely starts with COMMENT must not be
        // mistaken for the keyword — it stays part of the expression.
        let (expr, ann) = parse_trailing_annotations("commentary_col", 0).unwrap();
        assert_eq!(expr, "commentary_col");
        assert!(ann.comment.is_none() && ann.synonyms.is_empty());
    }

    #[test]
    fn test_annotation_with_without_synonyms_rejected() {
        // A second WITH clause that isn't WITH SYNONYMS is an error, not junk.
        let err = parse_trailing_annotations("x COMMENT = 'a' WITH FOO", 0).unwrap_err();
        assert!(
            err.message.contains("Expected SYNONYMS after WITH"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn f15_annotation_errors_carry_carets() {
        // F-15 (code-review 2026-07-16): annotation-path errors used to carry
        // `position: None`. They now anchor at the offending token, offset by
        // the caller-supplied `base_offset`.
        let text = "x COMMENT = 'a' banana";
        let base = 100;
        let err = parse_trailing_annotations(text, base).unwrap_err();
        // Caret points at `banana` (the unexpected trailing text).
        let banana_at = base + text.find("banana").unwrap();
        assert_eq!(
            err.position,
            Some(banana_at),
            "caret should point at the trailing junk: {}",
            err.message
        );

        // Duplicate COMMENT anchors at the second COMMENT keyword.
        let dup = "x COMMENT = 'a' COMMENT = 'b'";
        let err = parse_trailing_annotations(dup, 0).unwrap_err();
        let second_comment = dup.rfind("COMMENT").unwrap();
        assert_eq!(err.position, Some(second_comment), "got: {}", err.message);

        // A malformed synonym (missing quotes) anchors inside the paren list.
        let bad_syn = "x WITH SYNONYMS = (foo)";
        let err = parse_trailing_annotations(bad_syn, 0).unwrap_err();
        assert_eq!(
            err.position,
            Some(bad_syn.find("foo").unwrap()),
            "got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // PA-3 (code-review 2026-07-02): keyword scanners must not match inside
    // string literals. Pre-fix, a COMMENT payload mentioning PRIMARY KEY
    // fabricated pk_columns from comment text and discarded the comment.
    // -----------------------------------------------------------------------

    #[test]
    fn test_primary_key_inside_comment_string_not_fabricated() {
        let result =
            parse_tables_clause("o AS orders COMMENT = 'the PRIMARY KEY (id) lives here'", 0)
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].pk_columns, Vec::<String>::new());
        assert_eq!(
            result[0].comment.as_deref(),
            Some("the PRIMARY KEY (id) lives here")
        );
    }

    #[test]
    fn test_unique_inside_comment_string_not_fabricated() {
        let result = parse_tables_clause(
            "o AS orders PRIMARY KEY (id) COMMENT = 'a UNIQUE (x) mention'",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].pk_columns, vec!["id"]);
        assert!(result[0].unique_constraints.is_empty());
        assert_eq!(result[0].comment.as_deref(), Some("a UNIQUE (x) mention"));
    }

    // -----------------------------------------------------------------------
    // PA-9 (code-review 2026-07-02): table-level COMMENT / WITH SYNONYMS on a
    // table with no PK/UNIQUE used to be silently dropped (remainder was
    // hard-set to ""), and trailing junk was silently ignored.
    // -----------------------------------------------------------------------

    #[test]
    fn test_no_pk_table_comment_and_synonyms_preserved() {
        let result = parse_tables_clause(
            "li AS line_items COMMENT = 'fact rows' WITH SYNONYMS = ('items', 'lines')",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].pk_columns, Vec::<String>::new());
        assert_eq!(result[0].comment.as_deref(), Some("fact rows"));
        assert_eq!(result[0].synonyms, vec!["items", "lines"]);
    }

    #[test]
    fn test_table_entry_trailing_garbage_errors() {
        let err = parse_tables_clause("o AS orders garbage COMMENT = 'x'", 0).unwrap_err();
        assert!(
            err.message.contains("Unexpected text"),
            "expected trailing-garbage error, got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // PA-6 (code-review 2026-07-02): depth-0 scanners must honour
    // double-quoted identifiers.
    // -----------------------------------------------------------------------

    #[test]
    fn test_split_commas_ignores_comma_inside_quoted_ident() {
        let entries = split_at_depth0_commas("o.x AS o.\"a,b\", o.y AS o.c").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1, "o.x AS o.\"a,b\"");
        assert_eq!(entries[1].1, "o.y AS o.c");
    }

    #[test]
    fn test_quoted_ident_with_paren_does_not_close_clause() {
        // `"tbl)x"` inside TABLES must not close the TABLES clause early.
        let def = parse_keyword_body(
            "AS TABLES (o AS \"tbl)x\" PRIMARY KEY (id)) DIMENSIONS (o.d AS o.c)",
            0,
        )
        .unwrap();
        assert_eq!(def.tables.len(), 1);
        assert_eq!(def.tables[0].table, "\"tbl)x\"");
        assert_eq!(def.dimensions.len(), 1);
    }

    #[test]
    fn test_dot_inside_quoted_name_not_a_qualifier() {
        // Dimension bare name `"a.b"` — the inner dot is not a separator.
        let entries = parse_qualified_entries("o.\"a.b\" AS o.c", 0, false, "dimensions").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source_alias, "o");
        assert_eq!(entries[0].name, "\"a.b\"");
    }

    #[test]
    fn test_private_fact_structural_error_caret_after_modifier() {
        // §6.1 (phase 3, PR #102 review): the cursor base accounts for the
        // stripped PRIVATE/PUBLIC modifier, so a structural-error caret lands in
        // the alias.name region, not inside "PRIVATE ". `PRIVATE o.x` has no AS.
        let entry = "PRIVATE o.x";
        let err = parse_qualified_entries(entry, 0, true, "facts").unwrap_err();
        let pos = err.position.expect("caret");
        assert!(
            pos >= entry.find("o.x").unwrap(),
            "caret {pos} drifted into the access modifier of {entry:?}"
        );
    }

    #[test]
    fn test_quoted_dot_in_alias_not_a_qualifier() {
        // §6.1 (phase 3): the qualifier `.` is the first `.` SYMBOL token, so a
        // dot inside a QUOTED alias is inert — the split happens at the dot
        // AFTER the closing quote. (Quote-aware already, now token-structural.)
        let entries = parse_qualified_entries("\"a.b\".name AS 1", 0, false, "dimensions").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source_alias, "\"a.b\"");
        assert_eq!(entries[0].name, "name");
        assert_eq!(entries[0].expr, "1");
    }

    #[test]
    fn test_keyword_not_matched_before_non_ascii_continuation() {
        // PR #50 review: keyword boundary checks treated non-ASCII bytes as
        // boundaries, so COMMENT matched inside the identifier `commenté`
        // and the annotation scanner truncated the expression.
        let entries = parse_qualified_entries("o.x AS o.commenté", 0, false, "dimensions").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].expr, "o.commenté");
        assert_eq!(entries[0].comment, None);
    }

    // PARSE-6: the annotation scanner's word-boundary check treated `.` as a
    // boundary, so the `comment` in a QUALIFIED column reference started the
    // annotation region — leaving the dangling `o.` as the stored expression.
    // `graph/member_refs.rs` skips bare chains, so CREATE succeeded and the
    // user's predicate was silently discarded; the corruption surfaced only as
    // a query-time syntax error, and GET_DDL round-tripped the wreckage.
    //
    // DuckDB accepts unquoted `comment`/`labels` as column names, so by this
    // project's dialect rule (Snowflake decides semantics, DuckDB decides the
    // host language) the unquoted qualified form has to work.
    #[test]
    fn test_unquoted_qualified_comment_column_is_not_an_annotation() {
        let entries =
            parse_qualified_entries("o.d AS o.comment = 'x'", 0, false, "dimensions").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].expr, "o.comment = 'x'",
            "a qualified column named `comment` is part of the expression, not an annotation"
        );
        assert_eq!(entries[0].comment, None);
    }

    #[test]
    fn test_unquoted_qualified_labels_column_is_not_an_annotation() {
        let entries =
            parse_qualified_entries("o.d AS o.labels IS NOT NULL", 0, false, "dimensions").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].expr, "o.labels IS NOT NULL");
    }

    // DuckDB accepts whitespace around the qualifier dot, so the boundary walks
    // back over it rather than looking only at the immediately preceding byte.
    #[test]
    fn test_spaced_qualified_comment_column_is_not_an_annotation() {
        let entries =
            parse_qualified_entries("o.d AS o . comment = 'x'", 0, false, "dimensions").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].expr, "o . comment = 'x'");
        assert_eq!(entries[0].comment, None);
    }

    // The bare (unqualified) keyword must still be read as an annotation — the
    // fix narrows the boundary, it must not disable detection.
    #[test]
    fn test_bare_comment_keyword_still_parses_as_annotation() {
        let entries =
            parse_qualified_entries("o.d AS o.region COMMENT = 'hi'", 0, false, "dimensions")
                .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].expr, "o.region");
        assert_eq!(entries[0].comment.as_deref(), Some("hi"));
    }

    #[test]
    fn test_quoted_comment_column_usable_in_expression() {
        // PA-9 companion: a column literally named `comment` is usable at
        // depth 0 when quoted — the annotation scanner must not treat the
        // quoted identifier as the COMMENT keyword.
        let entries =
            parse_qualified_entries("o.note AS o.\"comment\"", 0, false, "dimensions").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].expr, "o.\"comment\"");
        assert_eq!(entries[0].comment, None);
    }

    // PARSE-7: DuckDB accepts Postgres-style escape strings (`E'...'`) where
    // `\'` is an escaped quote -- `SELECT e'\''` returns one quote character.
    // Every scanner in the crate treated `\` as an ordinary byte, so the middle
    // and closing quotes were consumed as one `''` escape pair and the entry
    // was rejected as an unterminated literal. This is the review's repro.
    #[test]
    fn test_escape_string_with_escaped_quote_parses_in_a_metric_entry() {
        let entries =
            parse_qualified_entries("o.m AS count_if(x = e'\\'')", 0, false, "metrics").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].expr, "count_if(x = e'\\'')");
    }

    // The worse, silent half: a `,` after the escape string stayed inside the
    // phantom literal, so a two-entry body collapsed into one mis-split entry.
    #[test]
    fn test_escape_string_does_not_swallow_the_entry_separator() {
        let entries = parse_qualified_entries(
            "o.a AS count_if(x = e'\\''), o.b AS SUM(y)",
            0,
            false,
            "metrics",
        )
        .unwrap();
        assert_eq!(entries.len(), 2, "entries: {entries:?}");
        assert_eq!(entries[1].expr, "SUM(y)");
    }

    // Control: a typed literal's `E` is part of the type name, not an escape
    // introducer -- `DATE'2020-01-01'` is ordinary DuckDB SQL.
    #[test]
    fn test_typed_literal_entry_still_splits_normally() {
        let entries = parse_qualified_entries(
            "o.a AS x > DATE'2020-01-01', o.b AS SUM(y)",
            0,
            false,
            "metrics",
        )
        .unwrap();
        assert_eq!(entries.len(), 2, "entries: {entries:?}");
        assert_eq!(entries[0].expr, "x > DATE'2020-01-01'");
    }

    #[test]
    fn test_unterminated_quote_in_dimension_entry_errors() {
        let err =
            parse_qualified_entries("o.x AS o.\"unclosed", 0, false, "dimensions").unwrap_err();
        assert!(
            err.message.contains("Unterminated quoted identifier"),
            "got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // Phase 68 A5: mixed bare/quoted dot-qualified source-table names must
    // parse correctly. The dot-walk inside `find_identifier_end` already
    // handles this case (its `fqn_with_quoted_parts_runs_to_whitespace`
    // doctest covers it at the helper level); these tests pin the
    // parse_tables_clause contract end-to-end.
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_single_table_entry_mixed_quoted_and_bare() {
        // Bare schema segment followed by quoted-with-whitespace table segment.
        let result = parse_tables_clause("o AS staging.\"my orders\" PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "staging.\"my orders\"");
        assert_eq!(result[0].pk_columns, vec!["id"]);

        // Symmetric case: quoted-with-whitespace database segment, then bare
        // schema + bare table.
        let result = parse_tables_clause("o AS \"my db\".sch.t PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "\"my db\".sch.t");
        assert_eq!(result[0].pk_columns, vec!["id"]);
    }

    // -----------------------------------------------------------------------
    // Phase 68 Plan 03 (B1) / TECH-DEBT #25: identifier-aware tokenisation of
    // the dim_name slot inside `NON ADDITIVE BY (...)`. Quoted identifiers with
    // internal whitespace AND dotted paths (`table.col`, D-08) must survive
    // intact through `parse_non_additive_dims`. `parse_non_additive_dims` is
    // private — exercise via the public `parse_metrics_clause` entry.
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_non_additive_dims_quoted_identifier_with_whitespace() {
        // Quoted dim_name with embedded space. DESC defaults to NULLS FIRST.
        let result = parse_metrics_clause(
            "a.balance NON ADDITIVE BY (\"my dim\" DESC) AS SUM(a.balance)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let na_vec = &result[0].non_additive_by;
        assert_eq!(na_vec.len(), 1);
        let na = &na_vec[0];
        assert_eq!(na.dimension, "\"my dim\"");
        assert_eq!(na.order, SortOrder::Desc);
        assert_eq!(na.nulls, NullsOrder::First); // DESC default
    }

    #[test]
    fn parse_metrics_using_nested_as_splits_at_structural_as() {
        // #103 review follow-up: an `AS` token nested inside the `USING (...)`
        // list must not be mistaken for the structural `AS`. The split now
        // lands on the depth-0 `AS`, so the expression is the whole `SUM(v)`,
        // not `b) AS SUM(v)`, and the USING list is `a AS b` verbatim.
        let result = parse_metrics_clause("s.m USING (a AS b) AS SUM(v)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias.as_deref(), Some("s"));
        assert_eq!(result[0].name, "m");
        assert_eq!(result[0].expr, "SUM(v)");
        assert_eq!(result[0].using_relationships, vec!["a AS b"]);
    }

    #[test]
    fn test_parse_non_additive_dims_dotted_path() {
        // Dotted-path dim_name (D-08 contract extension): `o."my dim"` must be
        // captured as a single identifier including the dot. Two-entry clause
        // confirms comma splitting still works.
        let result = parse_metrics_clause(
            "a.balance NON ADDITIVE BY (o.\"my dim\" ASC NULLS LAST, c2 DESC) AS SUM(a.balance)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let na_vec = &result[0].non_additive_by;
        assert_eq!(na_vec.len(), 2);

        let na0 = &na_vec[0];
        assert_eq!(na0.dimension, "o.\"my dim\"");
        assert_eq!(na0.order, SortOrder::Asc);
        assert_eq!(na0.nulls, NullsOrder::Last);

        let na1 = &na_vec[1];
        assert_eq!(na1.dimension, "c2");
        assert_eq!(na1.order, SortOrder::Desc);
        assert_eq!(na1.nulls, NullsOrder::First); // DESC default
    }

    #[test]
    fn test_keyword_boundaries_reject_underscore_and_non_ascii_continuation() {
        // PR #50 review: post-keyword boundary checks after BY / EXCLUDING
        // accepted `_` and non-ASCII bytes as boundaries, so `BY_foo` /
        // `EXCLUDING_foo` mis-tokenized as the keyword ending early. Such
        // identifiers must NOT activate the keyword.
        let r = parse_metrics_clause("a.bal NON ADDITIVE BY_x (d) AS SUM(a.bal)", 0);
        if let Ok(v) = r {
            assert!(
                v[0].non_additive_by.is_empty(),
                "BY_x must not match the NON ADDITIVE BY keyword"
            );
        }

        let r = parse_metrics_clause(
            "s.r AS SUM(t) OVER (PARTITION BY_x ORDER BY d ASC NULLS LAST)",
            0,
        );
        if let Ok(v) = r {
            let ws = v[0].window_spec.as_ref().expect("window spec");
            assert!(
                ws.partition_dims.is_empty(),
                "PARTITION BY_x must not match PARTITION BY: {:?}",
                ws.partition_dims
            );
        }

        let r = parse_metrics_clause(
            "s.r AS SUM(t) OVER (PARTITION BY EXCLUDING_x ORDER BY d ASC NULLS LAST)",
            0,
        );
        if let Ok(v) = r {
            let ws = v[0].window_spec.as_ref().expect("window spec");
            assert!(
                ws.excluding_dims.is_empty(),
                "EXCLUDING_x must not match EXCLUDING: {:?}",
                ws.excluding_dims
            );
        }

        // The legitimate forms keep working.
        let v = parse_metrics_clause(
            "s.r AS SUM(t) OVER (PARTITION BY EXCLUDING region ORDER BY d ASC NULLS LAST)",
            0,
        )
        .unwrap();
        let ws = v[0].window_spec.as_ref().expect("window spec");
        assert_eq!(ws.excluding_dims, vec!["region"]);
    }

    #[test]
    fn metric_non_additive_missing_paren_caret_accounts_for_access_modifier() {
        // P-4 (code-review 2026-07-11): the "Expected '(' after NON ADDITIVE
        // BY" caret is recovered from the failing token's slice, so it points
        // at that token even when a leading PRIVATE modifier shifts the
        // name portion off the entry origin (the old `entry_offset + na_end`
        // arithmetic drifted left by the modifier's length).
        let entry = "PRIVATE revenue NON ADDITIVE BY date AS SUM(x)";
        let err = parse_metrics_clause(entry, 0).unwrap_err();
        assert!(
            err.message.contains("Expected '(' after NON ADDITIVE BY"),
            "got: {}",
            err.message
        );
        let expected = entry.find("date").unwrap();
        assert_eq!(
            err.position,
            Some(expected),
            "caret should point at the token where '(' is expected"
        );
    }

    // -----------------------------------------------------------------------
    // F-3 (code-review 2026-07-16): text between a metric's USING (...) /
    // NON ADDITIVE BY (...) clause and AS was matched-then-sliced-backwards
    // and silently discarded.
    // -----------------------------------------------------------------------

    #[test]
    fn test_metric_junk_after_non_additive_by_rejected() {
        let err = parse_metrics_clause("o.m NON ADDITIVE BY (d) junk AS SUM(o.v)", 0).unwrap_err();
        assert!(
            err.message
                .contains("Unexpected text 'junk' after NON ADDITIVE BY (...)"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_metric_junk_after_using_rejected() {
        let err = parse_metrics_clause("o.m USING (r) junk AS SUM(o.v)", 0).unwrap_err();
        assert!(
            err.message
                .contains("Unexpected text 'junk' after USING (...)"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_metric_using_then_non_additive_by_still_parses() {
        // Guard: the legitimate `USING (...) NON ADDITIVE BY (...)` order is
        // still accepted (residue checks must not reject the real grammar).
        let result =
            parse_metrics_clause("o.m USING (r) NON ADDITIVE BY (d) AS SUM(o.v)", 0).unwrap();
        assert_eq!(result[0].using_relationships, vec!["r"]);
        assert_eq!(result[0].non_additive_by.len(), 1);
        assert_eq!(result[0].name, "m");
    }

    #[test]
    fn test_as_keyword_requires_boundary_in_tables_and_materializations() {
        // PR #50 review: `AS` was matched as a raw 2-byte prefix, so
        // `ASorders` / `ASx` were treated as the AS keyword ending early.
        // `ASorders` is a single bare token, never the AS keyword. With the
        // alias now optional (F-7, code-review 2026-07-16), `o` is read as the
        // table name and `ASorders` is unexpected text before PRIMARY KEY —
        // still rejected, just no longer phrased as "Expected 'AS'".
        let err = parse_tables_clause("o ASorders PRIMARY KEY (id)", 0).unwrap_err();
        assert!(
            err.message.contains("Unexpected text 'ASorders'"),
            "got: {}",
            err.message
        );
        let err = parse_materializations_clause("m1 ASx (TABLE t, DIMENSIONS (d))", 0).unwrap_err();
        assert!(
            err.message.contains("Expected 'AS'"),
            "got: {}",
            err.message
        );
        // Punctuation stays a legal boundary: AS"quoted" and AS( work.
        let result = parse_tables_clause("o AS\"my tbl\" PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result[0].table, "\"my tbl\"");
        let result = parse_materializations_clause("m1 AS(TABLE t, DIMENSIONS (d))", 0).unwrap();
        assert_eq!(result[0].table, "t");
    }

    #[test]
    fn test_over_clause_error_position_is_expression_relative() {
        // PR #50 review: OVER-clause errors were based at the entry start,
        // so carets pointed at the metric name instead of the expression.
        // Entry: "s.r AS SUM(t) OVER bad" — expr starts at byte 7, OVER at
        // byte 7 within the expr, error points just past "OVER" (byte 18).
        let err = parse_metrics_clause("s.r AS SUM(t) OVER bad", 0).unwrap_err();
        assert!(
            err.message.contains("Expected '(' after OVER"),
            "got: {}",
            err.message
        );
        assert_eq!(err.position, Some(18), "caret must sit after OVER");
    }

    #[test]
    fn test_over_partition_and_order_by_with_interior_whitespace() {
        // PR #50 review: the remainder after PARTITION BY dims was located
        // by len() subtraction over a trim()med slice. Exercise the full
        // PARTITION BY -> ORDER BY -> frame path with generous interior
        // whitespace so ORDER BY / frame detection must survive the
        // remainder-offset computation.
        let result = parse_metrics_clause(
            "s.r AS SUM(qty) OVER (PARTITION BY region   ORDER BY d ASC NULLS LAST   ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)",
            0,
        )
        .unwrap();
        let ws = result[0].window_spec.as_ref().expect("window spec");
        assert_eq!(ws.partition_dims, vec!["region"]);
        assert_eq!(ws.order_by.len(), 1, "ORDER BY must survive: {ws:?}");
        assert_eq!(ws.order_by[0].expr, "d");
        assert_eq!(
            ws.frame_clause.as_deref(),
            Some("ROWS BETWEEN 1 PRECEDING AND CURRENT ROW")
        );

        // EXCLUDING branch, same shape.
        let result = parse_metrics_clause(
            "s.r AS SUM(qty) OVER (PARTITION BY EXCLUDING region   ORDER BY d DESC)",
            0,
        )
        .unwrap();
        let ws = result[0].window_spec.as_ref().expect("window spec");
        assert_eq!(ws.excluding_dims, vec!["region"]);
        assert_eq!(ws.order_by.len(), 1);
        assert_eq!(ws.order_by[0].expr, "d");
    }

    #[test]
    fn test_over_partition_dim_named_like_frame_keyword() {
        // §6.1 (phase 5, PR #104 review): a PARTITION/EXCLUDING dim named like a
        // frame keyword (`groups`/`rows`/`range`) followed by ORDER BY stays a
        // dim — the dims boundary prefers ORDER over frame keywords, matching
        // the pre-migration `find_keyword_ci("ORDER").or_else(find_frame_start)`.
        // (Neither the old nor the new suite covered this boundary before.)
        let result = parse_metrics_clause(
            "s.r AS SUM(qty) OVER (PARTITION BY EXCLUDING region, groups ORDER BY d)",
            0,
        )
        .unwrap();
        let ws = result[0].window_spec.as_ref().expect("window spec");
        assert_eq!(ws.excluding_dims, vec!["region", "groups"]);
        assert_eq!(ws.order_by.len(), 1);
        assert_eq!(ws.order_by[0].expr, "d");
        // Plain PARTITION BY with a frame-keyword-named dim + ORDER BY.
        let result = parse_metrics_clause(
            "s.r AS SUM(qty) OVER (PARTITION BY region, rows ORDER BY d)",
            0,
        )
        .unwrap();
        assert_eq!(
            result[0].window_spec.as_ref().unwrap().partition_dims,
            vec!["region", "rows"]
        );
    }

    // -----------------------------------------------------------------------
    // P-3 (code-review 2026-07-11): the OVER-clause parser must not silently
    // degrade malformed content. `ORDER` without an adjacent `BY` previously
    // became the frame clause; junk between ORDER and BY was skipped; any
    // residue was stored verbatim as a frame clause.
    // -----------------------------------------------------------------------

    #[test]
    fn test_over_order_without_by_rejected() {
        let err = parse_metrics_clause("s.r AS SUM(t) OVER (PARTITION BY region ORDER d)", 0)
            .unwrap_err();
        assert!(
            err.message.contains("Expected BY immediately after ORDER"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_over_order_junk_before_by_rejected() {
        let err = parse_metrics_clause(
            "s.r AS SUM(t) OVER (PARTITION BY region ORDER banana BY d)",
            0,
        )
        .unwrap_err();
        assert!(
            err.message.contains("Expected BY immediately after ORDER"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_over_junk_content_rejected_as_frame() {
        let err = parse_metrics_clause("s.r AS SUM(t) OVER (banana)", 0).unwrap_err();
        assert!(
            err.message
                .contains("Expected frame clause starting with ROWS, RANGE, or GROUPS"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_over_order_by_frame_keyword_name_rejected() {
        // An unquoted reference named like a frame keyword is claimed by
        // find_frame_start, leaving zero ORDER BY entries — must error, not
        // silently produce an orderless window with a bogus frame.
        let err = parse_metrics_clause("s.r AS SUM(t) OVER (ORDER BY range)", 0).unwrap_err();
        assert!(
            err.message
                .contains("Expected column reference after ORDER BY"),
            "got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // F-2 (code-review 2026-07-16): content before PARTITION BY in an OVER
    // clause was matched-anywhere and silently discarded. A misplaced
    // `ORDER BY` before PARTITION BY previously produced an unordered
    // aggregate with no diagnostic.
    // -----------------------------------------------------------------------

    #[test]
    fn test_over_order_by_before_partition_by_rejected() {
        let err = parse_metrics_clause("s.r AS SUM(t) OVER (ORDER BY d PARTITION BY region)", 0)
            .unwrap_err();
        assert!(
            err.message
                .contains("Unexpected text 'ORDER BY d' before PARTITION BY"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_over_junk_before_partition_by_rejected() {
        let err =
            parse_metrics_clause("s.r AS SUM(t) OVER (banana PARTITION BY region)", 0).unwrap_err();
        assert!(
            err.message
                .contains("Unexpected text 'banana' before PARTITION BY"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_over_well_formed_partition_order_still_parses() {
        // Guard against over-eager rejection: the canonical order is accepted.
        let result =
            parse_metrics_clause("s.r AS SUM(t) OVER (PARTITION BY region ORDER BY d)", 0).unwrap();
        let ws = result[0].window_spec.as_ref().unwrap();
        assert_eq!(ws.partition_dims, vec!["region"]);
        assert_eq!(ws.order_by.len(), 1);
    }

    #[test]
    fn test_over_frame_only_still_parses() {
        let result = parse_metrics_clause(
            "s.r AS SUM(t) OVER (ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)",
            0,
        )
        .unwrap();
        let ws = result[0].window_spec.as_ref().expect("window spec");
        assert!(ws.order_by.is_empty());
        assert_eq!(
            ws.frame_clause.as_deref(),
            Some("ROWS BETWEEN 1 PRECEDING AND CURRENT ROW")
        );
    }

    #[test]
    fn test_non_additive_by_flexible_spacing() {
        // PA-10: the keyword offset was hardcoded as 16 ("NON ADDITIVE BY"
        // + one space), rejecting the no-space `BY(d)` form and extra
        // inter-keyword whitespace.
        for entry in [
            "a.balance NON ADDITIVE BY(report_date) AS SUM(a.balance)",
            "a.balance NON  ADDITIVE   BY (report_date) AS SUM(a.balance)",
        ] {
            let result = parse_metrics_clause(entry, 0)
                .unwrap_or_else(|e| panic!("{entry} failed: {}", e.message));
            assert_eq!(result.len(), 1);
            let na_vec = &result[0].non_additive_by;
            assert_eq!(na_vec.len(), 1, "for {entry}");
            assert_eq!(na_vec[0].dimension, "report_date");
        }
    }

    #[test]
    fn test_parse_non_additive_dims_unterminated_quote() {
        // Unterminated quoted dim_name must surface a structured ParseError
        // with the expected wording. Mirrors the A4 TABLES-clause contract.
        let err = parse_metrics_clause(
            "a.balance NON ADDITIVE BY (\"unclosed DESC) AS SUM(a.balance)",
            0,
        )
        .unwrap_err();
        assert!(
            err.message.contains("Unterminated quoted identifier"),
            "Expected unterminated-quote error, got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // Phase 68 Plan 03 (B2) / TECH-DEBT #25: identifier-aware tokenisation of
    // the OVER ORDER BY column-reference slot. Quoted identifiers with
    // internal whitespace AND dotted paths (`table.col`, D-08) must survive
    // intact through `parse_over_content` / `parse_window_over_clause`.
    // `parse_window_spec` / `parse_over_content` is private — exercise via the
    // public `parse_metrics_clause` entry which routes a window-metric expression
    // through `parse_window_over_clause`. The parsed `WindowSpec` lives in the
    // `ParsedMetric.window_spec` field.
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_window_spec_quoted_order_by() {
        // Quoted identifier with embedded space in OVER ORDER BY entry.
        let result = parse_metrics_clause(
            "s.running AS AVG(qty) OVER (PARTITION BY EXCLUDING r ORDER BY \"order date\" ASC NULLS LAST)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let ws = result[0]
            .window_spec
            .as_ref()
            .expect("window_spec must be Some");
        assert_eq!(ws.order_by.len(), 1);
        let ob = &ws.order_by[0];
        assert_eq!(ob.expr, "\"order date\"");
        assert_eq!(ob.order, SortOrder::Asc);
        assert_eq!(ob.nulls, NullsOrder::Last);
    }

    #[test]
    fn test_parse_window_spec_dotted_order_by() {
        // Dotted-path column ref (D-08): `o."order date"` must be captured as a
        // single identifier including the dot.
        let result = parse_metrics_clause(
            "s.running AS AVG(qty) OVER (PARTITION BY EXCLUDING r ORDER BY o.\"order date\" DESC)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let ws = result[0]
            .window_spec
            .as_ref()
            .expect("window_spec must be Some");
        assert_eq!(ws.order_by.len(), 1);
        let ob = &ws.order_by[0];
        assert_eq!(ob.expr, "o.\"order date\"");
        assert_eq!(ob.order, SortOrder::Desc);
        // DESC defaults to NULLS FIRST in window ORDER BY arm (matches NAB).
        assert_eq!(ob.nulls, NullsOrder::First);
    }

    #[test]
    fn test_parse_window_spec_unterminated_quote_order_by() {
        // Unterminated quoted column ref surfaces structured ParseError.
        let err = parse_metrics_clause(
            "s.running AS AVG(qty) OVER (PARTITION BY EXCLUDING r ORDER BY \"unclosed ASC)",
            0,
        )
        .unwrap_err();
        assert!(
            err.message.contains("Unterminated quoted identifier"),
            "Expected unterminated-quote error, got: {}",
            err.message
        );
    }

    #[test]
    fn test_parse_window_spec_regression_bare_order_by() {
        // Regression baseline: bare unquoted column ref. Mirrors phase48.
        let result = parse_metrics_clause(
            "s.running AS AVG(qty) OVER (PARTITION BY EXCLUDING r ORDER BY order_date ASC NULLS LAST)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let ws = result[0]
            .window_spec
            .as_ref()
            .expect("window_spec must be Some");
        assert_eq!(ws.order_by.len(), 1);
        let ob = &ws.order_by[0];
        assert_eq!(ob.expr, "order_date");
        assert_eq!(ob.order, SortOrder::Asc);
        assert_eq!(ob.nulls, NullsOrder::Last);
    }

    #[test]
    fn test_parse_non_additive_dims_regression_bare_no_whitespace() {
        // Regression baseline for the pre-existing happy path: bare unquoted
        // dim_name with ASC/DESC modifier. Mirrors phase47_semi_additive.test.
        let result = parse_metrics_clause(
            "a.balance NON ADDITIVE BY (report_date DESC) AS SUM(a.balance)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let na_vec = &result[0].non_additive_by;
        assert_eq!(na_vec.len(), 1);
        let na = &na_vec[0];
        assert_eq!(na.dimension, "report_date");
        assert_eq!(na.order, SortOrder::Desc);
        assert_eq!(na.nulls, NullsOrder::First); // DESC default
    }

    // -----------------------------------------------------------------------
    // parse_relationships_clause tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_relationships_empty_body() {
        let result = parse_relationships_clause("", 0).unwrap();
        assert_eq!(result.len(), 0, "Empty body must return empty vec");
    }

    #[test]
    fn parse_relationships_single_entry() {
        let result =
            parse_relationships_clause("order_to_customer AS o(customer_id) REFERENCES c", 0)
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name.as_deref(), Some("order_to_customer"));
        assert_eq!(result[0].from_alias, "o");
        assert_eq!(result[0].fk_columns, vec!["customer_id"]);
        assert_eq!(result[0].table, "c");
    }

    #[test]
    fn parse_relationships_composite_fk() {
        let result = parse_relationships_clause("rel AS o(fk1, fk2) REFERENCES c", 0).unwrap();
        assert_eq!(result[0].fk_columns, vec!["fk1", "fk2"]);
    }

    #[test]
    fn parse_relationships_quoted_paren_in_fk_column() {
        // PA-6 (PR #50 review): the close paren after the FK list was
        // located with a naive find(')'), so a quoted FK column containing
        // ')' truncated the list and mis-parsed the REFERENCES clause.
        let result =
            parse_relationships_clause("rel AS o(\"x)y\") REFERENCES c(\"a)b\")", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].fk_columns, vec!["\"x)y\""]);
        assert_eq!(result[0].table, "c");
        assert_eq!(result[0].ref_columns, vec!["\"a)b\""]);
    }

    #[test]
    fn parse_materializations_quoted_specials_do_not_split() {
        // PA-6 (PR #50 review): the sub-body paren scan and TABLE /
        // DIMENSIONS / METRICS keyword scan were not quote- or depth-aware —
        // a quoted name containing ')' closed the sub-body early, and
        // keyword text inside quotes or nested parens split it at the wrong
        // places.
        let result = parse_materializations_clause(
            "m1 AS (TABLE \"pre)agg\", DIMENSIONS (\"metrics\", region), METRICS (total))",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "m1");
        assert_eq!(result[0].table, "\"pre)agg\"");
        assert_eq!(result[0].dimensions, vec!["\"metrics\"", "region"]);
        assert_eq!(result[0].metrics, vec!["total"]);
    }

    #[test]
    fn parse_materializations_unquoted_keyword_name_nested_not_a_subkeyword() {
        // §6.1 phase 6: a *bare* dimension named like a sub-keyword (`metrics`)
        // sits at bracket-depth 1 inside DIMENSIONS (...), so the depth-0
        // TABLE/DIMENSIONS/METRICS scan must not treat it as the METRICS
        // sub-clause and split the sub-body there.
        let result =
            parse_materializations_clause("m1 AS (TABLE t, DIMENSIONS (metrics, region))", 0)
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].table, "t");
        assert_eq!(result[0].dimensions, vec!["metrics", "region"]);
        assert!(
            result[0].metrics.is_empty(),
            "the nested `metrics` name must not become a METRICS sub-clause"
        );
    }

    #[test]
    fn parse_materializations_quoted_name_with_space() {
        // §6.1 phase 6: the name is the first TOKEN, so a double-quoted name
        // containing whitespace stays whole (`"my mat"`) instead of splitting
        // at the interior space the way the old first-whitespace scan did.
        let result =
            parse_materializations_clause("\"my mat\" AS (TABLE t, DIMENSIONS (region))", 0)
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "\"my mat\"");
        assert_eq!(result[0].table, "t");
        assert_eq!(result[0].dimensions, vec!["region"]);
    }

    #[test]
    fn parse_materializations_leading_symbol_is_missing_name() {
        // §6.1 phase 6: an entry whose first token is punctuation has no name.
        let err = parse_materializations_clause("(TABLE t, DIMENSIONS (region))", 0).unwrap_err();
        assert!(
            err.message.contains("Expected materialization name"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_materializations_unterminated_name_rejected_cleanly() {
        // PR #105 review: an unterminated quoted-identifier name lexes as one
        // token spanning the whole entry; reject it up front (like every sibling
        // clause) instead of embedding the entry in an "Expected 'AS'" error.
        let err = parse_materializations_clause("\"unclosed AS (TABLE t, DIMENSIONS (r))", 0)
            .unwrap_err();
        assert!(
            err.message.contains("Unterminated quoted identifier"),
            "got: {}",
            err.message
        );
        // An unterminated string literal in the name slot is distinguished.
        let err =
            parse_materializations_clause("'unclosed AS (TABLE t, DIMENSIONS (r))", 0).unwrap_err();
        assert!(
            err.message.contains("Unterminated string literal"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_relationships_error_missing_name() {
        // Entry starts with "AS" — no preceding relationship name
        let result = parse_relationships_clause("AS o(customer_id) REFERENCES c", 0);
        assert!(
            result.is_err(),
            "Expected error for missing relationship name"
        );
        let err = result.unwrap_err();
        assert!(
            err.message.contains("name") || err.message.contains("required"),
            "Error should mention name or required: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // Phase 33: Cardinality keyword rejection + REFERENCES(cols) + UNIQUE tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_relationship_without_cardinality_defaults() {
        let result = parse_relationships_clause("rel AS a(fk) REFERENCES b", 0).unwrap();
        assert_eq!(result[0].table, "b");
        assert_eq!(
            result[0].cardinality,
            Cardinality::ManyToOne,
            "Cardinality should default to ManyToOne"
        );
    }

    #[test]
    fn old_cardinality_keywords_rejected() {
        // Phase 33: All cardinality keywords are rejected
        for input in [
            "rel AS a(fk) REFERENCES b MANY TO ONE",
            "rel AS a(fk) REFERENCES b ONE TO ONE",
            "rel AS a(fk) REFERENCES b ONE TO MANY",
        ] {
            let result = parse_relationships_clause(input, 0);
            assert!(
                result.is_err(),
                "Cardinality keyword should be rejected: {input}"
            );
            let err = result.unwrap_err();
            assert!(
                err.message.contains("no longer supported"),
                "Error should mention no longer supported for '{input}': {}",
                err.message
            );
        }
    }

    #[test]
    fn trailing_text_after_references_rejected() {
        let result = parse_relationships_clause("rel AS a(fk) REFERENCES b garbage", 0);
        assert!(result.is_err(), "Trailing text should be rejected");
        let err = result.unwrap_err();
        assert!(
            err.message.contains("Unexpected tokens")
                || err.message.contains("no longer supported"),
            "Error should mention unexpected tokens: {}",
            err.message
        );
    }

    #[test]
    fn references_with_column_list() {
        let result = parse_relationships_clause("rel AS a(fk) REFERENCES b(id)", 0).unwrap();
        assert_eq!(result[0].table, "b");
        assert_eq!(result[0].ref_columns, vec!["id"]);
    }

    #[test]
    fn references_without_column_list() {
        let result = parse_relationships_clause("rel AS a(fk) REFERENCES b", 0).unwrap();
        assert_eq!(result[0].table, "b");
        assert!(
            result[0].ref_columns.is_empty(),
            "ref_columns should be empty when no explicit column list"
        );
    }

    #[test]
    fn references_multi_column_list() {
        let result =
            parse_relationships_clause("rel AS a(fk1, fk2) REFERENCES b(col1, col2)", 0).unwrap();
        assert_eq!(result[0].ref_columns, vec!["col1", "col2"]);
    }

    #[test]
    fn references_target_no_space_before_paren() {
        // target(col) with no space between alias and paren
        let result = parse_relationships_clause("rel AS a(fk) REFERENCES b(id)", 0).unwrap();
        assert_eq!(result[0].table, "b");
        assert_eq!(result[0].ref_columns, vec!["id"]);
    }

    #[test]
    fn references_target_space_before_paren() {
        // target (col) with space between alias and paren
        let result = parse_relationships_clause("rel AS a(fk) REFERENCES b (id)", 0).unwrap();
        assert_eq!(result[0].table, "b");
        assert_eq!(result[0].ref_columns, vec!["id"]);
    }

    #[test]
    fn parse_relationships_quoted_paren_in_from_alias() {
        // §6.1 (phase 2, PR follow-up to P-11): the from-alias run stops at the
        // first `(` SYMBOL token, so a `(` inside a QUOTED from_alias is inert.
        // The pre-migration `after_as.find('(')` was not quote-aware and split
        // inside the quote, mis-parsing the entry.
        let result = parse_relationships_clause("rel AS \"a(b\"(fk) REFERENCES c", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].from_alias, "\"a(b\"");
        assert_eq!(result[0].fk_columns, vec!["fk"]);
        assert_eq!(result[0].table, "c");
    }

    #[test]
    fn parse_relationships_multi_token_from_alias_rejected() {
        // §6.1 (phase 2, PR #101 review): from_alias is a single value token
        // (like a TABLES alias), immediately followed by `(`. A multi-token run
        // (`a b(...)`, `a.b(...)`) is rejected at parse time rather than
        // captured as a bogus alias that only fails later at resolution.
        for entry in ["rel AS a b(fk) REFERENCES c", "rel AS a.b(fk) REFERENCES c"] {
            let err = parse_relationships_clause(entry, 0).unwrap_err();
            assert!(
                err.message.contains("Expected '(' after from_alias"),
                "{entry}: got {}",
                err.message
            );
        }
    }

    #[test]
    fn parse_relationships_junk_before_references_rejected() {
        // §6.1 (phase 2): text between the FK list and REFERENCES is rejected,
        // not silently skipped. The old code scanned for REFERENCES anywhere
        // after the FK `)` and dropped whatever preceded it (the P-1 class);
        // token-sequential parsing requires REFERENCES to follow immediately.
        let err = parse_relationships_clause("rel AS o(fk) junk REFERENCES c", 0).unwrap_err();
        assert!(
            err.message.contains("Expected 'REFERENCES'"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn unique_constraint_parsing() {
        let result = parse_tables_clause("o AS orders PRIMARY KEY (id) UNIQUE (email)", 0).unwrap();
        assert_eq!(result[0].unique_constraints.len(), 1);
        assert_eq!(result[0].unique_constraints[0], vec!["email"]);
    }

    #[test]
    fn multiple_unique_constraints() {
        let result = parse_tables_clause(
            "o AS orders PRIMARY KEY (id) UNIQUE (email) UNIQUE (first_name, last_name)",
            0,
        )
        .unwrap();
        assert_eq!(result[0].unique_constraints.len(), 2);
        assert_eq!(result[0].unique_constraints[0], vec!["email"]);
        assert_eq!(
            result[0].unique_constraints[1],
            vec!["first_name", "last_name"]
        );
    }

    #[test]
    fn table_without_primary_key() {
        let result = parse_tables_clause("f AS fact_table", 0).unwrap();
        assert_eq!(result[0].alias, "f");
        assert_eq!(result[0].table, "fact_table");
        assert!(result[0].pk_columns.is_empty());
        assert!(result[0].unique_constraints.is_empty());
    }

    #[test]
    fn table_with_unique_no_pk() {
        let result = parse_tables_clause("f AS fact_table UNIQUE (email)", 0).unwrap();
        assert_eq!(result[0].alias, "f");
        assert_eq!(result[0].table, "fact_table");
        assert!(result[0].pk_columns.is_empty());
        assert_eq!(result[0].unique_constraints.len(), 1);
        assert_eq!(result[0].unique_constraints[0], vec!["email"]);
    }

    // -----------------------------------------------------------------------
    // parse_qualified_entries tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_qualified_entries_simple() {
        let result =
            parse_qualified_entries("o.revenue AS SUM(amount)", 0, false, "dimensions").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, "o"); // source_alias
        assert_eq!(result[0].name, "revenue"); // bare_name
        assert_eq!(result[0].expr, "SUM(amount)"); // expr
    }

    #[test]
    fn parse_qualified_entries_nested_parens() {
        let result = parse_qualified_entries(
            "o.disc_price AS SUM(l_extendedprice * (1 - l_discount))",
            0,
            false,
            "dimensions",
        )
        .unwrap();
        assert_eq!(result[0].expr, "SUM(l_extendedprice * (1 - l_discount))");
    }

    #[test]
    fn parse_qualified_entries_trailing_comma() {
        let result =
            parse_qualified_entries("o.revenue AS SUM(amount),", 0, false, "dimensions").unwrap();
        assert_eq!(
            result.len(),
            1,
            "Trailing comma must not produce extra entry"
        );
    }

    #[test]
    fn parse_qualified_entries_multiple_with_trailing_comma() {
        let result =
            parse_qualified_entries("o.a AS x, o.b AS y,", 0, false, "dimensions").unwrap();
        assert_eq!(result.len(), 2, "Expected 2 entries, got {:?}", result);
        assert_eq!(result[0].name, "a");
        assert_eq!(result[1].name, "b");
    }

    #[test]
    fn parse_qualified_entries_error_missing_dot() {
        let result = parse_qualified_entries("revenue AS SUM(amount)", 0, false, "dimensions");
        assert!(result.is_err(), "Expected error for missing alias prefix");
        let err = result.unwrap_err();
        assert!(
            err.message.contains("alias") || err.message.contains("qualified"),
            "Error should mention alias or qualified: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // parse_keyword_body end-to-end tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_keyword_body_basic() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables.len(), 1);
        assert_eq!(kb.relationships.len(), 0);
        assert_eq!(kb.dimensions.len(), 1);
        assert_eq!(kb.metrics.len(), 1);
        assert_eq!(kb.tables[0].alias, "o");
        assert_eq!(kb.dimensions[0].name, "region");
        assert_eq!(kb.dimensions[0].source_table.as_deref(), Some("o"));
        assert_eq!(kb.metrics[0].name, "rev");
        assert_eq!(kb.metrics[0].expr, "SUM(amount)");
    }

    /// T-8 (code-review 2026-07-11): a large *valid* body parses correctly and
    /// without arbitrary entry limits. 500 METRICS entries exercise the
    /// depth-0 comma splitter and per-entry scans at scale; the test also pins
    /// linearity — an accidentally-quadratic clause scan would blow this up
    /// into a visible timeout rather than passing in milliseconds.
    #[test]
    fn parse_keyword_body_large_metrics_clause() {
        const N: usize = 500;
        let mut entries = Vec::with_capacity(N);
        for i in 0..N {
            entries.push(format!("o.m{i} AS SUM(amount)"));
        }
        let body = format!(
            "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS region) METRICS ({})",
            entries.join(", ")
        );
        let kb = parse_keyword_body(&body, 0).unwrap();
        assert_eq!(kb.metrics.len(), N);
        // Spot-check the boundaries so a silent off-by-one in the splitter
        // would surface.
        assert_eq!(kb.metrics[0].name, "m0");
        assert_eq!(kb.metrics[N - 1].name, format!("m{}", N - 1));
        assert!(kb.metrics.iter().all(|m| m.expr == "SUM(amount)"));
    }

    /// T-8: a deeply-nested but balanced parenthesized metric expression parses
    /// (the depth counter is a plain `i32`, so 64 levels is far within range)
    /// and is stored verbatim as an uninterpreted expression.
    #[test]
    fn parse_keyword_body_deeply_nested_parens() {
        const DEPTH: usize = 64;
        let expr = format!("SUM({}amount{})", "(".repeat(DEPTH), ")".repeat(DEPTH));
        let body = format!("AS TABLES (o AS orders PRIMARY KEY (id)) METRICS (o.deep AS {expr})");
        let kb = parse_keyword_body(&body, 0).unwrap();
        assert_eq!(kb.metrics.len(), 1);
        assert_eq!(kb.metrics[0].name, "deep");
        assert_eq!(kb.metrics[0].expr, expr);
    }

    #[test]
    fn parse_keyword_body_with_relationships() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id), c AS customers PRIMARY KEY (id)) RELATIONSHIPS (o_to_c AS o(cust_id) REFERENCES c) DIMENSIONS (o.reg AS region) METRICS (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.relationships.len(), 1);
        assert_eq!(kb.relationships[0].name.as_deref(), Some("o_to_c"));
        assert_eq!(kb.relationships[0].from_alias, "o");
        assert_eq!(kb.relationships[0].fk_columns, vec!["cust_id"]);
        assert_eq!(kb.relationships[0].table, "c");
    }

    #[test]
    fn parse_keyword_body_empty_relationships() {
        let body =
            "AS TABLES (o AS orders PRIMARY KEY (id)) RELATIONSHIPS () DIMENSIONS (o.x AS x)";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(
            kb.relationships.len(),
            0,
            "Empty RELATIONSHIPS must be empty vec"
        );
    }

    #[test]
    fn parse_keyword_body_error_missing_tables() {
        let body = "AS DIMENSIONS (o.x AS x)";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err(), "Expected error for missing TABLES clause");
    }

    // -----------------------------------------------------------------------
    // Whitespace tolerance tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_keyword_body_newlines_between_clauses() {
        let body = "AS\nTABLES (\n  o AS orders PRIMARY KEY (id)\n)\nDIMENSIONS (\n  o.region AS region\n)\nMETRICS (\n  o.rev AS SUM(amount)\n)";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables.len(), 1);
        assert_eq!(kb.dimensions.len(), 1);
        assert_eq!(kb.metrics.len(), 1);
    }

    #[test]
    fn parse_keyword_body_tabs_between_tokens() {
        let body = "AS\tTABLES\t(\to\tAS\torders\tPRIMARY\tKEY\t(id)\t)\tDIMENSIONS\t(\to.region\tAS\tregion\t)\tMETRICS\t(\to.rev\tAS\tSUM(amount)\t)";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables.len(), 1);
        assert_eq!(kb.dimensions[0].name, "region");
    }

    #[test]
    fn parse_keyword_body_extra_spaces() {
        let body = "AS  TABLES  (  o  AS  orders  PRIMARY  KEY  (  id  )  )  DIMENSIONS  (  o.region  AS  region  )  METRICS  (  o.rev  AS  SUM(amount)  )";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables[0].alias, "o");
        assert_eq!(kb.tables[0].table, "orders");
        assert_eq!(kb.tables[0].pk_columns, vec!["id"]);
    }

    #[test]
    fn parse_keyword_body_mixed_whitespace_multientry() {
        // Multiple entries with newline+indent separation
        let body = "AS TABLES (\n    o AS orders PRIMARY KEY (o_id),\n    c AS customers PRIMARY KEY (c_id)\n) DIMENSIONS (\n    o.region AS region,\n    c.name AS customer_name\n) METRICS (\n    o.rev AS SUM(amount)\n)";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables.len(), 2);
        assert_eq!(kb.dimensions.len(), 2);
    }

    #[test]
    fn parse_tables_extra_whitespace_around_tokens() {
        // Extra whitespace inside the clause body
        let result = parse_tables_clause(
            "  o   AS   main.orders   PRIMARY   KEY   ( o_id ,  o_seq )  ",
            0,
        )
        .unwrap();
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "main.orders");
        assert_eq!(result[0].pk_columns, vec!["o_id", "o_seq"]);
    }

    #[test]
    fn parse_relationships_newline_separated() {
        let body = "\n  order_to_customer AS o(customer_id) REFERENCES c\n";
        let result = parse_relationships_clause(body, 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name.as_deref(), Some("order_to_customer"));
    }

    #[test]
    fn parse_qualified_entries_newline_separated() {
        let body = "\n  o.revenue AS SUM(amount),\n  o.count AS COUNT(*)\n";
        let result = parse_qualified_entries(body, 0, false, "dimensions").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "revenue");
        assert_eq!(result[1].name, "count");
    }

    // -----------------------------------------------------------------------
    // Case insensitivity tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_keyword_body_lowercase_clause_keywords() {
        let body = "as tables (o AS orders primary key (id)) dimensions (o.region AS region) metrics (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables.len(), 1);
        assert_eq!(kb.dimensions.len(), 1);
        assert_eq!(kb.metrics.len(), 1);
    }

    #[test]
    fn parse_keyword_body_mixedcase_clause_keywords() {
        let body = "As Tables (o AS orders Primary Key (id)) Dimensions (o.region AS region) Metrics (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables.len(), 1);
        assert_eq!(kb.dimensions[0].name, "region");
    }

    #[test]
    fn parse_tables_lowercase_as_and_primary_key() {
        let result = parse_tables_clause("o as orders primary key (o_id)", 0).unwrap();
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "orders");
        assert_eq!(result[0].pk_columns, vec!["o_id"]);
    }

    #[test]
    fn parse_tables_primary_key_with_newline() {
        // PRIMARY\nKEY — newline between the two words
        let result = parse_tables_clause("o AS orders PRIMARY\nKEY (o_id)", 0).unwrap();
        assert_eq!(result[0].pk_columns, vec!["o_id"]);
    }

    #[test]
    fn parse_tables_primary_key_with_tab() {
        let result = parse_tables_clause("o AS orders PRIMARY\tKEY (o_id)", 0).unwrap();
        assert_eq!(result[0].pk_columns, vec!["o_id"]);
    }

    #[test]
    fn parse_relationships_lowercase_as_and_references() {
        let result =
            parse_relationships_clause("ord_to_cust as o(cust_id) references c", 0).unwrap();
        assert_eq!(result[0].name.as_deref(), Some("ord_to_cust"));
        assert_eq!(result[0].from_alias, "o");
        assert_eq!(result[0].table, "c");
    }

    #[test]
    fn parse_qualified_entries_lowercase_as() {
        let result =
            parse_qualified_entries("o.revenue as SUM(amount)", 0, false, "dimensions").unwrap();
        assert_eq!(result[0].name, "revenue");
        assert_eq!(result[0].expr, "SUM(amount)");
    }

    #[test]
    fn parse_keyword_body_all_lowercase_full_round_trip() {
        // Fully lowercase: every keyword, operator, and token in lowercase
        let body = "as\ntables (\n    o as main.orders primary\nkey (o_id)\n)\nrelationships (\n    ord_to_cust as o(o_cust_id) references c\n)\ndimensions (\n    o.region as region\n)\nmetrics (\n    o.revenue as sum(amount)\n)";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables.len(), 1);
        assert_eq!(kb.tables[0].table, "main.orders");
        assert_eq!(kb.relationships.len(), 1);
        assert_eq!(kb.relationships[0].name.as_deref(), Some("ord_to_cust"));
        assert_eq!(kb.dimensions[0].name, "region");
        assert_eq!(kb.metrics[0].expr, "sum(amount)");
    }

    // -----------------------------------------------------------------------
    // FACTS clause tests (Phase 29)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_keyword_body_with_facts_single() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) FACTS (o.net_price AS o.price * (1 - o.discount)) DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.facts.len(), 1);
        assert_eq!(kb.facts[0].name, "net_price");
        assert_eq!(kb.facts[0].expr, "o.price * (1 - o.discount)");
        assert_eq!(kb.facts[0].source_table.as_deref(), Some("o"));
    }

    #[test]
    fn parse_keyword_body_with_facts_multiple() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) FACTS (o.net_price AS o.price * (1 - o.discount), o.tax_amount AS o.price * o.tax_rate) DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.facts.len(), 2);
        assert_eq!(kb.facts[0].name, "net_price");
        assert_eq!(kb.facts[1].name, "tax_amount");
    }

    #[test]
    fn parse_keyword_body_with_facts_trailing_comma() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) FACTS (o.net_price AS o.price * (1 - o.discount),) DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(
            kb.facts.len(),
            1,
            "Trailing comma must not produce extra entry"
        );
    }

    #[test]
    fn parse_keyword_body_with_empty_facts() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) FACTS () DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert!(
            kb.facts.is_empty(),
            "Empty FACTS clause must produce empty vec"
        );
    }

    #[test]
    fn parse_keyword_body_without_facts_still_works() {
        // Backward compat: DDL without FACTS clause must still work
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert!(kb.facts.is_empty());
    }

    #[test]
    fn parse_keyword_body_fact_without_source_table_rejected() {
        // Facts reuse parse_qualified_entries which requires alias.name format
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) FACTS (net_price AS price * discount) DIMENSIONS (o.region AS region) METRICS (o.rev AS SUM(amount))";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_err(),
            "Fact without source table prefix must be rejected"
        );
    }

    #[test]
    fn parse_keyword_body_facts_after_dimensions_rejected() {
        // FACTS must come before DIMENSIONS (order: tables, relationships, facts, dimensions, metrics)
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS region) FACTS (o.net_price AS price) METRICS (o.rev AS SUM(amount))";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_err(),
            "FACTS after DIMENSIONS must be rejected (wrong order)"
        );
        let err = result.unwrap_err();
        assert!(
            err.message.contains("out of order"),
            "Error should mention out of order: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // parse_metrics_clause tests (Phase 30 -- derived metrics)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_metrics_clause_qualified_entry() {
        let result = parse_metrics_clause("li.revenue AS SUM(li.amount)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, Some("li".to_string())); // source alias
        assert_eq!(result[0].name, "revenue"); // bare_name
        assert_eq!(result[0].expr, "SUM(li.amount)"); // expr
    }

    #[test]
    fn parse_metrics_clause_unqualified_entry() {
        let result = parse_metrics_clause("profit AS revenue - cost", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, None); // no source alias (derived metric)
        assert_eq!(result[0].name, "profit"); // bare_name
        assert_eq!(result[0].expr, "revenue - cost"); // expr
    }

    #[test]
    fn parse_metrics_clause_mixed_entries() {
        let result = parse_metrics_clause(
            "li.revenue AS SUM(li.amount), profit AS revenue - cost, li.cost AS SUM(li.unit_cost)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 3);
        // First: qualified
        assert_eq!(result[0].source_alias, Some("li".to_string()));
        assert_eq!(result[0].name, "revenue");
        assert_eq!(result[0].expr, "SUM(li.amount)");
        // Second: unqualified (derived)
        assert_eq!(result[1].source_alias, None);
        assert_eq!(result[1].name, "profit");
        assert_eq!(result[1].expr, "revenue - cost");
        // Third: qualified
        assert_eq!(result[2].source_alias, Some("li".to_string()));
        assert_eq!(result[2].name, "cost");
        assert_eq!(result[2].expr, "SUM(li.unit_cost)");
    }

    #[test]
    fn parse_metrics_clause_trailing_comma() {
        let result = parse_metrics_clause("profit AS revenue - cost,", 0).unwrap();
        assert_eq!(
            result.len(),
            1,
            "Trailing comma must not produce extra entry"
        );
    }

    #[test]
    fn parse_metrics_clause_newline_separated() {
        let result = parse_metrics_clause(
            "\n  li.revenue AS SUM(li.amount),\n  profit AS revenue - cost\n",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].source_alias, Some("li".to_string()));
        assert_eq!(result[1].source_alias, None);
        assert_eq!(result[1].name, "profit");
    }

    #[test]
    fn parse_keyword_body_with_derived_metrics() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS region) METRICS (o.revenue AS SUM(o.amount), profit AS revenue - cost)";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics.len(), 2);
        // First: qualified metric -> source_table: Some("o")
        assert_eq!(kb.metrics[0].name, "revenue");
        assert_eq!(kb.metrics[0].source_table.as_deref(), Some("o"));
        assert_eq!(kb.metrics[0].expr, "SUM(o.amount)");
        // Second: derived metric -> source_table: None
        assert_eq!(kb.metrics[1].name, "profit");
        assert!(kb.metrics[1].source_table.is_none());
        assert_eq!(kb.metrics[1].expr, "revenue - cost");
    }

    #[test]
    fn parse_keyword_body_only_derived_metrics() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) METRICS (profit AS revenue - cost, margin AS profit / revenue)";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics.len(), 2);
        assert!(kb.metrics[0].source_table.is_none());
        assert!(kb.metrics[1].source_table.is_none());
        assert_eq!(kb.metrics[0].name, "profit");
        assert_eq!(kb.metrics[1].name, "margin");
    }

    #[test]
    fn parse_qualified_entries_still_rejects_unqualified() {
        // FACTS and DIMENSIONS still use parse_qualified_entries which requires alias.name
        let result = parse_qualified_entries("revenue AS SUM(amount)", 0, false, "dimensions");
        assert!(
            result.is_err(),
            "parse_qualified_entries must still reject unqualified entries (missing dot)"
        );
        let err = result.unwrap_err();
        assert!(
            err.message.contains("alias") || err.message.contains("qualified"),
            "Error should mention alias or qualified: {}",
            err.message
        );
    }

    #[test]
    fn parse_metrics_clause_empty_body() {
        let result = parse_metrics_clause("", 0).unwrap();
        assert_eq!(result.len(), 0, "Empty body must return empty vec");
    }

    // -----------------------------------------------------------------------
    // parse_metrics_clause USING tests (Phase 32 -- role-playing dimensions)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_metrics_using_single_relationship() {
        let result =
            parse_metrics_clause("f.departure_count USING (dep_airport) AS COUNT(*)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, Some("f".to_string())); // source alias
        assert_eq!(result[0].name, "departure_count"); // bare_name
        assert_eq!(result[0].expr, "COUNT(*)"); // expr
        assert_eq!(result[0].using_relationships, vec!["dep_airport"]); // using_relationships
    }

    #[test]
    fn parse_metrics_using_multiple_relationships() {
        let result = parse_metrics_clause("f.met USING (rel1, rel2) AS SUM(x)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, Some("f".to_string()));
        assert_eq!(result[0].name, "met");
        assert_eq!(result[0].expr, "SUM(x)");
        assert_eq!(result[0].using_relationships, vec!["rel1", "rel2"]);
    }

    #[test]
    fn parse_metrics_using_on_derived_produces_error() {
        // Derived metric (no dot prefix) with USING -> ParseError
        let result = parse_metrics_clause("derived_met USING (rel1) AS revenue - cost", 0);
        assert!(
            result.is_err(),
            "USING on derived metric must produce error"
        );
        let err = result.unwrap_err();
        assert!(
            err.message.contains("USING") && err.message.contains("derived"),
            "Error should mention USING and derived: {}",
            err.message
        );
    }

    #[test]
    fn parse_metrics_without_using_backward_compat() {
        // Metric without USING still parses correctly
        let result = parse_metrics_clause("o.revenue AS SUM(o.amount)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, Some("o".to_string()));
        assert_eq!(result[0].name, "revenue");
        assert_eq!(result[0].expr, "SUM(o.amount)");
        assert!(
            result[0].using_relationships.is_empty(),
            "No USING -> empty relationships"
        );
    }

    #[test]
    fn parse_metrics_using_case_insensitive() {
        let result =
            parse_metrics_clause("f.departure_count using (dep_airport) AS COUNT(*)", 0).unwrap();
        assert_eq!(result[0].using_relationships, vec!["dep_airport"]);

        let result2 =
            parse_metrics_clause("f.departure_count UsInG (dep_airport) AS COUNT(*)", 0).unwrap();
        assert_eq!(result2[0].using_relationships, vec!["dep_airport"]);
    }

    #[test]
    fn parse_keyword_body_with_using_metrics() {
        let body = "AS TABLES (f AS flights PRIMARY KEY (id), a AS airports PRIMARY KEY (id)) RELATIONSHIPS (dep_airport AS f(dep_id) REFERENCES a, arr_airport AS f(arr_id) REFERENCES a) DIMENSIONS (a.name AS airport_name) METRICS (f.departure_count USING (dep_airport) AS COUNT(*))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics.len(), 1);
        assert_eq!(kb.metrics[0].name, "departure_count");
        assert_eq!(kb.metrics[0].using_relationships, vec!["dep_airport"]);
    }

    // -----------------------------------------------------------------------
    // Phase 43: COMMENT, SYNONYMS, PRIVATE/PUBLIC annotation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_dimension_with_comment() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.date AS o.created_at COMMENT = 'Order date') METRICS (o.rev AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.dimensions[0].comment.as_deref(), Some("Order date"));
    }

    #[test]
    fn test_dimension_with_synonyms() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.date AS o.created_at WITH SYNONYMS = ('purchase_date', 'order_date')) METRICS (o.rev AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(
            kb.dimensions[0].synonyms,
            vec!["purchase_date", "order_date"]
        );
    }

    #[test]
    fn test_comment_and_synonyms_either_order() {
        // COMMENT then SYNONYMS
        let body1 = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.date AS o.created_at COMMENT = 'test' WITH SYNONYMS = ('a')) METRICS (o.rev AS SUM(o.amount))";
        let kb1 = parse_keyword_body(body1, 0).unwrap();
        assert_eq!(kb1.dimensions[0].comment.as_deref(), Some("test"));
        assert_eq!(kb1.dimensions[0].synonyms, vec!["a"]);

        // SYNONYMS then COMMENT
        let body2 = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.date AS o.created_at WITH SYNONYMS = ('a') COMMENT = 'test') METRICS (o.rev AS SUM(o.amount))";
        let kb2 = parse_keyword_body(body2, 0).unwrap();
        assert_eq!(kb2.dimensions[0].comment.as_deref(), Some("test"));
        assert_eq!(kb2.dimensions[0].synonyms, vec!["a"]);
    }

    #[test]
    fn test_private_metric() {
        let body =
            "AS TABLES (o AS orders PRIMARY KEY (id)) METRICS (PRIVATE o.cost AS SUM(o.cost))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics[0].access, AccessModifier::Private);
    }

    #[test]
    fn test_explicit_public_metric() {
        let body =
            "AS TABLES (o AS orders PRIMARY KEY (id)) METRICS (PUBLIC o.revenue AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics[0].access, AccessModifier::Public);
    }

    #[test]
    fn test_default_public_metric() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) METRICS (o.revenue AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics[0].access, AccessModifier::Public);
    }

    #[test]
    fn test_private_fact() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) FACTS (PRIVATE o.raw_cost AS o.cost) DIMENSIONS (o.region AS o.region) METRICS (o.rev AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.facts[0].access, AccessModifier::Private);
    }

    #[test]
    fn test_private_dimension_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (PRIVATE o.date AS o.created_at) METRICS (o.rev AS SUM(o.amount))";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err(), "PRIVATE on dimension must produce error");
        let err = result.unwrap_err();
        assert!(
            err.message.contains("PRIVATE") || err.message.contains("not supported"),
            "Error should mention PRIVATE or not supported: {}",
            err.message
        );
        assert!(
            err.message.to_lowercase().contains("dimension"),
            "Error should mention dimension: {}",
            err.message
        );
    }

    #[test]
    fn test_escaped_quotes_in_comment() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.date AS o.created_at COMMENT = 'It''s a test') METRICS (o.rev AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.dimensions[0].comment.as_deref(), Some("It's a test"));
    }

    #[test]
    fn test_comment_identifier_not_confused() {
        // Expression contains "comment_count" as an identifier -- must NOT trigger COMMENT annotation
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.flag AS CASE WHEN comment_count > 0 THEN 1 ELSE 0 END) METRICS (o.rev AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert!(
            kb.dimensions[0].comment.is_none(),
            "comment_count in expr must not trigger COMMENT annotation"
        );
        assert_eq!(
            kb.dimensions[0].expr,
            "CASE WHEN comment_count > 0 THEN 1 ELSE 0 END"
        );
    }

    #[test]
    fn test_table_with_comment_and_synonyms() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id) COMMENT = 'Orders table' WITH SYNONYMS = ('sales')) DIMENSIONS (o.region AS o.region) METRICS (o.rev AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.tables[0].comment.as_deref(), Some("Orders table"));
        assert_eq!(kb.tables[0].synonyms, vec!["sales"]);
    }

    #[test]
    fn test_metric_with_using_and_comment() {
        let body = "AS TABLES (f AS flights PRIMARY KEY (id), a AS airports PRIMARY KEY (id)) RELATIONSHIPS (dep AS f(dep_id) REFERENCES a) DIMENSIONS (a.name AS a.name) METRICS (f.dep_count USING (dep) AS COUNT(*) COMMENT = 'Departures')";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics[0].using_relationships, vec!["dep"]);
        assert_eq!(kb.metrics[0].comment.as_deref(), Some("Departures"));
    }

    #[test]
    fn test_private_keyword_disambiguation() {
        // Entry starting with table alias "private_schema.metric_name" should NOT be treated as PRIVATE
        // because "private_schema" is followed by "."
        let body = "AS TABLES (private_schema AS my_table PRIMARY KEY (id)) METRICS (private_schema.metric_name AS SUM(private_schema.value))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics[0].access, AccessModifier::Public);
        assert_eq!(
            kb.metrics[0].source_table.as_deref(),
            Some("private_schema")
        );
        assert_eq!(kb.metrics[0].name, "metric_name");
    }

    #[test]
    fn test_full_keyword_body_with_all_metadata() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id) COMMENT = 'Order table') FACTS (o.net_price AS o.amount * 0.9 COMMENT = 'Net price' WITH SYNONYMS = ('discounted_price')) DIMENSIONS (o.region AS o.region WITH SYNONYMS = ('territory')) METRICS (o.revenue AS SUM(o.amount) COMMENT = 'Total revenue', PRIVATE o.cost AS SUM(o.cost))";
        let kb = parse_keyword_body(body, 0).unwrap();

        assert_eq!(kb.tables[0].comment.as_deref(), Some("Order table"));
        assert_eq!(kb.facts[0].comment.as_deref(), Some("Net price"));
        assert_eq!(kb.facts[0].synonyms, vec!["discounted_price"]);
        assert_eq!(kb.dimensions[0].synonyms, vec!["territory"]);
        assert_eq!(kb.metrics[0].comment.as_deref(), Some("Total revenue"));
        assert_eq!(kb.metrics[1].access, AccessModifier::Private);
    }

    // -----------------------------------------------------------------------
    // Phase 47: NON ADDITIVE BY tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_metrics_non_additive_by_single_dim_defaults() {
        let result = parse_metrics_clause("a.bal NON ADDITIVE BY (d1) AS SUM(x)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, Some("a".to_string())); // source alias
        assert_eq!(result[0].name, "bal"); // bare name
        assert_eq!(result[0].expr, "SUM(x)"); // expr
        assert_eq!(result[0].non_additive_by.len(), 1);
        assert_eq!(result[0].non_additive_by[0].dimension, "d1");
        assert_eq!(result[0].non_additive_by[0].order, SortOrder::Asc);
        assert_eq!(result[0].non_additive_by[0].nulls, NullsOrder::Last);
    }

    #[test]
    fn parse_metrics_non_additive_by_desc_nulls_first() {
        let result = parse_metrics_clause(
            "a.balance NON ADDITIVE BY (date_dim DESC) AS SUM(a.balance)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].non_additive_by.len(), 1);
        assert_eq!(result[0].non_additive_by[0].dimension, "date_dim");
        assert_eq!(result[0].non_additive_by[0].order, SortOrder::Desc);
        // DESC defaults to NULLS FIRST
        assert_eq!(result[0].non_additive_by[0].nulls, NullsOrder::First);
    }

    #[test]
    fn parse_metrics_non_additive_by_multiple_dims() {
        let result = parse_metrics_clause(
            "a.bal NON ADDITIVE BY (d1 DESC NULLS FIRST, d2 ASC NULLS LAST) AS SUM(x)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].non_additive_by.len(), 2);
        assert_eq!(result[0].non_additive_by[0].dimension, "d1");
        assert_eq!(result[0].non_additive_by[0].order, SortOrder::Desc);
        assert_eq!(result[0].non_additive_by[0].nulls, NullsOrder::First);
        assert_eq!(result[0].non_additive_by[1].dimension, "d2");
        assert_eq!(result[0].non_additive_by[1].order, SortOrder::Asc);
        assert_eq!(result[0].non_additive_by[1].nulls, NullsOrder::Last);
    }

    #[test]
    fn parse_metrics_non_additive_by_on_derived_produces_error() {
        let result = parse_metrics_clause("profit NON ADDITIVE BY (d1) AS revenue - cost", 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("NON ADDITIVE BY"),
            "Error should mention NON ADDITIVE BY: {}",
            err.message
        );
        assert!(
            err.message.contains("derived"),
            "Error should mention derived: {}",
            err.message
        );
    }

    #[test]
    fn parse_metrics_non_additive_by_with_using() {
        let result =
            parse_metrics_clause("a.bal USING (rel1) NON ADDITIVE BY (d1 DESC) AS SUM(x)", 0)
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, Some("a".to_string()));
        assert_eq!(result[0].name, "bal");
        assert_eq!(result[0].using_relationships, vec!["rel1"]); // using_relationships
        assert_eq!(result[0].non_additive_by.len(), 1);
        assert_eq!(result[0].non_additive_by[0].dimension, "d1");
        assert_eq!(result[0].non_additive_by[0].order, SortOrder::Desc);
    }

    #[test]
    fn parse_keyword_body_non_additive_by_integration() {
        let body = "AS TABLES (a AS accounts PRIMARY KEY (id)) \
                     DIMENSIONS (a.date_dim AS a.date) \
                     METRICS (a.balance NON ADDITIVE BY (date_dim DESC) AS SUM(a.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics.len(), 1);
        assert_eq!(kb.metrics[0].name, "balance");
        assert_eq!(kb.metrics[0].non_additive_by.len(), 1);
        assert_eq!(kb.metrics[0].non_additive_by[0].dimension, "date_dim");
        assert_eq!(kb.metrics[0].non_additive_by[0].order, SortOrder::Desc);
    }

    #[test]
    fn parse_keyword_body_non_additive_by_invalid_dim_error() {
        let body = "AS TABLES (a AS accounts PRIMARY KEY (id)) \
                     DIMENSIONS (a.date_dim AS a.date) \
                     METRICS (a.balance NON ADDITIVE BY (nonexistent DESC) AS SUM(a.amount))";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message
                .contains("does not match any declared dimension"),
            "Error should mention dimension mismatch: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_non_additive_by_valid_dim_success() {
        let body = "AS TABLES (a AS accounts PRIMARY KEY (id)) \
                     DIMENSIONS (a.date_dim AS a.date, a.account AS a.account_id) \
                     METRICS (a.balance NON ADDITIVE BY (date_dim DESC, account) AS SUM(a.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics[0].non_additive_by.len(), 2);
        assert_eq!(kb.metrics[0].non_additive_by[0].dimension, "date_dim");
        assert_eq!(kb.metrics[0].non_additive_by[1].dimension, "account");
    }

    #[test]
    fn parse_keyword_body_non_additive_by_case_insensitive_dim_match() {
        let body = "AS TABLES (a AS accounts PRIMARY KEY (id)) \
                     DIMENSIONS (a.Date_Dim AS a.date) \
                     METRICS (a.balance NON ADDITIVE BY (date_dim DESC) AS SUM(a.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics[0].non_additive_by.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Phase 48: Window function OVER clause tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_metrics_window_over_basic() {
        let result = parse_metrics_clause(
            "o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING d1, d2 ORDER BY d1)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_alias, Some("o".to_string())); // source alias
        assert_eq!(result[0].name, "avg_qty"); // bare name
        let ws = result[0]
            .window_spec
            .as_ref()
            .expect("window_spec should be Some");
        assert_eq!(ws.window_function, "AVG");
        assert_eq!(ws.inner_metric, "total_qty");
        assert!(ws.extra_args.is_empty());
        assert_eq!(ws.excluding_dims, vec!["d1", "d2"]);
        assert_eq!(ws.order_by.len(), 1);
        assert_eq!(ws.order_by[0].expr, "d1");
        assert_eq!(ws.order_by[0].order, SortOrder::Asc);
        assert_eq!(ws.order_by[0].nulls, NullsOrder::Last);
        assert!(ws.frame_clause.is_none());
    }

    #[test]
    fn parse_metrics_window_over_with_frame_clause() {
        let result = parse_metrics_clause(
            "o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING d1 ORDER BY d1 RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let ws = result[0]
            .window_spec
            .as_ref()
            .expect("window_spec should be Some");
        assert_eq!(ws.window_function, "AVG");
        assert_eq!(ws.inner_metric, "total_qty");
        assert_eq!(ws.excluding_dims, vec!["d1"]);
        assert_eq!(ws.order_by.len(), 1);
        assert_eq!(ws.order_by[0].expr, "d1");
        assert_eq!(
            ws.frame_clause.as_deref(),
            Some("RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW")
        );
    }

    #[test]
    fn parse_metrics_window_lag_with_extra_args() {
        let result = parse_metrics_clause(
            "o.prev_qty AS LAG(total_qty, 30) OVER (PARTITION BY EXCLUDING d1 ORDER BY d1)",
            0,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let ws = result[0]
            .window_spec
            .as_ref()
            .expect("window_spec should be Some");
        assert_eq!(ws.window_function, "LAG");
        assert_eq!(ws.inner_metric, "total_qty");
        assert_eq!(ws.extra_args, vec!["30"]);
        assert_eq!(ws.excluding_dims, vec!["d1"]);
    }

    #[test]
    fn parse_metrics_window_over_on_derived_produces_error() {
        let result = parse_metrics_clause(
            "avg_ratio AS AVG(total_qty) OVER (PARTITION BY EXCLUDING d1)",
            0,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message
                .contains("OVER clause not allowed on derived metric"),
            "Error should mention OVER on derived: {}",
            err.message
        );
    }

    #[test]
    fn parse_metrics_window_over_with_non_additive_by_produces_error() {
        let result = parse_metrics_clause(
            "o.met NON ADDITIVE BY (d1) AS AVG(total_qty) OVER (PARTITION BY EXCLUDING d2)",
            0,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message
                .contains("Cannot combine OVER clause with NON ADDITIVE BY"),
            "Error should mention mutual exclusion: {}",
            err.message
        );
    }

    #[test]
    fn parse_metrics_window_over_no_over_returns_none() {
        let result = parse_metrics_clause("o.revenue AS SUM(o.amount)", 0).unwrap();
        assert_eq!(result.len(), 1);
        assert!(
            result[0].window_spec.is_none(),
            "Regular metric should have no window_spec"
        );
    }

    #[test]
    fn parse_metrics_window_over_order_by_desc_nulls() {
        let result = parse_metrics_clause(
            "o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING d1 ORDER BY d1 DESC NULLS LAST)",
            0,
        )
        .unwrap();
        let ws = result[0].window_spec.as_ref().unwrap();
        assert_eq!(ws.order_by[0].order, SortOrder::Desc);
        assert_eq!(ws.order_by[0].nulls, NullsOrder::Last);
    }

    #[test]
    fn parse_keyword_body_window_metric_integration() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region, o.month AS date_trunc('month', o.created_at)) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY month)\
                     )";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics.len(), 2);
        assert!(kb.metrics[0].window_spec.is_none());
        let ws = kb.metrics[1]
            .window_spec
            .as_ref()
            .expect("second metric should have window_spec");
        assert_eq!(ws.window_function, "AVG");
        assert_eq!(ws.inner_metric, "total_qty");
        assert_eq!(ws.excluding_dims, vec!["region"]);
        assert_eq!(ws.order_by[0].expr, "month");
    }

    #[test]
    fn parse_keyword_body_window_excluding_invalid_dim_error() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING nonexistent ORDER BY region)\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("EXCLUDING dimension"),
            "Error should mention EXCLUDING dimension: {}",
            err.message
        );
        assert!(
            err.message.contains("nonexistent"),
            "Error should mention the invalid dim: {}",
            err.message
        );
    }

    // ---------------------------------------------------------------------
    // EXP-12 (code-review 2026-08-03): a QUOTED reference in a window metric's
    // inner-metric or ORDER BY slot names the same identifier as its unquoted
    // declaration, per DuckDB's rule (CLAUDE.md: identifier matching follows
    // `ident::ident_matches`, not raw `eq_ignore_ascii_case`, which compares
    // the quote characters as data).
    //
    // These two sites, the fan-trap fence's `metric_grain`, and
    // `per_grain::window_cte_anchor` all resolve the SAME reference and must
    // agree. `expand::window` already used the canonical key, so the CREATE
    // side was the stricter one -- it rejected definitions the emitter could
    // have computed.
    // ---------------------------------------------------------------------

    #[test]
    fn parse_keyword_body_window_quoted_inner_metric_accepted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(\"total_qty\") OVER (PARTITION BY EXCLUDING region)\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "a quoted inner-metric reference names the same metric as its \
             unquoted declaration: {:?}",
            result.err().map(|e| e.message)
        );
    }

    #[test]
    fn parse_keyword_body_window_quoted_case_varied_inner_metric_accepted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(\"Total_Qty\") OVER (PARTITION BY EXCLUDING region)\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "DuckDB folds case inside quotes too, so \"Total_Qty\" is total_qty: {:?}",
            result.err().map(|e| e.message)
        );
    }

    #[test]
    fn parse_keyword_body_window_quoted_order_by_dim_accepted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY \"region\")\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "a quoted ORDER BY dimension reference names the declared dimension: {:?}",
            result.err().map(|e| e.message)
        );
    }

    /// Control: the checks must still REJECT a genuinely unknown name. Without
    /// this, "accept quoted references" could degenerate into "accept anything".
    #[test]
    fn parse_keyword_body_window_quoted_unknown_inner_metric_still_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(\"nope\") OVER (PARTITION BY EXCLUDING region)\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_err(),
            "quoting an unknown name must not make it resolve"
        );
        assert!(
            result.unwrap_err().message.contains("inner metric"),
            "the rejection must still name the inner-metric slot"
        );
    }

    // PARSE-8 (code-review 2026-08-06): EXP-12 migrated the ORDER BY and
    // inner-metric slots onto `ident_matches`, but four sibling CREATE-time
    // validators kept raw `eq_ignore_ascii_case`, which compares the quote
    // characters as data. So `PARTITION BY EXCLUDING "Region"` was rejected
    // against a `region` dimension while `ORDER BY "Region"` in the SAME clause
    // resolved — and the expansion layer, already migrated, would have computed
    // it. As with EXP-12, the CREATE-time validator was the stricter one.
    //
    // TECH-DEBT #28's closing line reads as if this arc had fully landed; these
    // sites were not enumerated there.

    #[test]
    fn parse_keyword_body_window_quoted_excluding_dim_accepted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING \"Region\")\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "a quoted EXCLUDING dimension names the declared dimension, exactly \
             as the ORDER BY slot beside it already accepts: {:?}",
            result.err().map(|e| e.message)
        );
    }

    #[test]
    fn parse_keyword_body_window_quoted_partition_dim_accepted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY \"Region\")\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "a quoted PARTITION BY dimension names the declared dimension: {:?}",
            result.err().map(|e| e.message)
        );
    }

    #[test]
    fn parse_keyword_body_materialization_quoted_dimension_accepted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.total_qty AS SUM(o.qty)) \
                     MATERIALIZATIONS (m AS (TABLE mt, DIMENSIONS (\"Region\"), METRICS (total_qty)))";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "a quoted materialization dimension names the declared dimension: {:?}",
            result.err().map(|e| e.message)
        );
    }

    #[test]
    fn parse_keyword_body_materialization_quoted_metric_accepted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.total_qty AS SUM(o.qty)) \
                     MATERIALIZATIONS (m AS (TABLE mt, DIMENSIONS (region), METRICS (\"Total_Qty\")))";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "a quoted materialization metric names the declared metric: {:?}",
            result.err().map(|e| e.message)
        );
    }

    // The fifth site of the same class. The review left it uncounted because
    // TECH-DEBT #28 already mentions it -- but #28's closing line reads "the
    // reference-tokenizer arc (#28) is fully landed", and migrating four of the
    // five sites would recreate exactly that misreading. It mirrors the ORDER BY
    // site including the D-08 dotted-path branch.
    #[test]
    fn parse_keyword_body_quoted_non_additive_by_dim_accepted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.bal NON ADDITIVE BY (\"Region\") AS SUM(o.amount))";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "a quoted NON ADDITIVE BY dimension names the declared dimension: {:?}",
            result.err().map(|e| e.message)
        );
    }

    #[test]
    fn parse_keyword_body_quoted_dotted_non_additive_by_dim_accepted() {
        // The D-08 dotted branch must fold the same way on both halves.
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.bal NON ADDITIVE BY (\"O\".\"Region\") AS SUM(o.amount))";
        let result = parse_keyword_body(body, 0);
        assert!(
            result.is_ok(),
            "a quoted alias-qualified NON ADDITIVE BY dimension resolves: {:?}",
            result.err().map(|e| e.message)
        );
    }

    #[test]
    fn parse_keyword_body_quoted_unknown_non_additive_by_dim_still_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.bal NON ADDITIVE BY (\"nope\") AS SUM(o.amount))";
        let err = parse_keyword_body(body, 0)
            .expect_err("quoting an unknown name must not make it resolve");
        assert!(
            err.message
                .contains("does not match any declared dimension"),
            "{}",
            err.message
        );
    }

    /// Controls: quoting an UNKNOWN name must not make it resolve. Without
    /// these, "accept quoted references" could degenerate into "accept
    /// anything" at all four migrated sites.
    #[test]
    fn parse_keyword_body_quoted_unknown_names_still_rejected_at_every_site() {
        let cases = [
            (
                "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS o.region) \
                 METRICS (o.total_qty AS SUM(o.qty), \
                 o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING \"nope\"))",
                "EXCLUDING dimension",
            ),
            (
                "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS o.region) \
                 METRICS (o.total_qty AS SUM(o.qty), \
                 o.avg_qty AS AVG(total_qty) OVER (PARTITION BY \"nope\"))",
                "PARTITION BY dimension",
            ),
            (
                "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS o.region) \
                 METRICS (o.total_qty AS SUM(o.qty)) \
                 MATERIALIZATIONS (m AS (TABLE mt, DIMENSIONS (\"nope\"), METRICS (total_qty)))",
                "dimension",
            ),
            (
                "AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS o.region) \
                 METRICS (o.total_qty AS SUM(o.qty)) \
                 MATERIALIZATIONS (m AS (TABLE mt, DIMENSIONS (region), METRICS (\"nope\")))",
                "metric",
            ),
        ];
        for (body, slot) in cases {
            let result = parse_keyword_body(body, 0);
            let err = result.expect_err(&format!(
                "quoting an unknown name must not resolve it ({slot})"
            ));
            assert!(
                err.message.contains("not found"),
                "the rejection must still name the missing reference ({slot}): {}",
                err.message
            );
        }
    }

    #[test]
    fn parse_keyword_body_window_inner_metric_invalid_error() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(nonexistent_metric) OVER (PARTITION BY EXCLUDING region ORDER BY region)\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("inner metric"),
            "Error should mention inner metric: {}",
            err.message
        );
        assert!(
            err.message.contains("nonexistent_metric"),
            "Error should mention the invalid metric: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_window_order_by_invalid_dim_error() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY bad_dim)\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("ORDER BY dimension"),
            "Error should mention ORDER BY dimension: {}",
            err.message
        );
        assert!(
            err.message.contains("bad_dim"),
            "Error should mention the invalid dim: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_window_valid_refs_succeed() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region, o.month AS date_trunc('month', o.created_at)) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY month)\
                     )";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics.len(), 2);
        assert!(kb.metrics[1].window_spec.is_some());
    }

    #[test]
    fn parse_metrics_window_partition_by_explicit() {
        let result = parse_metrics_clause(
            "o.avg_qty AS AVG(total_qty) OVER (PARTITION BY store ORDER BY date)",
            0,
        )
        .unwrap();
        let ws = result[0].window_spec.as_ref().unwrap();
        assert_eq!(ws.window_function, "AVG");
        assert_eq!(ws.inner_metric, "total_qty");
        assert!(ws.excluding_dims.is_empty());
        assert_eq!(ws.partition_dims, vec!["store"]);
        assert_eq!(ws.order_by.len(), 1);
        assert_eq!(ws.order_by[0].expr, "date");
    }

    #[test]
    fn parse_metrics_window_partition_by_multiple_dims() {
        let result = parse_metrics_clause(
            "o.avg_qty AS AVG(total_qty) OVER (PARTITION BY store, region ORDER BY date DESC NULLS FIRST)",
            0,
        )
        .unwrap();
        let ws = result[0].window_spec.as_ref().unwrap();
        assert!(ws.excluding_dims.is_empty());
        assert_eq!(ws.partition_dims, vec!["store", "region"]);
        assert_eq!(ws.order_by[0].order, SortOrder::Desc);
        assert_eq!(ws.order_by[0].nulls, NullsOrder::First);
    }

    #[test]
    fn parse_metrics_window_partition_by_no_order() {
        let result =
            parse_metrics_clause("o.avg_qty AS AVG(total_qty) OVER (PARTITION BY store)", 0)
                .unwrap();
        let ws = result[0].window_spec.as_ref().unwrap();
        assert!(ws.excluding_dims.is_empty());
        assert_eq!(ws.partition_dims, vec!["store"]);
        assert!(ws.order_by.is_empty());
    }

    #[test]
    fn parse_keyword_body_window_partition_by_integration() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.store AS o.store, o.date AS o.sale_date) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY store ORDER BY date)\
                     )";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.metrics.len(), 2);
        let ws = kb.metrics[1].window_spec.as_ref().unwrap();
        assert!(ws.excluding_dims.is_empty());
        assert_eq!(ws.partition_dims, vec!["store"]);
        assert_eq!(ws.order_by[0].expr, "date");
    }

    #[test]
    fn parse_keyword_body_window_partition_by_invalid_dim_error() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (\
                         o.total_qty AS SUM(o.qty), \
                         o.avg_qty AS AVG(total_qty) OVER (PARTITION BY nonexistent ORDER BY region)\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("PARTITION BY dimension"),
            "Error should mention PARTITION BY dimension: {}",
            err.message
        );
        assert!(
            err.message.contains("nonexistent"),
            "Error should mention the invalid dim: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // Phase 54: MATERIALIZATIONS clause tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_materializations_clause_single_entry() {
        let body = "daily_rev AS (\n\
                         TABLE daily_revenue_agg,\n\
                         DIMENSIONS (region),\n\
                         METRICS (revenue, order_count)\n\
                     )";
        let result = parse_materializations_clause(body, 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "daily_rev");
        assert_eq!(result[0].table, "daily_revenue_agg");
        assert_eq!(result[0].dimensions, vec!["region"]);
        assert_eq!(result[0].metrics, vec!["revenue", "order_count"]);
    }

    #[test]
    fn parse_materializations_clause_multiple_entries() {
        let body = "daily_rev AS (\n\
                         TABLE daily_agg,\n\
                         DIMENSIONS (region),\n\
                         METRICS (revenue)\n\
                     ),\n\
                     monthly_rev AS (\n\
                         TABLE monthly_agg,\n\
                         DIMENSIONS (region),\n\
                         METRICS (revenue, order_count)\n\
                     )";
        let result = parse_materializations_clause(body, 0).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "daily_rev");
        assert_eq!(result[0].table, "daily_agg");
        assert_eq!(result[1].name, "monthly_rev");
        assert_eq!(result[1].table, "monthly_agg");
        assert_eq!(result[1].metrics, vec!["revenue", "order_count"]);
    }

    #[test]
    fn parse_materializations_clause_dimensions_only() {
        let body = "dims_only AS (\n\
                         TABLE t1,\n\
                         DIMENSIONS (region, date_dim)\n\
                     )";
        let result = parse_materializations_clause(body, 0).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].dimensions, vec!["region", "date_dim"]);
        assert!(result[0].metrics.is_empty());
    }

    #[test]
    fn parse_materializations_clause_metrics_only() {
        let body = "mets_only AS (\n\
                         TABLE t2,\n\
                         METRICS (revenue)\n\
                     )";
        let result = parse_materializations_clause(body, 0).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].dimensions.is_empty());
        assert_eq!(result[0].metrics, vec!["revenue"]);
    }

    #[test]
    fn parse_materializations_clause_rejects_empty() {
        let body = "empty_mat AS (\n\
                         TABLE some_table\n\
                     )";
        let result = parse_materializations_clause(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message
                .contains("must specify at least one of DIMENSIONS or METRICS"),
            "Expected error about empty dims/metrics: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // F-4 (code-review 2026-07-16): the MATERIALIZATIONS sub-body tolerated
    // junk before the first sub-keyword, duplicate sub-clauses (last-wins),
    // and trailing junk after a `(...)` list — all silently.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_materializations_junk_before_first_subclause_rejected() {
        let err =
            parse_materializations_clause("mm AS (junk TABLE t, DIMENSIONS (d))", 0).unwrap_err();
        assert!(
            err.message
                .contains("unexpected text 'junk' before the first sub-clause"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_materializations_duplicate_table_rejected() {
        let err = parse_materializations_clause("mm AS (TABLE t, TABLE u, DIMENSIONS (d))", 0)
            .unwrap_err();
        assert!(
            err.message.contains("duplicate TABLE sub-clause"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_materializations_duplicate_dimensions_rejected() {
        let err =
            parse_materializations_clause("mm AS (TABLE t, DIMENSIONS (a), DIMENSIONS (b))", 0)
                .unwrap_err();
        assert!(
            err.message.contains("duplicate DIMENSIONS sub-clause"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_materializations_junk_after_list_rejected() {
        let err =
            parse_materializations_clause("mm AS (TABLE t, DIMENSIONS (d) junk)", 0).unwrap_err();
        assert!(
            err.message
                .contains("Unexpected text 'junk' after a MATERIALIZATIONS sub-clause list"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_with_materializations() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.revenue AS SUM(o.amount)) \
                     MATERIALIZATIONS (\
                         daily_rev AS (\
                             TABLE daily_agg,\
                             DIMENSIONS (region),\
                             METRICS (revenue)\
                         )\
                     )";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.materializations.len(), 1);
        assert_eq!(kb.materializations[0].name, "daily_rev");
        assert_eq!(kb.materializations[0].table, "daily_agg");
        assert_eq!(kb.materializations[0].dimensions, vec!["region"]);
        assert_eq!(kb.materializations[0].metrics, vec!["revenue"]);
    }

    #[test]
    fn parse_keyword_body_materializations_before_metrics_rejected() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     MATERIALIZATIONS (\
                         mat AS (TABLE t, DIMENSIONS (region))\
                     ) \
                     METRICS (o.revenue AS SUM(o.amount))";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("out of order"),
            "Expected ordering error: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_materialization_bad_dim_ref() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.revenue AS SUM(o.amount)) \
                     MATERIALIZATIONS (\
                         mat AS (\
                             TABLE t,\
                             DIMENSIONS (nonexistent_dim),\
                             METRICS (revenue)\
                         )\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message
                .contains("dimension 'nonexistent_dim' not found"),
            "Expected dim ref error: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_materialization_bad_dim_ref_with_suggestion() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.revenue AS SUM(o.amount)) \
                     MATERIALIZATIONS (\
                         mat AS (\
                             TABLE t,\
                             DIMENSIONS (regin),\
                             METRICS (revenue)\
                         )\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("dimension 'regin' not found"),
            "Expected dim ref error: {}",
            err.message
        );
        assert!(
            err.message.contains("Did you mean 'region'"),
            "Expected suggestion: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_materialization_bad_metric_ref() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.revenue AS SUM(o.amount)) \
                     MATERIALIZATIONS (\
                         mat AS (\
                             TABLE t,\
                             DIMENSIONS (region),\
                             METRICS (nonexistent_metric)\
                         )\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message
                .contains("metric 'nonexistent_metric' not found"),
            "Expected metric ref error: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_materialization_duplicate_name() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.revenue AS SUM(o.amount)) \
                     MATERIALIZATIONS (\
                         same_name AS (\
                             TABLE t1,\
                             DIMENSIONS (region)\
                         ),\
                         same_name AS (\
                             TABLE t2,\
                             METRICS (revenue)\
                         )\
                     )";
        let result = parse_keyword_body(body, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("Duplicate materialization name"),
            "Expected duplicate name error: {}",
            err.message
        );
    }

    #[test]
    fn parse_keyword_body_without_materializations_has_empty_vec() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                     DIMENSIONS (o.region AS o.region) \
                     METRICS (o.revenue AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert!(
            kb.materializations.is_empty(),
            "No MATERIALIZATIONS clause should produce empty vec"
        );
    }

    #[test]
    fn parse_materializations_qualified_table_name() {
        let body = "qual_mat AS (\n\
                         TABLE catalog.schema.daily_revenue_agg,\n\
                         DIMENSIONS (region),\n\
                         METRICS (revenue)\n\
                     )";
        let result = parse_materializations_clause(body, 0).unwrap();
        assert_eq!(result[0].table, "catalog.schema.daily_revenue_agg");
    }

    // -----------------------------------------------------------------------
    // Porting / diagnostics batch (code-review 2026-07-16): F-7 optional table
    // alias, F-9 multi-token name rejection, F-11 empty-quoted rejection, F-12
    // PUBLIC-on-dims + optional `=` in WITH SYNONYMS, F-14 bounded dot search.
    // -----------------------------------------------------------------------

    // --- F-7: the table alias is optional (Snowflake: `[alias AS] table`) ---

    #[test]
    fn f7_tables_bare_name_defaults_alias() {
        let result = parse_tables_clause("orders PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result[0].alias, "orders");
        assert_eq!(result[0].table, "orders");
        assert_eq!(result[0].pk_columns, vec!["id"]);
    }

    #[test]
    fn f7_tables_bare_name_no_constraint() {
        let result = parse_tables_clause("orders", 0).unwrap();
        assert_eq!(result[0].alias, "orders");
        assert_eq!(result[0].table, "orders");
        assert!(result[0].pk_columns.is_empty());
    }

    #[test]
    fn f7_tables_explicit_alias_still_parses() {
        // The `alias AS table` form is unchanged.
        let result = parse_tables_clause("o AS orders PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result[0].alias, "o");
        assert_eq!(result[0].table, "orders");
    }

    #[test]
    fn f7_tables_quoted_bare_name() {
        // A quoted alias-less name is captured whole and defaults the alias.
        let result = parse_tables_clause("\"my orders\" PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result[0].alias, "\"my orders\"");
        assert_eq!(result[0].table, "\"my orders\"");
    }

    #[test]
    fn f7_alias_less_qualified_name_uses_last_component() {
        // A qualified alias-less table name defaults the alias to its LAST
        // identifier component (not the whole dotted name), so the `alias.name`
        // reference splitter can still use it. The physical table keeps the
        // full qualified form. (Copilot review, F-7 follow-up.)
        let result = parse_tables_clause("schema.orders PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result[0].alias, "orders");
        assert_eq!(result[0].table, "schema.orders");

        // Quoted last part is preserved.
        let result = parse_tables_clause("schema.\"my orders\" PRIMARY KEY (id)", 0).unwrap();
        assert_eq!(result[0].alias, "\"my orders\"");
        assert_eq!(result[0].table, "schema.\"my orders\"");
    }

    // --- F-9: a name slot must be a single identifier, not a token run ---

    #[test]
    fn f9_dimension_multi_token_name_rejected() {
        let err = parse_qualified_entries("o.d junk AS o.x", 0, false, "dimensions").unwrap_err();
        assert!(
            err.message.contains("not a single identifier"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn f9_metric_multi_token_name_rejected() {
        let err = parse_metrics_clause("o.d junk AS SUM(o.v)", 0).unwrap_err();
        assert!(
            err.message.contains("not a single identifier"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn f9_derived_metric_multi_token_name_rejected() {
        let err = parse_metrics_clause("total junk AS revenue + 1", 0).unwrap_err();
        assert!(
            err.message.contains("not a single identifier"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn f9_relationship_multi_token_name_rejected() {
        let err = parse_relationships_clause("foo bar AS f(id) REFERENCES a", 0).unwrap_err();
        assert!(
            err.message.contains("not a single identifier"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn f9_quoted_name_with_space_still_valid() {
        // A quoted identifier that contains whitespace is a single token.
        let result =
            parse_qualified_entries("o.\"order date\" AS o.d", 0, false, "dimensions").unwrap();
        assert_eq!(result[0].name, "\"order date\"");
    }

    // --- A bracket in a name slot: the clause scan already counted it as an
    // open bracket, so the entry runs on to the next close (fuzz_create_text,
    // run 40191639265). Each slot reports on its own; TABLES already refuses
    // a bracket while it reads the alias and table name. ---

    fn assert_bracket_refused(result: Result<impl std::fmt::Debug, ParseError>) {
        let err = result.expect_err("an unquoted bracket in a name slot is refused");
        assert!(err.message.contains("unquoted '('"), "got: {}", err.message);
    }

    #[test]
    fn bracket_in_dimension_name_rejected() {
        assert_bracket_refused(parse_qualified_entries(
            "o.(egion AS o.region)",
            0,
            false,
            "dimensions",
        ));
    }

    #[test]
    fn bracket_in_dimension_alias_rejected() {
        assert_bracket_refused(parse_qualified_entries(
            "(o).region AS o.region",
            0,
            false,
            "dimensions",
        ));
    }

    #[test]
    fn bracket_in_fact_name_rejected() {
        assert_bracket_refused(parse_qualified_entries("o.(f) AS o.v", 0, true, "facts"));
    }

    #[test]
    fn bracket_in_metric_name_rejected() {
        assert_bracket_refused(parse_metrics_clause("o.(n) AS COUNT(*)", 0));
    }

    #[test]
    fn bracket_in_metric_alias_rejected() {
        assert_bracket_refused(parse_metrics_clause("(o).n AS COUNT(*)", 0));
    }

    #[test]
    fn bracket_in_derived_metric_name_rejected() {
        assert_bracket_refused(parse_metrics_clause("(t) AS 1", 0));
    }

    #[test]
    fn bracket_in_relationship_name_rejected() {
        assert_bracket_refused(parse_relationships_clause("(r) AS o(cid) REFERENCES c", 0));
    }

    #[test]
    fn quoted_bracket_in_name_still_valid() {
        let result =
            parse_qualified_entries("o.\"(egion\" AS o.region", 0, false, "dimensions").unwrap();
        assert_eq!(result[0].name, "\"(egion\"");
    }

    // --- F-11: an empty quoted identifier `""` in a name/alias slot ---

    #[test]
    fn f11_empty_quoted_table_alias_rejected() {
        let err = parse_tables_clause("\"\" AS orders PRIMARY KEY (id)", 0).unwrap_err();
        assert!(
            err.message.contains("empty quoted identifier"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn f11_empty_quoted_dimension_alias_rejected() {
        let err = parse_qualified_entries("\"\".name AS x", 0, false, "dimensions").unwrap_err();
        assert!(
            err.message.contains("empty quoted identifier"),
            "got: {}",
            err.message
        );
    }

    // --- F-12: PUBLIC accepted on dimensions; WITH SYNONYMS `=` optional ---

    #[test]
    fn f12_public_on_dimension_accepted() {
        let result =
            parse_qualified_entries("PUBLIC o.region AS o.region", 0, false, "dimensions").unwrap();
        assert_eq!(result[0].name, "region");
        assert_eq!(result[0].source_alias, "o");
    }

    #[test]
    fn f12_private_on_dimension_still_rejected() {
        let err = parse_qualified_entries("PRIVATE o.region AS o.region", 0, false, "dimensions")
            .unwrap_err();
        assert!(
            err.message.contains("PRIVATE is not supported"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn f12_with_synonyms_without_equals_accepted() {
        // Snowflake accepts `WITH SYNONYMS (...)` without the `=`.
        let (expr, ann) =
            parse_trailing_annotations("SUM(o.amount) WITH SYNONYMS ('territory', 'area')", 0)
                .unwrap();
        assert_eq!(expr, "SUM(o.amount)");
        assert_eq!(ann.synonyms, vec!["territory", "area"]);
    }

    #[test]
    fn f12_with_synonyms_with_equals_still_accepted() {
        let (_expr, ann) =
            parse_trailing_annotations("SUM(o.amount) WITH SYNONYMS = ('territory')", 0).unwrap();
        assert_eq!(ann.synonyms, vec!["territory"]);
    }

    // --- F-14: the qualifier dot search stops before the entry `AS` ---

    #[test]
    fn f14_unqualified_name_reports_qualifier_not_missing_as() {
        // `region AS upper(o.region)` has an unqualified entry name; the dot in
        // the expression must not be mistaken for the qualifier, which
        // previously produced a misleading "Expected 'AS'" error.
        let err = parse_qualified_entries("region AS upper(o.region)", 0, false, "dimensions")
            .unwrap_err();
        assert!(
            err.message.contains("qualified identifier"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn f14_qualified_entry_with_dotted_expression_still_parses() {
        // Guard: a normal qualified entry whose expression also contains a dot
        // still parses — the qualifier is the dot before AS, not the one in the
        // expression.
        let result =
            parse_qualified_entries("o.region AS upper(o.region)", 0, false, "dimensions").unwrap();
        assert_eq!(result[0].source_alias, "o");
        assert_eq!(result[0].name, "region");
        assert_eq!(result[0].expr, "upper(o.region)");
    }

    // --- F-6: a trailing view-level COMMENT after the last clause ---

    #[test]
    fn f6_trailing_view_comment_extracted() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    DIMENSIONS (o.region AS o.region) \
                    METRICS (o.rev AS SUM(o.amount)) COMMENT = 'My view'";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(kb.view_comment.as_deref(), Some("My view"));
        assert_eq!(kb.dimensions.len(), 1);
        assert_eq!(kb.metrics.len(), 1);
    }

    #[test]
    fn f6_no_trailing_comment_is_none() {
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (o.rev AS SUM(o.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert!(kb.view_comment.is_none());
    }

    #[test]
    fn f6_entry_level_comment_not_mistaken_for_view_comment() {
        // A COMMENT inside a clause (on a metric) sits at depth > 0 and must NOT
        // be picked up as the view-level trailing comment.
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (o.rev AS SUM(o.amount) COMMENT = 'metric note')";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert!(kb.view_comment.is_none());
        assert_eq!(kb.metrics[0].comment.as_deref(), Some("metric note"));
    }

    #[test]
    fn f6_view_level_synonyms_rejected() {
        // WITH SYNONYMS is not a view-level annotation.
        let body = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                    METRICS (o.rev AS SUM(o.amount)) COMMENT = 'x' WITH SYNONYMS = ('v')";
        let err = parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message
                .contains("WITH SYNONYMS is not valid at the view level"),
            "got: {}",
            err.message
        );
    }
}
