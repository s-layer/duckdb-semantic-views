//! AS-body clause scanning: split the body into `KEYWORD (...)` clause bounds.
//!
//! §6.1 (phase 7, code-review 2026-07-11): the clause-header scan runs on the
//! shared [`Cursor`]/lexer. A clause keyword is a bare-identifier token whose
//! first byte is ASCII-alphabetic (so a quoted `"tables"`, a symbol, or a
//! digit/underscore/non-ASCII lead is still the "unexpected character" at top
//! level, exactly as the old byte scan reported), and the `(...)` body is
//! consumed with the quote-aware [`Cursor::take_parens`] — a `)` inside a
//! `'string'` / `"ident"` token cannot close the clause early (PA-6), replacing
//! the hand-rolled `QuoteState` depth loop. Keyword validation (known / unknown
//! with a "did you mean?" suggestion, duplicate, ordering, required) is
//! unchanged.
//!
//! One deliberate divergence from the old prefix scan: because the keyword is a
//! whole lexer token, an ALPHA-led word with a trailing non-alphabetic byte
//! (`TABLES2`, `TABLES_`, `TABLES★`) is now the single token `TABLES2` and so is
//! rejected as `Unknown clause keyword 'TABLES2'`. The old byte scan collected
//! only the `[A-Za-z]+` run, matched `TABLES`, then failed at the trailing char
//! with `Expected '(' ... found '2'`. Both reject; the message differs. (A word
//! whose *first* byte is non-alphabetic is still the "unexpected character"
//! case above — only the trailing-byte shape changed.)

use super::cursor::{closer_of, BracketMismatch, Cursor, UnterminatedRegion};
use super::lexer::TokenKind;
use crate::errors::ParseError;

/// Decode the character starting at byte offset `i` in `text` for an error
/// message. `bytes[i] as char` truncated a multibyte codepoint to its lead
/// byte, so `★` (0xE2 0x98 0x85) surfaced as the mojibake `'â'` (0xE2) —
/// P-14, code-review 2026-07-11. Returns `None` at end-of-input or a
/// non-char-boundary offset (both callers pass boundary offsets).
fn char_at(text: &str, i: usize) -> Option<char> {
    text.get(i..).and_then(|s| s.chars().next())
}

/// Known clause keywords for the AS-body scanner.
const CLAUSE_KEYWORDS: &[&str] = &[
    "tables",
    "relationships",
    "facts",
    "dimensions",
    "metrics",
    "materializations",
];

/// Clause ordering — TABLES must be first, then RELATIONSHIPS (optional),
/// FACTS (optional), DIMENSIONS (optional),
/// METRICS (optional), MATERIALIZATIONS (optional).
/// At least one of DIMENSIONS or METRICS is required.
const CLAUSE_ORDER: &[&str] = &[
    "tables",
    "relationships",
    "facts",
    "dimensions",
    "metrics",
    "materializations",
];

/// Suggest the closest known clause keyword for a near-miss word.
fn suggest_clause_keyword(word: &str) -> Option<&'static str> {
    let lower = word.to_ascii_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for &kw in CLAUSE_KEYWORDS {
        let dist = strsim::levenshtein(&lower, kw);
        if dist <= 3 {
            match best {
                Some((best_dist, _)) if dist < best_dist => best = Some((dist, kw)),
                None => best = Some((dist, kw)),
                _ => {}
            }
        }
    }
    best.map(|(_, kw)| kw)
}

/// Internal result of scanning a single clause from the AS-body.
#[derive(Debug)]
pub(super) struct ClauseBound<'a> {
    pub(super) keyword: &'static str,
    pub(super) content: &'a str, // text inside the matching parens
    // Offsets below include `base_offset`, so they are byte offsets in the
    // original query string (what `ParseError.position` expects), not relative
    // to the AS-body `text` this scanner receives.
    pub(super) content_offset: usize, // byte offset of content[0] in the original query
    pub(super) keyword_offset: usize, // byte offset of the keyword in the original query
}

/// Scan `text` (the text after "AS") at depth 0 to find clause headers of the form
/// `KEYWORD (...)`. Returns all found clause bounds in encounter order.
///
/// Validates:
/// - All keywords must be in `CLAUSE_KEYWORDS` (with "did you mean?" suggestion on error)
/// - No duplicate clauses
/// - Order must be TABLES -> RELATIONSHIPS? -> DIMENSIONS? -> METRICS?
/// - TABLES is required; at least one of DIMENSIONS or METRICS is required (that
///   they are not empty is checked by `parse_keyword_body`)
#[allow(clippy::too_many_lines)]
pub(super) fn find_clause_bounds<'a>(
    text: &'a str,
    base_offset: usize,
) -> Result<Vec<ClauseBound<'a>>, ParseError> {
    let mut cur = Cursor::new(text, base_offset);
    let mut bounds: Vec<ClauseBound<'a>> = Vec::new();
    let mut seen: Vec<&'static str> = Vec::new();

    while let Some(kw_tok) = cur.peek() {
        let word = cur.text(kw_tok);
        // A clause keyword is a bare-identifier token whose first byte is
        // ASCII-alphabetic. Anything else at the top level — a symbol, a quoted
        // `"..."`/`'...'` token, or a bare token led by a digit / `_` / a
        // non-ASCII byte (e.g. `★`, tokenized whole so its char renders
        // verbatim, no mojibake — P-14) — is an unexpected character.
        let first = word.chars().next().expect("lexer tokens are never empty");
        let is_keyword_word = matches!(kw_tok.kind, TokenKind::Ident { quoted: false })
            && first.is_ascii_alphabetic();
        if !is_keyword_word {
            return Err(cur.err(
                kw_tok.start,
                format!(
                    "Unexpected character '{first}' in AS body; expected a clause keyword (TABLES, RELATIONSHIPS, FACTS, DIMENSIONS, METRICS, MATERIALIZATIONS).",
                ),
            ));
        }

        let lower = word.to_ascii_lowercase();

        // Find matching static keyword
        let keyword: &'static str = if let Some(&kw) =
            CLAUSE_KEYWORDS.iter().find(|&&kw| kw == lower)
        {
            kw
        } else {
            let msg = if let Some(sug) = suggest_clause_keyword(word) {
                let sug_upper = sug.to_ascii_uppercase();
                format!("Unknown clause keyword '{word}'; did you mean '{sug_upper}'?")
            } else {
                format!(
                    "Unknown clause keyword '{word}'; expected one of TABLES, RELATIONSHIPS, FACTS, DIMENSIONS, METRICS, MATERIALIZATIONS.",
                )
            };
            return Err(cur.err(kw_tok.start, msg));
        };

        // Duplicate check
        if seen.contains(&keyword) {
            let kw_upper = keyword.to_ascii_uppercase();
            return Err(cur.err(
                kw_tok.start,
                format!("Duplicate clause keyword '{kw_upper}'."),
            ));
        }

        cur.bump(); // consume the keyword token

        // Expect '(' as the next token.
        if !cur.peek_is_symbol(b'(') {
            let kw_upper = keyword.to_ascii_uppercase();
            // Decode the real UTF-8 char for the message; '\0' at EOF keeps
            // the prior end-of-input sentinel (P-14).
            let found = char_at(text, cur.byte_pos()).unwrap_or('\0');
            return Err(cur.err(
                cur.byte_pos(),
                format!("Expected '(' after clause keyword '{kw_upper}', found '{found}'."),
            ));
        }
        // The `(` token — kept for the content offset and the unclosed-paren
        // error caret before `take_parens` consumes it.
        let open_tok = cur.peek().expect("peek_is_symbol guaranteed a '(' token");

        // Consume the balanced `(...)`. `take_parens` is quote-aware: a `)`
        // inside a `'...'`/`"..."` token cannot close the clause early (PA-6).
        let Some(content) = cur.take_parens() else {
            let kw_upper = keyword.to_ascii_uppercase();
            // Distinguish "a quote never closed, swallowing the rest of the
            // body" from a genuinely missing ')' — the quote-aware scan
            // otherwise reports the misleading unclosed-paren error for
            // unterminated quotes.
            let message = match cur.unterminated_tail() {
                Some(UnterminatedRegion::Ident) => {
                    format!("Unterminated quoted identifier in clause '{kw_upper}'.")
                }
                Some(UnterminatedRegion::String) => {
                    format!("Unterminated string literal in clause '{kw_upper}'.")
                }
                Some(UnterminatedRegion::Dollar) => {
                    format!("Unterminated dollar-quoted string in clause '{kw_upper}'.")
                }
                None => format!("Unclosed '(' in clause '{kw_upper}'."),
            };
            return Err(cur.err(open_tok.start, message));
        };
        let content_offset = cur.abs(open_tok.end);

        // Every scanner that later splits `content` must agree on where its
        // brackets close, which they only do when the brackets nest by kind.
        let inner = Cursor::new(content, content_offset);
        if let Some(m) = inner.bracket_mismatch() {
            let kw_upper = keyword.to_ascii_uppercase();
            let (at, message) = match m {
                BracketMismatch::Mismatched { closer, opener } => {
                    let found = char::from(inner.symbol(closer));
                    let want = char::from(closer_of(inner.symbol(opener)));
                    let open = char::from(inner.symbol(opener));
                    (
                        closer.start,
                        format!(
                            "Mismatched '{found}' in clause '{kw_upper}'; expected '{want}' to close the '{open}' before it."
                        ),
                    )
                }
                BracketMismatch::Unmatched(t) => (
                    t.start,
                    format!(
                        "Unmatched '{}' in clause '{kw_upper}'.",
                        char::from(inner.symbol(t))
                    ),
                ),
                // With several brackets open, the innermost is the one
                // reported, as compilers do.
                BracketMismatch::Unclosed(t) => (
                    t.start,
                    format!(
                        "Unclosed '{}' in clause '{kw_upper}'.",
                        char::from(inner.symbol(t))
                    ),
                ),
            };
            return Err(inner.err(at, message));
        }

        seen.push(keyword);
        bounds.push(ClauseBound {
            keyword,
            content,
            content_offset,
            keyword_offset: cur.abs(kw_tok.start),
        });
    }

    // Validate ordering: TABLES < RELATIONSHIPS < DIMENSIONS < METRICS
    let mut last_order: Option<usize> = None;
    for bound in &bounds {
        let order = CLAUSE_ORDER
            .iter()
            .position(|&k| k == bound.keyword)
            .unwrap_or(999);
        if last_order.is_some_and(|lo| order <= lo) {
            let kw_upper = bound.keyword.to_ascii_uppercase();
            return Err(ParseError {
                message: format!(
                    "Clause '{kw_upper}' appears out of order; clauses must appear as: TABLES, RELATIONSHIPS (optional), FACTS (optional), DIMENSIONS (optional), METRICS (optional), MATERIALIZATIONS (optional).",
                ),
                // T-7 (code-review 2026-07-11): point the caret at the
                // out-of-order clause keyword instead of dropping the position.
                position: Some(bound.keyword_offset),
            });
        }
        last_order = Some(order);
    }

    // Required: TABLES must be present
    if !seen.contains(&"tables") {
        return Err(ParseError {
            message: "Missing required clause 'TABLES'.".to_string(),
            position: None,
        });
    }

    // Required: at least one of DIMENSIONS or METRICS
    if !seen.contains(&"dimensions") && !seen.contains(&"metrics") {
        return Err(ParseError {
            message: "At least one of 'DIMENSIONS' or 'METRICS' is required.".to_string(),
            position: None,
        });
    }

    Ok(bounds)
}

#[cfg(test)]
mod tests {
    use super::{char_at, find_clause_bounds};

    /// P-14 (code-review 2026-07-11): a multibyte character at the top level
    /// of the AS body must appear verbatim in the error, not truncated to its
    /// UTF-8 lead byte. `bytes[i] as char` rendered `★` (0xE2 0x98 0x85) as
    /// the mojibake `'â'` (U+00E2), the char for byte 0xE2 alone.
    #[test]
    fn unexpected_multibyte_char_reported_verbatim() {
        let err = find_clause_bounds("★ DIMENSIONS (d AS x)", 0).unwrap_err();
        assert!(
            err.message.contains("'★'"),
            "error must contain the real character, got: {}",
            err.message
        );
        assert!(
            !err.message.contains('\u{00E2}'),
            "error must not contain the mojibake lead byte: {}",
            err.message
        );
        // Position is the byte offset of the character (unchanged).
        assert_eq!(err.position, Some(0));
    }

    /// P-14, the "expected '(' after keyword" arm: a multibyte character where
    /// a '(' is expected must also render verbatim.
    #[test]
    fn expected_paren_multibyte_char_reported_verbatim() {
        let err = find_clause_bounds("TABLES ★", 0).unwrap_err();
        assert!(
            err.message.contains("Expected '('") && err.message.contains("'★'"),
            "error must name the real character, got: {}",
            err.message
        );
        assert!(
            !err.message.contains('\u{00E2}'),
            "error must not contain the mojibake lead byte: {}",
            err.message
        );
    }

    /// `char_at` decodes the whole codepoint at a boundary offset and yields
    /// `None` past end-of-input (the EOF sentinel path for the paren arm).
    #[test]
    fn char_at_decodes_codepoint_and_handles_eof() {
        assert_eq!(char_at("★x", 0), Some('★'));
        assert_eq!(char_at("a★", 1), Some('★'));
        assert_eq!(char_at("abc", 3), None);
    }

    /// A plain ASCII unexpected character still renders correctly (no
    /// regression for the common case).
    #[test]
    fn unexpected_ascii_char_reported() {
        let err = find_clause_bounds("# DIMENSIONS (d AS x)", 0).unwrap_err();
        assert!(err.message.contains("'#'"), "got: {}", err.message);
    }

    /// The clause-keyword lists in the "unexpected character" and "unknown
    /// keyword" errors must name every keyword the scanner accepts —
    /// MATERIALIZATIONS was previously omitted from both, while the ordering
    /// error already listed it (Copilot review, #83).
    #[test]
    fn keyword_list_errors_include_materializations() {
        // "unexpected character" arm (leading non-alphabetic byte).
        let err = find_clause_bounds("# TABLES (o AS x)", 0).unwrap_err();
        assert!(
            err.message.contains("MATERIALIZATIONS"),
            "unexpected-char message must list MATERIALIZATIONS: {}",
            err.message
        );
        // "unknown clause keyword" arm. ZZZQQQ is >3 edits from every keyword
        // so it takes the no-suggestion branch that lists the keywords.
        let err = find_clause_bounds("ZZZQQQ (x)", 0).unwrap_err();
        assert!(
            err.message.contains("MATERIALIZATIONS"),
            "unknown-keyword message must list MATERIALIZATIONS: {}",
            err.message
        );
    }

    /// T-7 (code-review 2026-07-11): every 2-clause inversion (a clause
    /// written before one that must precede it) is rejected as out-of-order,
    /// and the error caret points at the offending (out-of-order) clause
    /// keyword rather than being dropped (`position: None` before the fix).
    /// Exhaustive over all 15 ordered pairs of the 6 clause keywords; empty
    /// `()` bodies isolate the ordering rule from per-clause content parsing.
    #[test]
    fn all_two_clause_order_inversions_rejected_with_caret() {
        let order = [
            "tables",
            "relationships",
            "facts",
            "dimensions",
            "metrics",
            "materializations",
        ];
        for (i, &earlier) in order.iter().enumerate() {
            for &later in &order[i + 1..] {
                // Write `later` first, then `earlier` — an inversion.
                let body = format!("{later} () {earlier} ()");
                let err = find_clause_bounds(&body, 0).unwrap_err();
                assert!(
                    err.message.contains("out of order"),
                    "`{body}` must be rejected as out of order, got: {}",
                    err.message
                );
                // Caret points at the out-of-order (`earlier`) keyword, which
                // begins right after "{later} () ".
                let expected = later.len() + 4;
                assert_eq!(
                    err.position,
                    Some(expected),
                    "caret for `{body}` should point at the out-of-order '{earlier}' keyword"
                );
            }
        }
    }

    /// T-7: the out-of-order caret is anchored in the original query via
    /// `base_offset`, matching every other position this scanner reports.
    #[test]
    fn out_of_order_caret_honours_base_offset() {
        let base = 100;
        let err = find_clause_bounds("metrics () dimensions ()", base).unwrap_err();
        assert!(err.message.contains("out of order"), "{}", err.message);
        // "dimensions" starts at byte 11 within the body ("metrics () " == 11).
        assert_eq!(err.position, Some(base + 11));
    }

    /// Fuzz #277 / #285: every scanner below the clause split counts `(`, `[`
    /// and `{` as one depth, while `take_parens` counts only `(`/`)`. A clause
    /// whose brackets don't nest by kind therefore splits one way on the first
    /// parse and another way once rendered back, so the round trip drifts.
    /// Each closer must close the opener of its own kind.
    #[test]
    fn a_closer_of_the_wrong_kind_is_refused() {
        let text = "TABLES (o AS orders) METRICS (o.x AS SUM(o.a]))";
        let err = find_clause_bounds(text, 0).unwrap_err();
        assert_eq!(
            err.message,
            "Mismatched ']' in clause 'METRICS'; expected ')' to close the '(' before it."
        );
        assert_eq!(err.position, Some(text.find(']').unwrap()));
    }

    #[test]
    fn a_closer_with_nothing_open_is_refused() {
        let text = "TABLES (o AS orders) METRICS (o.x AS o.a})";
        let err = find_clause_bounds(text, 0).unwrap_err();
        assert_eq!(err.message, "Unmatched '}' in clause 'METRICS'.");
        assert_eq!(err.position, Some(text.find('}').unwrap()));
    }

    #[test]
    fn an_opener_left_open_is_refused() {
        let text = "TABLES (o AS orders) DIMENSIONS (o.d AS o.a[1)";
        let err = find_clause_bounds(text, 0).unwrap_err();
        assert_eq!(err.message, "Unclosed '[' in clause 'DIMENSIONS'.");
        assert_eq!(err.position, Some(text.find('[').unwrap()));
    }

    /// The control: brackets of every kind, nested by kind, and brackets
    /// inside strings, quoted names and dollar-quoted strings, are accepted.
    #[test]
    fn brackets_nested_by_kind_are_accepted() {
        let text = "TABLES (o AS orders) METRICS (o.x AS SUM(o.a[1]) + {'k': (1)}.k, \
                    o.y AS COUNT(\"a]\") + LENGTH(']}') + LENGTH($$)[$$))";
        let bounds = find_clause_bounds(text, 0).unwrap();
        assert_eq!(bounds.len(), 2);
    }
}
