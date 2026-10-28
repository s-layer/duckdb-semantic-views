//! Snowflake's metadata-only clauses: `SAMPLE_VALUES`, `IS_ENUM` and
//! `WITH CORTEX SEARCH SERVICE` on a member, and `MAX_STALENESS`,
//! `AI_SQL_GENERATION`, `AI_QUESTION_CATEGORIZATION`, `AI_VERIFIED_QUERIES` and
//! `COPY GRANTS` after a view's last clause.
//!
//! Each reader takes the text just after its keyword and returns what it read
//! with the text after it, for the annotation loop in `annotations.rs` to go on
//! from. `base_offset` is the absolute offset of the text's first byte.

use super::annotations::starts_with_keyword;
use super::scan::extract_paren_prefix;
use super::split_at_depth0_commas;
use crate::errors::ParseError;
use crate::model::{CortexSearchService, MaxStaleness, VerifiedQuery};
use crate::util::{byte_offset_within, extract_single_quoted_prefix, SingleQuoteError};

fn err(message: impl Into<String>, position: usize) -> ParseError {
    ParseError {
        message: message.into(),
        position: Some(position),
    }
}

/// A single-quoted string at the start of `text`, unescaped, and the text after
/// it. `what` names the string in the error.
fn quoted<'t>(text: &'t str, base: usize, what: &str) -> Result<(String, &'t str), ParseError> {
    match extract_single_quoted_prefix(text) {
        Ok((value, consumed)) => Ok((value, &text[consumed..])),
        Err(SingleQuoteError::NotQuoted) => Err(err(
            format!("Expected a single-quoted string {what}."),
            base,
        )),
        Err(SingleQuoteError::Unterminated) => Err(err("Unclosed single-quoted string.", base)),
    }
}

/// `text` without a leading `=` (and the whitespace around it).
fn skip_optional_eq(text: &str) -> &str {
    let text = text.trim_start();
    text.strip_prefix('=').unwrap_or(text).trim_start()
}

/// `SAMPLE_VALUES [=] ('a', ...)`: `text` follows the keyword. The values are
/// string literals, at least one.
pub(super) fn parse_sample_values(
    text: &str,
    base: usize,
) -> Result<(Vec<String>, &str), ParseError> {
    let after_eq = skip_optional_eq(text);
    let at = base + byte_offset_within(text, after_eq);
    let (content, consumed) = extract_paren_prefix(after_eq)
        .ok_or_else(|| err("Expected parenthesized list after SAMPLE_VALUES.", at))?;
    let content_at = base + byte_offset_within(text, content);
    if content.trim_end().ends_with(',') {
        return Err(err(
            "Empty entry in SAMPLE_VALUES. Expected SAMPLE_VALUES ('value', ...).",
            content_at + content.trim_end().len() - 1,
        ));
    }
    let mut values = Vec::new();
    for (_, entry) in split_at_depth0_commas(content)? {
        let trimmed = entry.trim();
        let entry_at = content_at + byte_offset_within(content, trimmed);
        if trimmed.is_empty() {
            return Err(err(
                "Empty entry in SAMPLE_VALUES. Expected SAMPLE_VALUES ('value', ...).",
                entry_at,
            ));
        }
        let (value, rest) = quoted(trimmed, entry_at, "as a sample value")?;
        if !rest.trim().is_empty() {
            return Err(err(
                format!("Unexpected text '{}' after a sample value.", rest.trim()),
                entry_at,
            ));
        }
        values.push(value);
    }
    if values.is_empty() {
        return Err(err(
            "SAMPLE_VALUES needs at least one value, each a single-quoted string.",
            content_at,
        ));
    }
    Ok((values, &after_eq[consumed..]))
}

/// One name part of a Cortex Search Service or its column, stored unquoted.
fn name_parts(text: &str) -> Result<Vec<String>, String> {
    let parts = crate::ident::parse_qualified_identifier_with_quoting(text)?;
    for (part, quoted) in &parts {
        let b = part.as_bytes();
        let bare_ok = b
            .first()
            .is_some_and(|&c| crate::util::is_ident_byte(c) && !c.is_ascii_digit())
            && b.iter().all(|&c| crate::util::is_ident_continue(c));
        if !quoted && !bare_ok {
            return Err(format!(
                "'{part}' is not an identifier; double-quote it to use it as a name"
            ));
        }
        if part.is_empty() {
            return Err("a name part is empty".to_string());
        }
    }
    Ok(parts.into_iter().map(|(p, _)| p).collect())
}

/// The name at the start of `text` (up to whitespace) and the text after it.
fn take_name(text: &str) -> (&str, &str) {
    let end = crate::ident::find_identifier_end(text, false);
    (&text[..end], &text[end..])
}

/// `CORTEX SEARCH SERVICE <name> [USING <column>]`: `text` starts at `CORTEX`
/// (after `WITH`). The name is `service`, `schema.service` or
/// `database.schema.service`.
pub(super) fn parse_cortex_search_service(
    text: &str,
    base: usize,
) -> Result<(CortexSearchService, &str), ParseError> {
    let mut rest = text;
    for kw in ["CORTEX", "SEARCH", "SERVICE"] {
        rest = rest.trim_start();
        if !starts_with_keyword(&rest.to_ascii_uppercase(), kw) {
            return Err(err(
                "Expected WITH CORTEX SEARCH SERVICE <name> [USING <column>].",
                base + byte_offset_within(text, rest),
            ));
        }
        rest = &rest[kw.len()..];
    }
    let rest = rest.trim_start();
    let name_at = base + byte_offset_within(text, rest);
    let (name_text, mut rest) = take_name(rest);
    if name_text.is_empty() {
        return Err(err("Expected a Cortex Search Service name.", name_at));
    }
    let mut parts = name_parts(name_text).map_err(|e| {
        err(
            format!("Invalid Cortex Search Service name '{name_text}': {e}."),
            name_at,
        )
    })?;
    if parts.len() > 3 {
        return Err(err(
            format!(
                "Invalid Cortex Search Service name '{name_text}': a name has at most three \
                 parts (database.schema.service)."
            ),
            name_at,
        ));
    }
    let service = parts.pop().unwrap_or_default();
    let schema = parts.pop();
    let database = parts.pop();
    let mut literal_column = None;
    let after = rest.trim_start();
    if starts_with_keyword(&after.to_ascii_uppercase(), "USING") {
        let col_text_full = after[5..].trim_start();
        let col_at = base + byte_offset_within(text, col_text_full);
        let (col_text, after_col) = take_name(col_text_full);
        let col = name_parts(col_text)
            .ok()
            .filter(|p| p.len() == 1)
            .and_then(|mut p| p.pop())
            .ok_or_else(|| {
                err(
                    format!(
                        "Expected a column name after USING in WITH CORTEX SEARCH SERVICE, \
                         got '{col_text}'."
                    ),
                    col_at,
                )
            })?;
        literal_column = Some(col);
        rest = after_col;
    }
    Ok((
        CortexSearchService {
            service,
            literal_column,
            database,
            schema,
        },
        rest,
    ))
}

/// `MAX_STALENESS = <seconds> | '<interval>'`: `text` follows the keyword.
pub(super) fn parse_max_staleness(
    text: &str,
    base: usize,
) -> Result<(MaxStaleness, &str), ParseError> {
    let after_kw = text.trim_start();
    let Some(after_eq) = after_kw.strip_prefix('=') else {
        return Err(err(
            "Expected '=' after MAX_STALENESS.",
            base + byte_offset_within(text, after_kw),
        ));
    };
    let value_text = after_eq.trim_start();
    let at = base + byte_offset_within(text, value_text);
    let (value, rest) = if value_text.starts_with('\'') {
        let (interval, rest) = quoted(value_text, at, "after MAX_STALENESS =")?;
        (MaxStaleness::Interval(interval), rest)
    } else {
        let digits = value_text.bytes().take_while(u8::is_ascii_digit).count();
        let after = &value_text[digits..];
        if digits == 0
            || after
                .bytes()
                .next()
                .is_some_and(crate::util::is_ident_continue)
        {
            return Err(err(
                "Expected a number of seconds or a quoted interval such as '1 hour' after \
                 MAX_STALENESS =.",
                at,
            ));
        }
        let seconds = value_text[..digits]
            .parse::<u64>()
            .map_err(|_| err("MAX_STALENESS is too large.", at))?;
        (MaxStaleness::Seconds(seconds), after)
    };
    value.seconds().map_err(|e| err(format!("{e}."), at))?;
    Ok((value, rest))
}

/// `AI_SQL_GENERATION '<text>'` or `AI_QUESTION_CATEGORIZATION '<text>'`: `text`
/// follows the keyword. Snowflake writes no `=`; one is accepted.
pub(super) fn parse_instruction<'t>(
    text: &'t str,
    base: usize,
    keyword: &str,
) -> Result<(String, &'t str), ParseError> {
    let value = skip_optional_eq(text);
    quoted(
        value,
        base + byte_offset_within(text, value),
        &format!("after {keyword}"),
    )
}

/// `AI_VERIFIED_QUERIES ( name AS ( QUESTION '...' [VERIFIED_AT <n>]
/// [ONBOARDING_QUESTION TRUE|FALSE] [VERIFIED_BY '...'] SQL '...' ), ... )`:
/// `text` follows the keyword. The sub-clauses may come in any order, each once;
/// `QUESTION` and `SQL` are required. Names must differ (case-insensitively).
pub(super) fn parse_verified_queries(
    text: &str,
    base: usize,
) -> Result<(Vec<VerifiedQuery>, &str), ParseError> {
    let after_kw = skip_optional_eq(text);
    let at = base + byte_offset_within(text, after_kw);
    let (content, consumed) = extract_paren_prefix(after_kw)
        .ok_or_else(|| err("Expected parenthesized list after AI_VERIFIED_QUERIES.", at))?;
    let content_at = base + byte_offset_within(text, content);
    if content.trim_end().ends_with(',') {
        return Err(err(
            "Empty entry in AI_VERIFIED_QUERIES. Expected name AS (QUESTION '...' SQL '...').",
            content_at + content.trim_end().len() - 1,
        ));
    }
    let mut queries: Vec<VerifiedQuery> = Vec::new();
    let mut offsets = Vec::new();
    for (_, entry) in split_at_depth0_commas(content)? {
        let trimmed = entry.trim();
        let entry_at = content_at + byte_offset_within(content, trimmed);
        if trimmed.is_empty() {
            return Err(err(
                "Empty entry in AI_VERIFIED_QUERIES. Expected name AS (QUESTION '...' SQL '...').",
                entry_at,
            ));
        }
        queries.push(parse_verified_query(trimmed, entry_at)?);
        offsets.push(entry_at);
    }
    if queries.is_empty() {
        return Err(err(
            "AI_VERIFIED_QUERIES needs at least one entry: name AS (QUESTION '...' SQL '...').",
            content_at,
        ));
    }
    if let Some(i) = crate::model::duplicate_verified_query(&queries) {
        return Err(err(
            format!("Duplicate verified query '{}'.", queries[i].name),
            offsets[i],
        ));
    }
    Ok((queries, &after_kw[consumed..]))
}

/// One `name AS ( ... )` entry, trimmed.
#[allow(clippy::too_many_lines)]
fn parse_verified_query(entry: &str, base: usize) -> Result<VerifiedQuery, ParseError> {
    let cur = super::cursor::Cursor::new(entry, base);
    let Some(as_tok) = cur.find_kw("AS") else {
        return Err(err(
            format!("Expected 'AS' in verified query '{entry}'. Form: name AS (QUESTION '...' SQL '...')."),
            base,
        ));
    };
    let name = entry[..as_tok.start].trim();
    if name.is_empty() {
        return Err(err("Missing verified query name before 'AS'.", base));
    }
    if let Some(reason) = crate::model::verified_query_name_error(name) {
        return Err(err(
            format!("Invalid verified query name '{name}': {reason}."),
            base,
        ));
    }
    let body = entry[as_tok.end..].trim_start();
    let body_at = base + byte_offset_within(entry, body);
    let (inner, consumed) = extract_paren_prefix(body).ok_or_else(|| {
        err(
            format!("Expected '(' after 'AS' in verified query '{name}'."),
            body_at,
        )
    })?;
    let tail = body[consumed..].trim();
    if !tail.is_empty() {
        return Err(err(
            format!("Unexpected text '{tail}' after verified query '{name}'."),
            base + byte_offset_within(entry, tail),
        ));
    }
    let inner_at = base + byte_offset_within(entry, inner);
    let pos = |sub: &str| inner_at + byte_offset_within(inner, sub);

    let mut question = None;
    let mut sql = None;
    let mut verified_at = None;
    let mut verified_by = None;
    let mut onboarding = None;
    let mut rest = inner;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let upper = rest.to_ascii_uppercase();
        let Some(kw) = [
            "QUESTION",
            "VERIFIED_AT",
            "ONBOARDING_QUESTION",
            "VERIFIED_BY",
            "SQL",
        ]
        .into_iter()
        .find(|k| starts_with_keyword(&upper, k)) else {
            return Err(err(
                format!(
                    "Unexpected text '{rest}' in verified query '{name}'. Expected QUESTION, \
                     VERIFIED_AT, ONBOARDING_QUESTION, VERIFIED_BY or SQL."
                ),
                pos(rest),
            ));
        };
        let duplicate = match kw {
            "QUESTION" => question.is_some(),
            "SQL" => sql.is_some(),
            "VERIFIED_AT" => verified_at.is_some(),
            "VERIFIED_BY" => verified_by.is_some(),
            _ => onboarding.is_some(),
        };
        if duplicate {
            return Err(err(
                format!("Duplicate {kw} in verified query '{name}'."),
                pos(rest),
            ));
        }
        let value = rest[kw.len()..].trim_start();
        let value_at = pos(value);
        match kw {
            "VERIFIED_AT" => {
                let digits = value.bytes().take_while(u8::is_ascii_digit).count();
                let after = &value[digits..];
                if digits == 0
                    || after
                        .bytes()
                        .next()
                        .is_some_and(crate::util::is_ident_continue)
                {
                    return Err(err(
                        format!(
                            "Expected seconds since the Unix epoch after VERIFIED_AT in \
                             verified query '{name}'."
                        ),
                        value_at,
                    ));
                }
                verified_at = Some(
                    value[..digits]
                        .parse::<u64>()
                        .map_err(|_| err("VERIFIED_AT is too large.", value_at))?,
                );
                rest = after;
            }
            "ONBOARDING_QUESTION" => {
                let upper = value.to_ascii_uppercase();
                let (flag, len) = if starts_with_keyword(&upper, "TRUE") {
                    (true, 4)
                } else if starts_with_keyword(&upper, "FALSE") {
                    (false, 5)
                } else {
                    return Err(err(
                        format!(
                            "Expected TRUE or FALSE after ONBOARDING_QUESTION in verified \
                             query '{name}'."
                        ),
                        value_at,
                    ));
                };
                onboarding = Some(flag);
                rest = &value[len..];
            }
            _ => {
                let (s, after) = quoted(value, value_at, &format!("after {kw}"))?;
                match kw {
                    "QUESTION" => question = Some(s),
                    "SQL" => sql = Some(s),
                    _ => verified_by = Some(s),
                }
                rest = after;
            }
        }
    }
    let missing = |what: &str| {
        err(
            format!("Verified query '{name}' needs {what} '...'."),
            inner_at,
        )
    };
    Ok(VerifiedQuery {
        name: name.to_string(),
        question: question.ok_or_else(|| missing("QUESTION"))?,
        verified_at,
        verified_by,
        use_as_onboarding_question: onboarding.unwrap_or(false),
        sql: sql.ok_or_else(|| missing("SQL"))?,
    })
}

/// `COPY GRANTS`: `text` follows `COPY`. Returns the text after `GRANTS`.
pub(super) fn parse_copy_grants(text: &str, base: usize) -> Result<&str, ParseError> {
    let after = text.trim_start();
    if !starts_with_keyword(&after.to_ascii_uppercase(), "GRANTS") {
        return Err(err(
            "Expected GRANTS after COPY.",
            base + byte_offset_within(text, after),
        ));
    }
    Ok(&after[6..])
}

#[cfg(test)]
mod tests {
    use crate::body_parser::{parse_keyword_body, KeywordBody};
    use crate::model::{CortexSearchService, MaxStaleness, VerifiedQuery};

    const TABLES: &str = "TABLES (o AS orders PRIMARY KEY (id))";

    fn body(rest: &str) -> String {
        format!("{TABLES} {rest}")
    }

    fn parse(rest: &str) -> KeywordBody {
        let text = body(rest);
        parse_keyword_body(&text, 0).unwrap_or_else(|e| panic!("{text}: {}", e.message))
    }

    fn err(rest: &str) -> String {
        let text = body(rest);
        match parse_keyword_body(&text, 0) {
            Ok(kb) => panic!("{text}: accepted as {kb:?}"),
            Err(e) => e.message,
        }
    }

    #[test]
    fn snowflakes_sample_values_example_parses() {
        let kb = parse(
            "FACTS (o.amount AS amount COMMENT = 'Transaction amount' \
                 SAMPLE_VALUES ('100', '250', '500', '1000')) \
             DIMENSIONS (o.region AS region COMMENT = 'Sales region' \
                 SAMPLE_VALUES ('East', 'West', 'North', 'South') IS_ENUM, \
                 o.warehouse_name AS WAREHOUSE_NAME COMMENT = 'Name of the warehouse' \
                 SAMPLE_VALUES ('SMALL', 'CLOUD_SERVICES_ONLY', 'SP_WAREHOUSE'))",
        );
        assert_eq!(kb.facts[0].expr, "amount");
        assert_eq!(kb.facts[0].sample_values, ["100", "250", "500", "1000"]);
        assert_eq!(kb.dimensions[0].expr, "region");
        assert_eq!(kb.dimensions[0].comment.as_deref(), Some("Sales region"));
        assert_eq!(
            kb.dimensions[0].sample_values,
            ["East", "West", "North", "South"]
        );
        assert!(kb.dimensions[0].is_enum);
        assert_eq!(kb.dimensions[1].expr, "WAREHOUSE_NAME");
        assert!(!kb.dimensions[1].is_enum);
        assert_eq!(kb.dimensions[1].sample_values.len(), 3);
    }

    #[test]
    fn member_clauses_parse_in_any_order_and_without_others() {
        let kb = parse(
            "DIMENSIONS (o.a AS o.a IS_ENUM, \
             o.b AS upper(o.b) IS_ENUM SAMPLE_VALUES = ('it''s', 'x, y') WITH SYNONYMS ('bee'), \
             o.c AS o.c SAMPLE_VALUES ('') COMMENT = 'c')",
        );
        let d = &kb.dimensions;
        assert!(d[0].is_enum && d[0].sample_values.is_empty() && d[0].expr == "o.a");
        assert_eq!(d[1].expr, "upper(o.b)");
        assert!(d[1].is_enum);
        assert_eq!(d[1].sample_values, ["it's", "x, y"]);
        assert_eq!(d[1].synonyms, ["bee"]);
        assert_eq!(d[2].sample_values, [""]);
        assert_eq!(d[2].comment.as_deref(), Some("c"));
    }

    #[test]
    fn cortex_search_service_reads_every_name_shape() {
        let kb = parse(
            "DIMENSIONS (o.a AS o.a WITH CORTEX SEARCH SERVICE svc, \
             o.b AS o.b COMMENT = 'b' WITH CORTEX SEARCH SERVICE db.sch.svc USING col, \
             o.c AS o.c with cortex search service \"My Schema\".\"svc.x\" using \"Col 1\" \
                 SAMPLE_VALUES ('v'))",
        );
        let c = |i: usize| kb.dimensions[i].cortex_search_service.clone().unwrap();
        assert_eq!(
            c(0),
            CortexSearchService {
                service: "svc".to_string(),
                ..Default::default()
            }
        );
        assert_eq!(
            c(1),
            CortexSearchService {
                service: "svc".to_string(),
                literal_column: Some("col".to_string()),
                database: Some("db".to_string()),
                schema: Some("sch".to_string()),
            }
        );
        assert_eq!(kb.dimensions[1].comment.as_deref(), Some("b"));
        assert_eq!(
            c(2),
            CortexSearchService {
                service: "svc.x".to_string(),
                literal_column: Some("Col 1".to_string()),
                database: None,
                schema: Some("My Schema".to_string()),
            }
        );
        assert_eq!(kb.dimensions[2].sample_values, ["v"]);
    }

    /// A word that only looks like a clause stays in the expression: after an
    /// operator, at the start, or qualified.
    #[test]
    fn lookalikes_in_an_expression_stay_expression_text() {
        let kb = parse(
            "DIMENSIONS (o.a AS sample_values('x'), o.b AS o.x = is_enum, \
             o.c AS o.is_enum, o.d AS is_enum, o.e AS o.f + sample_values ('y'))",
        );
        let exprs: Vec<&str> = kb.dimensions.iter().map(|d| d.expr.as_str()).collect();
        assert_eq!(
            exprs,
            [
                "sample_values('x')",
                "o.x = is_enum",
                "o.is_enum",
                "is_enum",
                "o.f + sample_values ('y')"
            ]
        );
        assert!(kb
            .dimensions
            .iter()
            .all(|d| !d.is_enum && d.sample_values.is_empty()));
    }

    /// A column named `is_enum` after a keyword operator is an operand, not the
    /// clause: `NOT is_enum`, `AND is_enum`, `WHEN is_enum THEN`. One test per
    /// shape, so each reports on its own.
    fn expr_of(entry: &str) -> (String, bool) {
        let kb = parse(&format!("DIMENSIONS ({entry})"));
        let d = &kb.dimensions[0];
        (d.expr.clone(), d.is_enum)
    }

    #[test]
    fn is_enum_after_not_is_an_operand() {
        assert_eq!(expr_of("o.a AS NOT is_enum"), ("NOT is_enum".into(), false));
    }

    #[test]
    fn is_enum_after_and_is_an_operand() {
        assert_eq!(
            expr_of("o.a AS o.flag AND is_enum"),
            ("o.flag AND is_enum".into(), false)
        );
    }

    #[test]
    fn is_enum_inside_a_case_is_an_operand() {
        assert_eq!(
            expr_of("o.a AS CASE WHEN is_enum THEN 1 ELSE 0 END"),
            ("CASE WHEN is_enum THEN 1 ELSE 0 END".into(), false)
        );
    }

    #[test]
    fn is_enum_followed_by_more_expression_is_an_operand() {
        assert_eq!(
            expr_of("o.a AS o.x || is_enum || 'y'"),
            ("o.x || is_enum || 'y'".into(), false)
        );
    }

    #[test]
    fn is_enum_after_an_operand_and_before_another_clause_is_the_clause() {
        assert_eq!(
            expr_of("o.a AS o.a IS_ENUM COMMENT = 'c'"),
            ("o.a".into(), true)
        );
        assert_eq!(expr_of("o.a AS o.a is_enum"), ("o.a".into(), true));
        // A column named like a keyword operator is still an operand.
        assert_eq!(expr_of("o.a AS o.case IS_ENUM"), ("o.case".into(), true));
    }

    #[test]
    fn member_clauses_are_refused_where_snowflake_refuses_them() {
        for (rest, needle) in [
            (
                "FACTS (o.f AS o.f IS_ENUM) DIMENSIONS (o.d AS o.d)",
                "IS_ENUM is not valid on a fact; it applies to dimensions.",
            ),
            (
                "FACTS (o.f AS o.f WITH CORTEX SEARCH SERVICE s) DIMENSIONS (o.d AS o.d)",
                "WITH CORTEX SEARCH SERVICE is not valid on a fact; it applies to dimensions.",
            ),
            (
                "METRICS (o.m AS SUM(o.x) SAMPLE_VALUES ('1'))",
                "SAMPLE_VALUES is not valid on a metric; it applies to facts and dimensions.",
            ),
            (
                "METRICS (o.m AS SUM(o.x) IS_ENUM)",
                "IS_ENUM is not valid on a metric; it applies to dimensions.",
            ),
            (
                "DIMENSIONS (o.d AS o.d) SAMPLE_VALUES ('a')",
                "Unknown clause",
            ),
            (
                "DIMENSIONS (o.d AS o.d) COMMENT = 'v' IS_ENUM",
                "IS_ENUM is not valid at the view level; it applies to dimensions.",
            ),
            (
                "DIMENSIONS (o.d AS o.d) WITH CORTEX SEARCH SERVICE s",
                "WITH CORTEX SEARCH SERVICE is not valid at the view level",
            ),
            (
                "DIMENSIONS (o.d AS o.d SAMPLE_VALUES ('a') SAMPLE_VALUES ('b'))",
                "Duplicate SAMPLE_VALUES annotation.",
            ),
            (
                "DIMENSIONS (o.d AS o.d IS_ENUM IS_ENUM)",
                "Duplicate IS_ENUM annotation.",
            ),
            (
                "DIMENSIONS (o.d AS o.d SAMPLE_VALUES ())",
                "SAMPLE_VALUES needs at least one value",
            ),
            (
                "DIMENSIONS (o.d AS o.d SAMPLE_VALUES ('a',))",
                "Empty entry in SAMPLE_VALUES",
            ),
            (
                "DIMENSIONS (o.d AS o.d COMMENT = 'c' SAMPLE_VALUES (1, 2))",
                "Expected a single-quoted string as a sample value.",
            ),
            (
                "DIMENSIONS (o.d AS o.d WITH CORTEX SEARCH svc)",
                "Expected WITH CORTEX SEARCH SERVICE <name> [USING <column>].",
            ),
            (
                "DIMENSIONS (o.d AS o.d WITH CORTEX SEARCH SERVICE a.b.c.d)",
                "at most three parts",
            ),
            (
                "DIMENSIONS (o.d AS o.d WITH CORTEX SEARCH SERVICE s USING a.b)",
                "Expected a column name after USING",
            ),
            (
                "DIMENSIONS (o.d AS o.d WITH CORTEX SEARCH SERVICE s junk)",
                "Unexpected text in annotations: 'junk'",
            ),
            (
                "DIMENSIONS (o.d AS o.d COMMENT = 'c' MAX_STALENESS = 300)",
                "Unexpected text in annotations",
            ),
            (
                "DIMENSIONS (o.d IS_ENUM AS o.d)",
                "'d IS_ENUM' is not a single identifier",
            ),
        ] {
            let message = err(rest);
            assert!(message.contains(needle), "{rest}\n  got: {message}");
        }
    }

    #[test]
    fn view_clauses_parse_in_snowflakes_order_and_any_other() {
        let kb = parse(
            "DIMENSIONS (o.d AS o.d) COMMENT = 'v' MAX_STALENESS = 300 \
             AI_SQL_GENERATION 'Round to 2 places.' \
             AI_QUESTION_CATEGORIZATION 'Reject HR questions.' \
             AI_VERIFIED_QUERIES ( \
               total AS ( QUESTION 'What is the total?' VERIFIED_AT 1772645863 \
                 ONBOARDING_QUESTION TRUE VERIFIED_BY '(STEWARD = data_stewards)' \
                 SQL 'SELECT SUM(x), ''a'' FROM t' ), \
               \"Second Q\" AS (SQL 'SELECT 1' QUESTION 'One?') \
             ) WITH TAG (t = 'v') COPY GRANTS",
        );
        assert_eq!(kb.view_comment.as_deref(), Some("v"));
        assert_eq!(kb.max_staleness, Some(MaxStaleness::Seconds(300)));
        assert_eq!(
            kb.custom_instructions.sql_generation.as_deref(),
            Some("Round to 2 places.")
        );
        assert_eq!(
            kb.custom_instructions.question_categorization.as_deref(),
            Some("Reject HR questions.")
        );
        assert_eq!(kb.view_tags.len(), 1);
        assert_eq!(
            kb.verified_queries,
            [
                VerifiedQuery {
                    name: "total".to_string(),
                    question: "What is the total?".to_string(),
                    verified_at: Some(1_772_645_863),
                    verified_by: Some("(STEWARD = data_stewards)".to_string()),
                    use_as_onboarding_question: true,
                    sql: "SELECT SUM(x), 'a' FROM t".to_string(),
                },
                VerifiedQuery {
                    name: "\"Second Q\"".to_string(),
                    question: "One?".to_string(),
                    sql: "SELECT 1".to_string(),
                    ..Default::default()
                },
            ]
        );

        let kb = parse(
            "DIMENSIONS (o.d AS o.d) copy grants AI_question_categorization = 'q' \
             max_staleness = '2 Hours'",
        );
        assert_eq!(
            kb.max_staleness,
            Some(MaxStaleness::Interval("2 Hours".to_string()))
        );
        assert_eq!(
            kb.custom_instructions.question_categorization.as_deref(),
            Some("q")
        );
        assert!(kb.verified_queries.is_empty() && kb.view_comment.is_none());
    }

    #[test]
    fn view_clauses_are_checked() {
        for (rest, needle) in [
            (
                "MAX_STALENESS = 119",
                "MAX_STALENESS is 119 seconds; the minimum is 120",
            ),
            ("MAX_STALENESS = '1 minute'", "the minimum is 120"),
            ("MAX_STALENESS = '2 fortnights'", "is not an interval"),
            ("MAX_STALENESS = '2'", "is not an interval"),
            ("MAX_STALENESS 300", "Expected '=' after MAX_STALENESS."),
            ("MAX_STALENESS = 3e5", "Expected a number of seconds"),
            (
                "MAX_STALENESS = 300 MAX_STALENESS = 400",
                "Duplicate MAX_STALENESS",
            ),
            (
                "AI_SQL_GENERATION instructions",
                "Expected a single-quoted string after AI_SQL_GENERATION",
            ),
            (
                "AI_SQL_GENERATION 'a' AI_SQL_GENERATION 'b'",
                "Duplicate AI_SQL_GENERATION",
            ),
            (
                "AI_VERIFIED_QUERIES ()",
                "AI_VERIFIED_QUERIES needs at least one entry",
            ),
            (
                "AI_VERIFIED_QUERIES (q AS (QUESTION 'x'))",
                "Verified query 'q' needs SQL",
            ),
            (
                "AI_VERIFIED_QUERIES (q AS (SQL 'x'))",
                "Verified query 'q' needs QUESTION",
            ),
            (
                "AI_VERIFIED_QUERIES (q AS (QUESTION 'a' QUESTION 'b' SQL 's'))",
                "Duplicate QUESTION in verified query 'q'.",
            ),
            (
                "AI_VERIFIED_QUERIES (q AS (QUESTION 'a' SQL 's'), Q AS (QUESTION 'b' SQL 't'))",
                "Duplicate verified query 'Q'.",
            ),
            (
                "AI_VERIFIED_QUERIES (q AS (QUESTION 'a' ONBOARDING_QUESTION yes SQL 's'))",
                "Expected TRUE or FALSE after ONBOARDING_QUESTION",
            ),
            (
                "AI_VERIFIED_QUERIES (q AS (QUESTION 'a' VERIFIED_AT '2026' SQL 's'))",
                "Expected seconds since the Unix epoch after VERIFIED_AT",
            ),
            (
                "AI_VERIFIED_QUERIES (q AS (QUESTION 'a' SQL 's' LIMIT 1))",
                "Unexpected text 'LIMIT 1' in verified query 'q'",
            ),
            (
                "AI_VERIFIED_QUERIES (q (QUESTION 'a' SQL 's'))",
                "Expected 'AS' in verified query",
            ),
            (
                "AI_VERIFIED_QUERIES (a b AS (QUESTION 'a' SQL 's'))",
                "Invalid verified query name 'a b'",
            ),
            (
                "AI_VERIFIED_QUERIES (q AS (QUESTION 'a' SQL 's') extra)",
                "Unexpected text 'extra' after verified query 'q'.",
            ),
            ("COPY", "Expected GRANTS after COPY."),
            ("COPY GRANTS COPY GRANTS", "Duplicate COPY GRANTS"),
            (
                "WITH SYNONYMS ('x')",
                "WITH SYNONYMS is not valid at the view level",
            ),
        ] {
            let message = err(&format!("DIMENSIONS (o.d AS o.d) {rest}"));
            assert!(message.contains(needle), "{rest}\n  got: {message}");
        }
    }

    /// GET_DDL writes every clause back in a form the parser reads to the same
    /// definition.
    #[test]
    fn a_verified_query_name_is_one_identifier() {
        let e = err("DIMENSIONS (o.d AS o.d) AI_VERIFIED_QUERIES (db.q AS (QUESTION 'x' SQL 'y'))");
        assert!(e.contains("Invalid verified query name 'db.q'"), "{e}");
    }

    #[test]
    fn verified_query_names_differing_only_by_quotes_are_duplicates() {
        let e = err(
            "DIMENSIONS (o.d AS o.d) AI_VERIFIED_QUERIES (q AS (QUESTION 'x' SQL 'y'), \"Q\" AS (QUESTION 'x' SQL 'y'))",
        );
        assert!(e.contains("Duplicate verified query '\"Q\"'"), "{e}");
    }

    #[test]
    fn quoted_verified_query_names_render_and_parse_back() {
        let kb = parse(
            "DIMENSIONS (o.d AS o.d) AI_VERIFIED_QUERIES (\"a,b\" AS (QUESTION 'x' SQL 'y'), \"q)\" AS (QUESTION 'x' SQL 'y'), \
             \"AS\" AS (QUESTION 'x' SQL 'y'))",
        );
        let def = kb.into_definition();
        let ddl = crate::render_ddl::render_create_ddl("v", &def).unwrap();
        let rendered = ddl
            .strip_prefix("CREATE OR REPLACE SEMANTIC VIEW v ")
            .unwrap();
        let back = parse_keyword_body(rendered, 0)
            .unwrap_or_else(|e| panic!("{ddl}\n{}", e.message))
            .into_definition();
        assert_eq!(back.verified_queries, def.verified_queries, "{ddl}");
    }

    #[test]
    fn a_clause_after_an_interval_cast_is_still_a_clause() {
        assert_eq!(
            expr_of("o.a AS o.x::INTERVAL IS_ENUM WITH SYNONYMS ('a')"),
            ("o.x::INTERVAL".into(), true)
        );
        let kb = parse("DIMENSIONS (o.a AS o.x::INTERVAL SAMPLE_VALUES ('1 day'))");
        assert_eq!(kb.dimensions[0].expr, "o.x::INTERVAL");
        assert_eq!(kb.dimensions[0].sample_values, ["1 day"]);
    }

    #[test]
    fn unquoted_sample_values_right_after_the_expression_are_refused() {
        for rest in [
            "DIMENSIONS (o.d AS o.d SAMPLE_VALUES (1, 2))",
            "DIMENSIONS (o.d AS o.d SAMPLE_VALUES (E'x'))",
            "DIMENSIONS (o.d AS o.d SAMPLE_VALUES = (1))",
        ] {
            let e = err(rest);
            assert!(
                e.contains("Expected a single-quoted string as a sample value"),
                "{rest}: {e}"
            );
        }
    }

    #[test]
    fn rendered_clauses_parse_back_unchanged() {
        let kb = parse(
            "FACTS (o.f AS o.f SAMPLE_VALUES ('1', 'it''s')) \
             DIMENSIONS (o.d AS o.d SAMPLE_VALUES ('a') IS_ENUM \
                 WITH CORTEX SEARCH SERVICE \"my db\".s.\"select\" USING \"Col\", \
               o.e AS o.e IS_ENUM WITH CORTEX SEARCH SERVICE svc) \
             MAX_STALENESS = '1 hour' AI_SQL_GENERATION 'g''s' AI_QUESTION_CATEGORIZATION 'q' \
             AI_VERIFIED_QUERIES (a AS (QUESTION 'q?' VERIFIED_AT 5 VERIFIED_BY '(X = y)' \
                 ONBOARDING_QUESTION FALSE SQL 'SELECT ''x'''), \
               b AS (QUESTION 'r' ONBOARDING_QUESTION TRUE SQL 's')) COPY GRANTS",
        );
        let def = kb.into_definition();
        let ddl = crate::render_ddl::render_create_ddl("v", &def).unwrap();
        let rendered = ddl
            .strip_prefix("CREATE OR REPLACE SEMANTIC VIEW v ")
            .unwrap();
        let back = parse_keyword_body(rendered, 0)
            .unwrap_or_else(|e| panic!("{ddl}\n{}", e.message))
            .into_definition();
        assert_eq!(back, def, "{ddl}");
        assert!(!ddl.contains("COPY GRANTS"), "{ddl}");
        assert!(
            ddl.contains("o.d AS o.d SAMPLE_VALUES ('a') IS_ENUM WITH CORTEX SEARCH SERVICE \"my db\".s.\"select\" USING Col"),
            "{ddl}"
        );
    }
}
