//! VARIABLES clause parsing (Phase 6.5).
//!
//! Snowflake's grammar, with the clause between RELATIONSHIPS and FACTS:
//!
//! ```text
//! VARIABLES ( variableDef [ , ... ] )
//! variableDef ::= <name> [ <data_type> ] [ { DEFAULT | := } <default_expr> ] [ COMMENT '<text>' ]
//! ```
//!
//! The type runs from the name to the first depth-0 `DEFAULT`, `:=` or
//! `COMMENT`, the default from there to `COMMENT` or the end. Snowflake writes
//! the variable's comment without `=`; `COMMENT = '...'`, the form every other
//! entry takes, is accepted too. What a variable may be (its name against the
//! members', a default with no subquery or parameter) is checked for the DDL
//! and YAML forms alike, by `crate::variables::validate`.

use super::cursor::Cursor;
use super::lexer::{Token, TokenKind};
use super::split_at_depth0_commas;
use crate::errors::ParseError;
use crate::model::Variable;

/// Parse the content inside `VARIABLES (...)`.
pub(crate) fn parse_variables_clause(
    body: &str,
    base_offset: usize,
) -> Result<Vec<Variable>, ParseError> {
    let entries = split_at_depth0_commas(body)?;
    if entries.is_empty() {
        return Err(ParseError {
            message: "VARIABLES needs at least one variable: 'name [type] [DEFAULT <expr>]'."
                .to_string(),
            position: Some(base_offset),
        });
    }
    entries
        .into_iter()
        .map(|(start, entry)| parse_variable_entry(entry, base_offset + start))
        .collect()
}

/// Where the default starts: after `DEFAULT`, or after `:=` (two symbols).
enum Boundary {
    Default(usize),
    Assign(usize),
    Comment(usize),
}

/// Parse one entry, `name [type] [{DEFAULT | :=} expr] [COMMENT '...']`.
pub(crate) fn parse_variable_entry(entry: &str, offset: usize) -> Result<Variable, ParseError> {
    let mut cur = Cursor::new(entry, offset);
    let toks: Vec<Token> = std::iter::from_fn(|| cur.bump()).collect();
    let cur = Cursor::new(entry, offset);
    if let Some(t) = toks
        .iter()
        .find(|t| matches!(t.kind, TokenKind::Unterminated { .. }))
    {
        return Err(cur.err(
            t.start,
            "Unterminated quote in a VARIABLES entry.".to_string(),
        ));
    }
    let Some(&name_tok) = toks.first() else {
        return Err(cur.err(0, "Missing variable name.".to_string()));
    };
    let name = cur.text(name_tok);
    if !matches!(name_tok.kind, TokenKind::Ident { .. })
        || toks
            .get(1)
            .is_some_and(|t| t.kind == TokenKind::Symbol(b'.') && t.start == name_tok.end)
    {
        return Err(cur.err(
            name_tok.start,
            format!(
                "Expected a variable name at the start of '{entry}': a variable is \
                 'name [type] [DEFAULT <expr>] [COMMENT '...']'."
            ),
        ));
    }
    if let Some(reason) = super::scan::ddl_identifier_slot_error(name) {
        return Err(cur.err(name_tok.start, format!("Invalid variable name: {reason}.")));
    }

    let boundaries = boundaries(&cur, &toks);
    let first = boundaries.first();
    let type_end = match first {
        Some(Boundary::Default(i) | Boundary::Assign(i) | Boundary::Comment(i)) => *i,
        None => toks.len(),
    };
    let data_type = span(entry, &toks[1..type_end]);

    let mut default = None;
    let mut comment_at = None;
    match first {
        Some(Boundary::Default(i) | Boundary::Assign(i)) => {
            let starts = if matches!(first, Some(Boundary::Assign(_))) {
                i + 2
            } else {
                i + 1
            };
            let ends = match boundaries.get(1) {
                Some(Boundary::Comment(j)) => {
                    comment_at = Some(*j);
                    *j
                }
                Some(Boundary::Default(j) | Boundary::Assign(j)) => {
                    return Err(cur.err(
                        toks[*j].start,
                        format!("Variable '{name}' has two defaults; give one."),
                    ));
                }
                None => toks.len(),
            };
            default = span(entry, &toks[starts..ends]);
            if default.is_none() {
                return Err(cur.err(
                    toks[*i].start,
                    format!("Expected an expression after the default of variable '{name}'."),
                ));
            }
        }
        Some(Boundary::Comment(i)) => comment_at = Some(*i),
        None => {}
    }

    let comment = match comment_at {
        None => None,
        Some(i) => Some(take_comment(&cur, entry, &toks, i, name)?),
    };
    Ok(Variable {
        name: name.to_string(),
        data_type,
        default,
        comment,
    })
}

/// The depth-0 `DEFAULT`, `:=` and `COMMENT` tokens of an entry, in order. A
/// `COMMENT` counts only when a string (or `=` and a string) follows it, so a
/// default that calls a function named `comment` is not cut short.
fn boundaries(cur: &Cursor<'_>, toks: &[Token]) -> Vec<Boundary> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate().skip(1) {
        match t.kind {
            TokenKind::Symbol(b'(' | b'[' | b'{') => depth += 1,
            TokenKind::Symbol(b')' | b']' | b'}') => depth -= 1,
            TokenKind::Symbol(b':')
                if depth == 0
                    && toks
                        .get(i + 1)
                        .is_some_and(|n| n.kind == TokenKind::Symbol(b'=') && n.start == t.end) =>
            {
                out.push(Boundary::Assign(i));
            }
            _ if depth == 0 && cur.is_kw(*t, "DEFAULT") => out.push(Boundary::Default(i)),
            _ if depth == 0 && cur.is_kw(*t, "COMMENT") && comment_follows(toks, i) => {
                out.push(Boundary::Comment(i));
            }
            _ => {}
        }
    }
    out
}

fn comment_follows(toks: &[Token], i: usize) -> bool {
    match toks.get(i + 1).map(|t| t.kind) {
        Some(TokenKind::String) => true,
        Some(TokenKind::Symbol(b'=')) => {
            toks.get(i + 2).is_some_and(|t| t.kind == TokenKind::String)
        }
        _ => false,
    }
}

/// `COMMENT [=] '<text>'` ending the entry.
fn take_comment(
    cur: &Cursor<'_>,
    entry: &str,
    toks: &[Token],
    at: usize,
    name: &str,
) -> Result<String, ParseError> {
    let mut i = at + 1;
    if toks
        .get(i)
        .is_some_and(|t| t.kind == TokenKind::Symbol(b'='))
    {
        i += 1;
    }
    let lit = toks[i];
    if let Some(extra) = toks.get(i + 1) {
        return Err(cur.err(
            extra.start,
            format!(
                "Unexpected '{}' after the comment of variable '{name}'; COMMENT comes last.",
                cur.text(*extra)
            ),
        ));
    }
    crate::util::extract_single_quoted_prefix(&entry[lit.start..lit.end])
        .map(|(text, _)| text)
        .map_err(|_| cur.err(lit.start, "Expected single-quoted string.".to_string()))
}

/// The source text the tokens cover, or `None` for no tokens.
fn span(entry: &str, toks: &[Token]) -> Option<String> {
    let (first, last) = (toks.first()?, toks.last()?);
    Some(entry[first.start..last.end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body_parser::parse_keyword_body;

    fn entry(text: &str) -> Variable {
        parse_variable_entry(text, 0).unwrap_or_else(|e| panic!("{text}: {}", e.message))
    }

    fn entry_err(text: &str) -> (String, Option<usize>) {
        let e = parse_variable_entry(text, 0).expect_err(text);
        (e.message, e.position)
    }

    fn var(name: &str, t: Option<&str>, d: Option<&str>, c: Option<&str>) -> Variable {
        Variable {
            name: name.to_string(),
            data_type: t.map(str::to_string),
            default: d.map(str::to_string),
            comment: c.map(str::to_string),
        }
    }

    /// Snowflake's documented entries.
    #[test]
    fn snowflakes_examples_parse() {
        assert_eq!(
            entry("threshold DECIMAL(10,2) DEFAULT 100.50"),
            var("threshold", Some("DECIMAL(10,2)"), Some("100.50"), None)
        );
        assert_eq!(
            entry("region_filter VARCHAR"),
            var("region_filter", Some("VARCHAR"), None, None)
        );
        assert_eq!(
            entry("start_date DATE DEFAULT CURRENT_DATE()"),
            var("start_date", Some("DATE"), Some("CURRENT_DATE()"), None)
        );
        assert_eq!(
            entry("multiplier DOUBLE := 1.0"),
            var("multiplier", Some("DOUBLE"), Some("1.0"), None)
        );
        assert_eq!(
            entry("target_region VARCHAR COMMENT 'the region'"),
            var("target_region", Some("VARCHAR"), None, Some("the region"))
        );
    }

    #[test]
    fn every_part_but_the_name_is_optional() {
        assert_eq!(entry("v"), var("v", None, None, None));
        assert_eq!(entry("v DEFAULT 1"), var("v", None, Some("1"), None));
        assert_eq!(entry("v := 1"), var("v", None, Some("1"), None));
        assert_eq!(
            entry("v COMMENT = 'it''s'"),
            var("v", None, None, Some("it's"))
        );
        assert_eq!(
            entry("\"Min Amount\" TIMESTAMP WITH TIME ZONE DEFAULT now() COMMENT 'x'"),
            var(
                "\"Min Amount\"",
                Some("TIMESTAMP WITH TIME ZONE"),
                Some("now()"),
                Some("x")
            )
        );
    }

    /// A `DEFAULT` or `COMMENT` inside brackets or a string belongs to the
    /// default, and `comment` called as a function is no COMMENT.
    #[test]
    fn keywords_inside_the_default_stay_in_it() {
        assert_eq!(
            entry("v VARCHAR DEFAULT coalesce('DEFAULT', 'COMMENT x')"),
            var(
                "v",
                Some("VARCHAR"),
                Some("coalesce('DEFAULT', 'COMMENT x')"),
                None
            )
        );
        assert_eq!(
            entry("v DEFAULT comment(1) COMMENT 'c'"),
            var("v", None, Some("comment(1)"), Some("c"))
        );
    }

    #[test]
    fn a_malformed_entry_is_refused_at_its_place() {
        assert_eq!(
            entry_err("v DEFAULT"),
            (
                "Expected an expression after the default of variable 'v'.".to_string(),
                Some(2)
            )
        );
        let (m, at) = entry_err("v DEFAULT 1 DEFAULT 2");
        assert!(m.contains("has two defaults"), "{m}");
        assert_eq!(at, Some(12));
        let (m, at) = entry_err("v COMMENT 'c' DEFAULT 1");
        assert!(m.contains("COMMENT comes last"), "{m}");
        assert_eq!(at, Some(14));
        let (m, _) = entry_err("o.v INTEGER");
        assert!(m.contains("Expected a variable name"), "{m}");
        let (m, _) = entry_err("'v' INTEGER");
        assert!(m.contains("Expected a variable name"), "{m}");
    }

    #[test]
    fn the_clause_sits_between_relationships_and_facts() {
        let body = "AS TABLES (s AS sales PRIMARY KEY (id)) \
                    VARIABLES (m INTEGER DEFAULT 1, t VARCHAR) \
                    FACTS (s.f AS s.amount * m) METRICS (s.x AS SUM(s.amount))";
        let kb = parse_keyword_body(body, 0).unwrap();
        assert_eq!(
            kb.variables,
            vec![
                var("m", Some("INTEGER"), Some("1"), None),
                var("t", Some("VARCHAR"), None, None)
            ]
        );
        let e = parse_keyword_body(
            "AS TABLES (s AS sales) FACTS (s.f AS 1) VARIABLES (m) METRICS (s.x AS 1)",
            0,
        )
        .unwrap_err();
        assert!(
            e.message.contains("'VARIABLES' appears out of order"),
            "{}",
            e.message
        );
        let e = parse_keyword_body("AS TABLES (s AS sales) VARIABLES () METRICS (s.x AS 1)", 0)
            .unwrap_err();
        assert!(
            e.message.contains("VARIABLES needs at least one"),
            "{}",
            e.message
        );
    }

    /// What a variable may be is checked on the DDL form as on YAML.
    #[test]
    fn the_ddl_form_runs_the_variable_rules() {
        let cases = [
            (
                "VARIABLES (x) DIMENSIONS (s.x AS 1)",
                "has the name of a dimension",
            ),
            ("VARIABLES (m, M) DIMENSIONS (s.d AS 1)", "declared twice"),
            (
                "VARIABLES (m DEFAULT (SELECT 1)) DIMENSIONS (s.d AS 1)",
                "holds a subquery",
            ),
            (
                "VARIABLES (m DEFAULT $1) DIMENSIONS (s.d AS 1)",
                "prepared statement parameter $1",
            ),
            (
                "VARIABLES (n INTEGER DEFAULT 1) DIMENSIONS (s.d AS s.k) \
                 METRICS (s.c AS COUNT(*), s.w AS SUM(c) OVER (PARTITION BY d \
                 ORDER BY d ROWS BETWEEN n PRECEDING AND CURRENT ROW))",
                "in its window frame",
            ),
        ];
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|(clauses, want)| {
                let body = format!("AS TABLES (s AS sales) {clauses}");
                match parse_keyword_body(&body, 0) {
                    Err(e) if e.message.contains(want) => None,
                    Err(e) => Some(format!("{clauses}: {}", e.message)),
                    Ok(_) => Some(format!("{clauses}: accepted")),
                }
            })
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
