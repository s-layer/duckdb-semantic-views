//! Window-metric OVER clause parsing and the shared ORDER-BY modifier loop.

use super::cursor::Cursor;
use super::lexer::TokenKind;
use super::scan::{is_ident_continuation, is_quoting_balanced};
use super::split_at_depth0_commas;
use crate::errors::ParseError;
use crate::ident::find_identifier_end;
use crate::model::{NullsOrder, SortOrder, WindowOrderBy, WindowSpec};

/// The frame-clause lead keywords (`ROWS` / `RANGE` / `GROUPS`).
const FRAME_KEYWORDS: [&str; 3] = ["ROWS", "RANGE", "GROUPS"];

/// Parse a window function OVER clause from the expression text.
///
/// Detects `FUNC(metric[, args...]) OVER (PARTITION BY EXCLUDING d1, d2 [ORDER BY ...] [frame])`.
/// Returns the raw expression and an optional parsed `WindowSpec`.
///
/// §6.1 (phase 5): parsed on the shared [`Cursor`]/lexer. `OVER` is the first
/// depth-0 keyword token (outside the function-call parens); the function-call
/// and OVER-body parentheses are consumed with `take_parens`; and the OVER
/// content's `PARTITION BY [EXCLUDING]` / `ORDER BY` / frame boundaries are
/// keyword tokens. The P-3 hardening (ORDER-must-be-adjacent-BY, no silent
/// frame, frame-keyword-name rejection) is preserved exactly.
pub(super) fn parse_window_over_clause(
    expr: &str,
    base_offset: usize,
) -> Result<(String, Option<WindowSpec>), ParseError> {
    let expr = expr.trim();
    let mut cur = Cursor::new(expr, base_offset);

    // OVER keyword at depth-0 (outside the window-function call's parentheses).
    let Some(over_tok) = cur.find_kw_depth0("OVER") else {
        return Ok((expr.to_string(), None));
    };
    let func_part = expr[..over_tok.start].trim();

    // The OVER body: `(...)` immediately after OVER.
    cur.advance_past_byte(over_tok.end);
    if !cur.peek_is_symbol(b'(') {
        return Err(ParseError {
            message: format!("Expected '(' after OVER in expression '{expr}'."),
            position: Some(base_offset + over_tok.end),
        });
    }
    let Some(over_content) = cur.take_parens() else {
        return Err(ParseError {
            message: format!("Unclosed '(' after OVER in expression '{expr}'."),
            position: Some(base_offset + over_tok.end),
        });
    };

    // PARSE-10 (code-review 2026-08-08): everything after the OVER body's `)`
    // used to be dropped on the floor. Emission rebuilds the SQL from the
    // `WindowSpec` alone (`expand::window`), so `AVG(w) OVER (...) + 100`
    // computed without the `+ 100` while `GET_DDL` round-tripped the text that
    // still had it — definition and behaviour disagreeing, with no diagnostic.
    // Same "reject, don't discard" rule as the USING / NON ADDITIVE BY residues
    // (P-2 / F-3). Composition around a window function is rejected rather than
    // supported — see TECH-DEBT "window-metric composition".
    if let Some(residue_tok) = cur.peek() {
        let residue = expr[residue_tok.start..].trim_end();
        return Err(ParseError {
            message: format!(
                "Unexpected text '{residue}' after the OVER clause in expression '{expr}'. \
                 A window metric must be exactly FUNC(args) OVER (...); combining it with \
                 other expressions is not supported."
            ),
            position: Some(base_offset + residue_tok.start),
        });
    }

    // The function call before OVER: `FUNC(inner_metric[, extra_args...])`.
    let mut fcur = Cursor::new(func_part, base_offset);
    let Some(fparen) = fcur.find_symbol(b'(') else {
        return Err(ParseError {
            message: format!(
                "Window function before OVER must have parenthesized arguments: '{func_part}'."
            ),
            position: Some(base_offset),
        });
    };
    let window_function = func_part[..fparen.start].trim().to_string();
    // PARSE-10: the text before the first `(` became `window_function`
    // VERBATIM, so `1 + AVG(w) OVER (...)` stored the function name
    // `"1 + AVG"` and emitted `1 + AVG(...) OVER (...)`-shaped nonsense (or, in
    // the qualified-metric paths, an arbitrary prefix silently reinterpreted).
    // The name must be exactly one identifier chain.
    if !is_identifier_chain(&window_function) {
        return Err(ParseError {
            message: format!(
                "Window function before OVER must be a plain function call, found \
                 '{func_part}'. A window metric must be exactly FUNC(args) OVER (...); \
                 combining it with other expressions is not supported."
            ),
            position: Some(base_offset),
        });
    }
    let paren_start = fparen.start;
    fcur.advance_past_byte(paren_start);
    let Some(func_args_content) = fcur.take_parens() else {
        return Err(ParseError {
            message: format!("Unclosed '(' in window function call '{func_part}'."),
            position: Some(base_offset + paren_start),
        });
    };

    // Split function arguments: first is inner_metric, rest are extra_args
    let func_args: Vec<&str> = split_at_depth0_commas(func_args_content)?
        .into_iter()
        .map(|(_, s)| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if func_args.is_empty() {
        return Err(ParseError {
            message: format!("Window function '{window_function}' has no arguments."),
            position: Some(base_offset),
        });
    }
    let inner_metric = func_args[0].to_string();
    let extra_args: Vec<String> = func_args[1..]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();

    // Parse OVER clause content: PARTITION BY [EXCLUDING] ..., ORDER BY ..., frame clause
    let (excluding_dims, partition_dims, order_by, frame_clause) =
        parse_over_content(over_content, base_offset + over_tok.start)?;

    Ok((
        expr.to_string(),
        Some(WindowSpec {
            window_function,
            inner_metric,
            extra_args,
            excluding_dims,
            partition_dims,
            order_by,
            frame_clause,
        }),
    ))
}

/// Is `text` exactly ONE identifier chain — a bare or double-quoted name,
/// optionally qualified (`avg`, `main.avg`, `"my func"`, `main."my func"`)?
///
/// The shape test for a window function's name (PARSE-10). Anything else — an
/// operator, a literal, a second call, a leading term — makes the text an
/// *expression*, and this parser only models a bare `FUNC(args) OVER (...)`.
/// Whitespace between the name and its argument parens is fine (`AVG (x)` is
/// legal `DuckDB`); what is rejected is another TOKEN sitting in the name
/// position.
fn is_identifier_chain(text: &str) -> bool {
    let mut cur = Cursor::new(text, 0);
    loop {
        if !matches!(cur.bump(), Some(t) if matches!(t.kind, TokenKind::Ident { .. })) {
            return false;
        }
        if cur.peek().is_none() {
            return true;
        }
        if !cur.peek_is_symbol(b'.') {
            return false;
        }
        cur.bump();
    }
}

/// Parsed components of an OVER clause.
/// (`excluding_dims`, `partition_dims`, `order_by`, `frame_clause`)
type OverContent = (Vec<String>, Vec<String>, Vec<WindowOrderBy>, Option<String>);

/// Split a comma-separated dimension list (already sliced from the source),
/// trimming and dropping empties. A stray/leading comma is rejected by
/// [`split_at_depth0_commas`] (T-13).
fn split_dim_list(text: &str) -> Result<Vec<String>, ParseError> {
    Ok(split_at_depth0_commas(text)?
        .into_iter()
        .map(|(_, s)| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// Parse the comma-separated `dim [ASC|DESC] [NULLS FIRST|LAST]` entries of an
/// OVER `ORDER BY`. `dim` is captured identifier-aware (quoted / dotted names
/// survive); the modifier suffix is whitespace-tokenised and resolved by the
/// shared [`parse_order_by_modifiers`]. Entry errors anchor at
/// `base_offset + <entry offset>`.
fn parse_order_by_entries(
    order_text: &str,
    base_offset: usize,
) -> Result<Vec<WindowOrderBy>, ParseError> {
    let mut order_by = Vec::new();
    for (start, entry_text) in split_at_depth0_commas(order_text)? {
        let entry_text = entry_text.trim();
        if entry_text.is_empty() {
            continue;
        }
        let name_end = find_identifier_end(entry_text, /* allow_paren = */ false);
        if name_end == 0 {
            continue;
        }
        if !is_quoting_balanced(&entry_text[..name_end]) {
            return Err(ParseError {
                message: format!(
                    "Unterminated quoted identifier in OVER ORDER BY entry '{entry_text}'."
                ),
                position: Some(base_offset + start),
            });
        }
        let dim_name = entry_text[..name_end].trim().to_string();
        let suffix = entry_text[name_end..].trim();
        let parts: Vec<&str> = suffix.split_whitespace().collect();
        let (sort, nulls) = parse_order_by_modifiers(
            &parts,
            OrderModifierContext::OverOrderBy { entry_text },
            base_offset + start,
        )?;
        order_by.push(WindowOrderBy {
            expr: dim_name,
            order: sort,
            nulls,
        });
    }
    Ok(order_by)
}

/// Refuse a `PARTITION BY` / `PARTITION BY EXCLUDING` naming no dimension. An
/// empty list would be stored as no partition clause, which means "every result
/// row". SQL's window grammar needs at least one expression after
/// `PARTITION BY`, so the empty list is refused rather than read that way.
fn require_partition_dims(
    dims: &[String],
    excluding: bool,
    base_offset: usize,
) -> Result<(), ParseError> {
    if !dims.is_empty() {
        return Ok(());
    }
    let clause = if excluding {
        "PARTITION BY EXCLUDING"
    } else {
        "PARTITION BY"
    };
    Err(ParseError {
        message: format!(
            "{clause} names no dimension in OVER clause. Name at least one \
             dimension, or leave the clause out to run the window over every \
             result row."
        ),
        position: Some(base_offset),
    })
}

/// Parse the content inside the OVER (...) clause:
/// `[PARTITION BY [EXCLUDING] dims] [ORDER BY entries] [frame]`.
///
/// All boundaries are keyword tokens on a [`Cursor`] scoped to `content`. The
/// P-3 diagnostics anchor at `base_offset` (as before); ORDER BY entry errors
/// at `base_offset + <entry offset>`.
fn parse_over_content(content: &str, base_offset: usize) -> Result<OverContent, ParseError> {
    let content = content.trim();
    if content.is_empty() {
        return Ok((vec![], vec![], vec![], None));
    }

    let mut cur = Cursor::new(content, base_offset);
    let mut excluding_dims: Vec<String> = Vec::new();
    let mut partition_dims: Vec<String> = Vec::new();

    // `tail_start` is where the post-PARTITION-BY region begins in `content`
    // (0 when there is no PARTITION BY).
    let mut tail_start = 0usize;

    if let Some((partition_tok, by_tok)) = cur.find_kw_seq(&["PARTITION", "BY"]) {
        // F-2 (code-review 2026-07-16): `find_kw_seq` matches PARTITION BY
        // anywhere in the OVER body, so any tokens before it — including a
        // misplaced `ORDER BY` — were silently discarded (the window metric
        // then computed an unordered aggregate). PARTITION BY is the first
        // clause of a window spec, so nothing may precede it.
        let before_partition = content[..partition_tok.start].trim();
        if !before_partition.is_empty() {
            return Err(ParseError {
                message: format!(
                    "Unexpected text '{before_partition}' before PARTITION BY in OVER clause."
                ),
                position: Some(base_offset),
            });
        }
        cur.advance_past_byte(by_tok.end);
        // Optional EXCLUDING.
        let excluding = cur.peek().is_some_and(|t| cur.is_kw(t, "EXCLUDING"));
        if excluding {
            let ex_end = cur.peek().map_or(by_tok.end, |t| t.end);
            cur.advance_past_byte(ex_end);
        }
        let dims_start = cur.byte_pos();
        // Dims run up to ORDER BY, else a frame keyword, else end. ORDER is
        // PREFERRED over frame keywords (matching the old
        // `find_keyword_ci("ORDER").or_else(find_frame_start)`), so a dim named
        // like a frame keyword (`groups`/`rows`/`range`) followed by ORDER BY
        // stays a dim rather than being taken as the frame boundary (PR #104
        // review).
        let boundary = cur
            .find_kw("ORDER")
            .or_else(|| cur.find_any_kw(&FRAME_KEYWORDS));
        let dims_end = boundary.map_or(content.len(), |t| t.start);
        let dims = split_dim_list(content[dims_start..dims_end].trim())?;
        require_partition_dims(&dims, excluding, base_offset)?;
        if excluding {
            excluding_dims = dims;
        } else {
            partition_dims = dims;
        }
        cur.advance_past_byte(dims_end);
        tail_start = dims_end;
    }

    // ORDER BY, then the frame region.
    let mut order_by: Vec<WindowOrderBy> = Vec::new();
    let frame_region: &str = if let Some(order_tok) = cur.find_kw("ORDER") {
        // P-3: no stray text between the dims/start and ORDER.
        let before = content[tail_start..order_tok.start].trim();
        if !before.is_empty() {
            return Err(ParseError {
                message: format!("Unexpected text '{before}' before ORDER BY in OVER clause."),
                position: Some(base_offset),
            });
        }
        // P-3: ORDER must be immediately followed by BY (no junk between, and
        // absent-BY is an error rather than a silent frame clause).
        cur.advance_past_byte(order_tok.end);
        let by_tok = match cur.peek() {
            Some(t) if cur.is_kw(t, "BY") => t,
            _ => {
                return Err(ParseError {
                    message: "Expected BY immediately after ORDER in OVER clause.".to_string(),
                    position: Some(base_offset),
                });
            }
        };
        cur.advance_past_byte(by_tok.end);

        // ORDER BY entries run up to a frame keyword (or end).
        let frame_tok = cur.find_any_kw(&FRAME_KEYWORDS);
        let order_end = frame_tok.map_or(content.len(), |t| t.start);
        let order_text = content[by_tok.end..order_end].trim();
        order_by = parse_order_by_entries(order_text, base_offset)?;

        // P-3: ORDER BY must yield at least one parsed entry (an unquoted
        // reference named like a frame keyword otherwise leaves zero entries
        // and a bogus frame with no diagnostics).
        if order_by.is_empty() {
            let after_order_by = content[by_tok.end..].trim();
            return Err(ParseError {
                message: format!(
                    "Expected column reference after ORDER BY in OVER clause, found '{after_order_by}'. (Quote the reference if it is named like a frame keyword.)"
                ),
                position: Some(base_offset),
            });
        }

        frame_tok.map_or("", |t| content[t.start..].trim())
    } else {
        // No ORDER BY: the tail is the frame clause (or junk).
        content[tail_start..].trim()
    };

    // P-3: validate the frame region actually starts with a frame keyword —
    // previously any residue was stored verbatim as `frame_clause`.
    let frame_clause = if frame_region.is_empty() {
        None
    } else {
        let upper_rem = frame_region.to_ascii_uppercase();
        let is_frame = FRAME_KEYWORDS.iter().any(|kw| {
            let kw = kw.as_bytes();
            upper_rem.as_bytes().starts_with(kw)
                && (upper_rem.len() == kw.len()
                    || !is_ident_continuation(upper_rem.as_bytes()[kw.len()]))
        });
        if !is_frame {
            return Err(ParseError {
                message: format!(
                    "Expected frame clause starting with ROWS, RANGE, or GROUPS in OVER clause, found '{frame_region}'."
                ),
                position: Some(base_offset),
            });
        }
        Some(frame_region.to_string())
    };

    Ok((excluding_dims, partition_dims, order_by, frame_clause))
}

/// Sort-modifier parsing context: which clause the `ASC|DESC|NULLS FIRST|LAST`
/// modifier suffix belongs to. The two call sites share the token loop below
/// but differ deliberately in two behaviours (kept byte-for-byte identical to
/// the pre-extraction loops):
///
/// - **Unknown tokens**: a hard `ParseError` for NON ADDITIVE BY entries; a
///   silent stop for OVER ORDER BY entries (trailing text belongs to the
///   frame clause).
/// - **DESC nulls default**: NON ADDITIVE BY applies the `DESC => NULLS
///   FIRST` default after the loop, only when no explicit NULLS was given
///   (so `NULLS LAST DESC` keeps LAST); OVER ORDER BY sets NULLS FIRST the
///   moment DESC is seen (matches DuckDB/Snowflake), so a later explicit
///   NULLS wins but an earlier one is overridden.
#[derive(Clone, Copy)]
pub(super) enum OrderModifierContext<'a> {
    /// `NON ADDITIVE BY (dim [ASC|DESC] [NULLS FIRST|LAST], ...)` entry.
    NonAdditiveBy,
    /// `OVER (... ORDER BY dim [ASC|DESC] [NULLS FIRST|LAST] ...)` entry;
    /// carries the entry text for error messages.
    OverOrderBy { entry_text: &'a str },
}

impl OrderModifierContext<'_> {
    /// Error message when the token after NULLS is neither FIRST nor LAST.
    fn nulls_bad_follower_message(self, follower: &str) -> String {
        match self {
            Self::NonAdditiveBy => {
                format!("Expected FIRST or LAST after NULLS, got '{follower}'")
            }
            Self::OverOrderBy { entry_text } => {
                format!("Expected FIRST or LAST after NULLS in OVER ORDER BY entry '{entry_text}'.")
            }
        }
    }

    /// Error message when NULLS is the final token.
    fn nulls_missing_message(self) -> String {
        match self {
            Self::NonAdditiveBy => "Expected FIRST or LAST after NULLS".to_string(),
            Self::OverOrderBy { entry_text } => {
                format!("Expected FIRST or LAST after NULLS in OVER ORDER BY entry '{entry_text}'.")
            }
        }
    }
}

/// Parse a whitespace-tokenised `[ASC|DESC] [NULLS FIRST|LAST]` modifier
/// suffix. This is the ONE shared implementation of the modifier loop that
/// previously appeared in near-identical form in the NON ADDITIVE BY dim
/// parser and the OVER ORDER BY parser (ST-3, code-review 2026-07-02).
/// Returns the resolved `(SortOrder, NullsOrder)` pair. Defaults are
/// ASC / NULLS LAST; the DESC => NULLS FIRST default and the unknown-token
/// policy vary by `context` (see `OrderModifierContext`).
pub(super) fn parse_order_by_modifiers(
    parts: &[&str],
    context: OrderModifierContext<'_>,
    err_position: usize,
) -> Result<(SortOrder, NullsOrder), ParseError> {
    let mut order = SortOrder::Asc;
    let mut nulls = NullsOrder::Last;
    let mut has_explicit_nulls = false;
    let mut i = 0;
    while i < parts.len() {
        match parts[i].to_ascii_uppercase().as_str() {
            "ASC" => {
                order = SortOrder::Asc;
                i += 1;
            }
            "DESC" => {
                order = SortOrder::Desc;
                if matches!(context, OrderModifierContext::OverOrderBy { .. }) {
                    // DESC defaults to NULLS FIRST (matches DuckDB/Snowflake)
                    nulls = NullsOrder::First;
                }
                i += 1;
            }
            "NULLS" => {
                if i + 1 < parts.len() {
                    match parts[i + 1].to_ascii_uppercase().as_str() {
                        "FIRST" => {
                            nulls = NullsOrder::First;
                            has_explicit_nulls = true;
                            i += 2;
                        }
                        "LAST" => {
                            nulls = NullsOrder::Last;
                            has_explicit_nulls = true;
                            i += 2;
                        }
                        _ => {
                            return Err(ParseError {
                                message: context.nulls_bad_follower_message(parts[i + 1]),
                                position: Some(err_position),
                            });
                        }
                    }
                } else {
                    return Err(ParseError {
                        message: context.nulls_missing_message(),
                        position: Some(err_position),
                    });
                }
            }
            other => match context {
                OrderModifierContext::NonAdditiveBy => {
                    return Err(ParseError {
                        message: format!(
                            "Unexpected token '{other}' in NON ADDITIVE BY dimension entry",
                        ),
                        position: Some(err_position),
                    });
                }
                OrderModifierContext::OverOrderBy { .. } => {
                    // Unexpected token, stop parsing ORDER BY modifiers
                    break;
                }
            },
        }
    }
    // Adjust default nulls based on sort order (DESC defaults to NULLS FIRST)
    // Only if user did not explicitly specify NULLS
    if matches!(context, OrderModifierContext::NonAdditiveBy)
        && !has_explicit_nulls
        && order == SortOrder::Desc
    {
        nulls = NullsOrder::First;
    }
    Ok((order, nulls))
}

// The `trimmed_bounds` helper and its tests were retired with the §6.1 OVER
// migration: dims/order regions are now sliced between keyword TOKEN offsets
// (`content[dims_start..dims_end]`), which are exact by construction, so the
// uppercase-twin offset bookkeeping the helper served is gone.

#[cfg(test)]
mod tests {
    use super::parse_window_over_clause;

    /// Parse at a non-zero base offset so every caret assertion below also
    /// proves the error position is anchored in the ORIGINAL query, not in the
    /// re-sliced expression.
    const BASE: usize = 100;

    fn err(expr: &str) -> (String, Option<usize>) {
        match parse_window_over_clause(expr, BASE) {
            Ok((_, spec)) => panic!("expected a ParseError, got Ok({spec:?}) for {expr:?}"),
            Err(e) => (e.message, e.position),
        }
    }

    // PARSE-10 (code-review 2026-08-08): text AFTER the OVER body's `)` was
    // never inspected, so `+ 100` vanished — the stored definition text kept it
    // while the emitted SQL (rebuilt from the WindowSpec) dropped it, and the
    // metric silently returned a number 100 too small.
    #[test]
    fn trailing_arithmetic_after_the_over_clause_is_rejected() {
        let (msg, pos) = err("AVG(w) OVER (PARTITION BY d) + 100");
        assert!(msg.contains("+ 100"), "{msg}");
        assert!(
            msg.contains("Unexpected text") && msg.contains("after the OVER clause"),
            "{msg}"
        );
        // Caret at the `+`, which is byte 29 of the expression.
        assert_eq!(pos, Some(BASE + 29));
    }

    // A `PARTITION BY` (or `PARTITION BY EXCLUDING`) naming no dimension used
    // to be stored as no partition clause at all, which since Phase 7 means
    // "every result row". SQL's window grammar needs at least one expression
    // after `PARTITION BY`, so an empty list is refused rather than quietly
    // meaning "every result row".
    #[test]
    fn an_empty_partition_by_list_is_rejected() {
        let (msg, pos) = err("SUM(w) OVER (PARTITION BY ORDER BY d)");
        assert!(msg.contains("PARTITION BY names no dimension"), "{msg}");
        // Anchored at `OVER` (byte 7), like the other OVER-clause diagnostics.
        assert_eq!(pos, Some(BASE + 7));
    }

    #[test]
    fn an_empty_partition_by_excluding_list_is_rejected() {
        let (msg, _) = err("SUM(w) OVER (PARTITION BY EXCLUDING ORDER BY d)");
        assert!(
            msg.contains("PARTITION BY EXCLUDING names no dimension"),
            "{msg}"
        );
        let (msg, _) = err("SUM(w) OVER (PARTITION BY EXCLUDING)");
        assert!(
            msg.contains("PARTITION BY EXCLUDING names no dimension"),
            "{msg}"
        );
        let (msg, _) = err("SUM(w) OVER (PARTITION BY)");
        assert!(msg.contains("PARTITION BY names no dimension"), "{msg}");
    }

    // PARSE-10: `func_part` was everything before OVER and its text before the
    // first `(` became `window_function` verbatim, so this stored the function
    // name `"1 + AVG"`.
    #[test]
    fn an_expression_prefix_before_the_window_function_is_rejected() {
        let (msg, pos) = err("1 + AVG(w) OVER (PARTITION BY d)");
        assert!(msg.contains("must be a plain function call"), "{msg}");
        assert_eq!(pos, Some(BASE), "caret at the start of the function part");
    }

    // PARSE-10: a second `FUNC(...) OVER (...)` term was dropped wholesale —
    // only the first OVER is parsed and the rest was residue.
    #[test]
    fn a_second_over_term_is_rejected() {
        let (msg, _) = err("AVG(w) OVER (PARTITION BY d) - SUM(w) OVER (PARTITION BY d)");
        assert!(msg.contains("Unexpected text"), "{msg}");
        assert!(msg.contains("SUM(w)"), "{msg}");
    }

    /// Control: a plain window metric still parses, with the spec intact.
    #[test]
    fn a_plain_window_metric_still_parses() {
        let (expr, spec) =
            parse_window_over_clause("AVG(qty) OVER (PARTITION BY EXCLUDING d ORDER BY t)", BASE)
                .expect("valid window metric");
        assert_eq!(expr, "AVG(qty) OVER (PARTITION BY EXCLUDING d ORDER BY t)");
        let spec = spec.expect("a WindowSpec");
        assert_eq!(spec.window_function, "AVG");
        assert_eq!(spec.inner_metric, "qty");
        assert_eq!(spec.excluding_dims, vec!["d".to_string()]);
        assert_eq!(spec.order_by.len(), 1);
    }

    /// Control: the accepted function-part shapes — a qualified name, a quoted
    /// name, extra arguments, and whitespace before the argument parens (which
    /// `DuckDB` accepts) — must all keep parsing. The new rule is "one identifier
    /// chain then the arg list", not "no whitespace anywhere".
    #[test]
    fn qualified_quoted_and_spaced_function_names_still_parse() {
        for (expr, want_fn) in [
            ("main.avg(qty) OVER (PARTITION BY d)", "main.avg"),
            ("\"my func\"(qty) OVER (PARTITION BY d)", "\"my func\""),
            ("AVG (qty) OVER (PARTITION BY d)", "AVG"),
            ("lead(qty, 1) OVER (PARTITION BY d ORDER BY t)", "lead"),
        ] {
            let (_, spec) = parse_window_over_clause(expr, BASE)
                .unwrap_or_else(|e| panic!("{expr:?} must parse, got {}", e.message));
            assert_eq!(spec.expect("a WindowSpec").window_function, want_fn);
        }
    }

    /// Control: an expression with no OVER at all is returned untouched (this
    /// is the non-window metric path — every ordinary metric expression flows
    /// through here and must NOT be subjected to the function-call shape rule).
    #[test]
    fn an_expression_without_over_is_untouched() {
        let (expr, spec) = parse_window_over_clause("SUM(revenue) - SUM(cost) + 100", BASE)
            .expect("a non-window expression is not a window metric");
        assert_eq!(expr, "SUM(revenue) - SUM(cost) + 100");
        assert!(spec.is_none());
    }

    /// Control: the pre-existing "no parenthesized arguments" error still fires
    /// for a bare name before OVER.
    #[test]
    fn a_window_function_without_arguments_is_still_rejected() {
        let (msg, _) = err("row_number OVER (PARTITION BY d)");
        assert!(msg.contains("parenthesized arguments"), "{msg}");
    }
}
