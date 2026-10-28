//! METRICS clause parsing, including USING and NON ADDITIVE BY.
//!
//! §6.1 (phase 4, code-review 2026-07-11): the structural scan — `AS`, `USING`,
//! `NON ADDITIVE BY`, and the `alias.name` dot split — is parsed on the shared
//! [`Cursor`]/lexer. Keyword and delimiter detection is now quote-aware by
//! construction (a `USING`/`.`/`(` inside a `"quoted"`/`'string'` token is
//! inert), replacing the `find_keyword_ci` / `find_live_byte` /
//! `extract_paren_content` scans. The OVER/window sub-parser
//! ([`parse_window_over_clause`], migrated separately) and the NON ADDITIVE BY
//! dimension list ([`parse_non_additive_dims`]) are delegated unchanged, as is
//! the trailing COMMENT / WITH SYNONYMS region.

use super::annotations::{parse_leading_access_modifier, parse_trailing_annotations};
use super::cursor::Cursor;
use super::scan::{is_quoting_balanced, unterminated_quote_error};
use super::window::{parse_order_by_modifiers, parse_window_over_clause, OrderModifierContext};
use super::{split_at_depth0_commas, ParsedMetric};
use crate::errors::ParseError;
use crate::ident::find_identifier_end;
use crate::model::NonAdditiveDim;
use crate::util::byte_offset_within;

/// Parse the content inside METRICS (...) supporting both qualified and unqualified entries.
///
/// Qualified entries have the form: `alias.name AS expr` (base metric).
/// Qualified entries may include: `alias.name USING (rel1, rel2) AS expr` (Phase 32).
/// Unqualified entries have the form: `name AS expr` (derived metric).
///
/// Returns one [`ParsedMetric`] per entry, where `source_alias` is `Some(alias)`
/// for qualified entries and `None` for unqualified (derived) entries;
/// `using_relationships` is empty when there is no USING clause; and comment,
/// synonyms, and access are parsed from trailing annotations / a leading keyword.
pub(crate) fn parse_metrics_clause(
    body: &str,
    base_offset: usize,
) -> Result<Vec<ParsedMetric>, ParseError> {
    if body.trim().is_empty() {
        return Ok(vec![]);
    }

    let entries = split_at_depth0_commas(body)?;
    let mut result = Vec::new();

    for (entry_start, entry) in entries {
        let entry_offset = base_offset + entry_start;
        let parsed = parse_single_metric_entry(entry, entry_offset)?;
        result.push(parsed);
    }

    Ok(result)
}

/// Parse the dimension entries inside a NON ADDITIVE BY (...) clause.
/// Each entry: `dim_name [ASC|DESC] [NULLS FIRST|LAST]`
///
/// Phase 68 Plan 03 (B1) / TECH-DEBT #25: `dim_name` is captured via
/// identifier-aware tokenisation (`find_identifier_end`) so quoted identifiers
/// containing literal whitespace AND dotted paths (`table.col`, D-08) survive
/// intact. The modifier suffix (`ASC|DESC|NULLS FIRST|LAST`) is then
/// `split_whitespace`-tokenised since the suffix has no quoted identifiers.
fn parse_non_additive_dims(
    content: &str,
    base_offset: usize,
) -> Result<Vec<NonAdditiveDim>, ParseError> {
    let entries = split_at_depth0_commas(content)?;
    let mut result = Vec::new();
    for (start, entry_text) in entries {
        let entry_text = entry_text.trim();
        if entry_text.is_empty() {
            continue; // trailing comma
        }
        // Phase 68 B1: identifier-aware capture of dim_name. `allow_paren=false`
        // because NAB entries have no parens inside identifiers.
        let name_end = find_identifier_end(entry_text, /* allow_paren = */ false);
        if name_end == 0 {
            return Err(ParseError {
                message: "Empty dimension in NON ADDITIVE BY clause".to_string(),
                position: Some(base_offset + start),
            });
        }
        // Phase 68 B1: reject unterminated quoted identifiers — mirrors the
        // TABLES-clause A4 check. `find_identifier_end` saturates at
        // `input.len()` on an unterminated `"`, so without this guard the
        // malformed name would flow downstream.
        if !is_quoting_balanced(&entry_text[..name_end]) {
            return Err(ParseError {
                message: format!(
                    "Unterminated quoted identifier in NON ADDITIVE BY dimension entry '{entry_text}'."
                ),
                position: Some(base_offset + start),
            });
        }
        let dim_name = entry_text[..name_end].trim().to_string();
        let suffix = entry_text[name_end..].trim();
        let parts: Vec<&str> = suffix.split_whitespace().collect();
        let (order, nulls) = parse_order_by_modifiers(
            &parts,
            OrderModifierContext::NonAdditiveBy,
            base_offset + start,
        )?;
        result.push(NonAdditiveDim {
            dimension: dim_name,
            order,
            nulls,
        });
    }
    Ok(result)
}

/// Parse one METRICS entry: either `alias.name [USING (...)] [NON ADDITIVE BY (...)] AS expr` (qualified)
/// or `name AS expr` (derived).
///
/// Phase 32: If a USING clause is present, it must be on a qualified entry (has dot).
/// USING on a derived metric (no dot) produces a `ParseError`.
/// Phase 47: If a NON ADDITIVE BY clause is present, it must be on a qualified entry (has dot).
/// Phase 48: If an OVER clause is present, it must be on a qualified entry (has dot).
#[allow(clippy::too_many_lines)]
fn parse_single_metric_entry(entry: &str, entry_offset: usize) -> Result<ParsedMetric, ParseError> {
    let entry = entry.trim();

    // Unterminated quoting swallows the rest of the entry under the
    // quote-aware scanners — reject it up front with a precise error.
    if let Some(noun) = unterminated_quote_error(entry) {
        return Err(ParseError {
            message: format!("{noun} in metric entry '{entry}'."),
            position: Some(entry_offset),
        });
    }

    // Phase 43: Check for leading PRIVATE/PUBLIC keyword. The cursor base
    // includes the modifier's byte offset so token-position carets are accurate.
    let (access, entry_after_access) = parse_leading_access_modifier(entry);
    let cur = Cursor::new(
        entry_after_access,
        entry_offset + byte_offset_within(entry, entry_after_access),
    );

    // Split the entry at the first `AS` keyword token AT DEPTH 0 — an `AS`
    // nested inside a preceding `USING (a AS b)` list is inert, so the split
    // lands on the structural `AS` before the expression (#103 review).
    let Some(as_tok) = cur.find_kw_depth0("AS") else {
        return Err(ParseError {
            message: format!(
                "Expected 'AS' keyword in metric entry '{entry}'. Form: 'alias.name AS expr' or 'name AS expr'.",
            ),
            position: Some(entry_offset),
        });
    };
    let before_as = entry_after_access[..as_tok.start].trim();
    let raw_expr = entry_after_access[as_tok.end..].trim();

    if raw_expr.is_empty() {
        return Err(cur.err(
            as_tok.end,
            format!("Missing expression after 'AS' in metric entry '{entry}'."),
        ));
    }

    // Phase 43: Parse trailing annotations from expression
    let (expr, annotations) = parse_trailing_annotations(raw_expr, cur.abs_of(raw_expr))?;

    // `parse_trailing_annotations` is shared with FACTS / DIMENSIONS, which is
    // where `LABELS` means something. `Metric` carries no `is_filter`, so
    // accepting it here would parse the label and then drop it — the definition
    // would round-trip having silently lost what the user wrote, the exact
    // failure rejecting an unsupported label exists to prevent.
    if annotations.is_filter {
        return Err(ParseError {
            message: "LABELS is not valid on a metric; it applies to facts and dimensions."
                .to_string(),
            position: Some(cur.abs_of(raw_expr)),
        });
    }
    if let Some(clause) = annotations.member_metadata_clause() {
        return Err(ParseError {
            message: format!(
                "{clause} is not valid on a metric; it applies to {}.",
                super::applies_to(clause)
            ),
            position: Some(cur.abs_of(raw_expr)),
        });
    }

    // Phase 48: Detect and parse OVER clause from the expression text.
    //   AVG(total_qty) OVER (PARTITION BY EXCLUDING d1, d2 ORDER BY d1)
    // Base the reported positions at the expression's own offset within the
    // entry so OVER-clause error carets point at the expression, not the
    // metric name (PR #50 review).
    let expr_abs = entry_offset + byte_offset_within(entry, raw_expr);
    let (expr, window_spec) = parse_window_over_clause(&expr, expr_abs)?;

    // Reported BEFORE the misplaced-clause scan below: a missing name is the
    // more basic defect, and letting the scan pre-empt it would answer a
    // question the user did not ask (Copilot review, #181).
    if before_as.is_empty() {
        return Err(ParseError {
            message: format!("Missing metric name before 'AS' in entry '{entry}'."),
            position: Some(entry_offset),
        });
    }

    // TECH-DEBT #38: `USING` and `NON ADDITIVE BY` are parsed from the prefix
    // BEFORE `AS`, so writing either after it made the clause part of the
    // expression: the metric was stored as ordinary and additive with its
    // declared semantics silently discarded, `GET_DDL` round-tripped the
    // malformed text, and the only complaint arrived at query time as a parser
    // error pointing inside generated SQL. Reject it here, naming the clause and
    // the position it belongs in.
    //
    // Scanning at DEPTH 0 is what makes this safe: an aggregate call's contents
    // sit at depth >= 1, so a depth-0 keyword can only be OUTSIDE the call —
    // exactly the misplaced-clause position. The scan is also quote-aware, so
    // the same words inside a string literal or quoted identifier are data, not
    // syntax. `OVER` is simply not one of the scanned keywords — note that
    // `parse_window_over_clause` returns the expression text UNCHANGED, so an
    // `OVER (...)` is still present here; it is left alone because it is the one
    // clause that legitimately follows `AS`.
    if let Some((at, message)) = misplaced_metric_clause(&expr) {
        return Err(ParseError {
            message: message.to_string(),
            position: Some(expr_abs + at),
        });
    }

    // Parse `before_as` = `name [USING (...)] [NON ADDITIVE BY (...)]` on a
    // cursor scoped to that slice (its base is its offset within the entry, so
    // token carets stay accurate under a leading access modifier).
    let before_base = entry_offset + byte_offset_within(entry, before_as);

    // Snowflake writes a fact's or dimension's LABELS between its name and
    // `AS`. On a metric it is as invalid there as after the expression, so say
    // that rather than misreading it as part of the name or USING.
    let before_cur = Cursor::new(before_as, before_base);
    if let Some(labels) = before_cur
        .find_kw_depth0("LABELS")
        .filter(|l| l.start > 0 && before_as.as_bytes()[l.start - 1].is_ascii_whitespace())
    {
        return Err(before_cur.err(
            labels.start,
            "LABELS is not valid on a metric; it applies to facts and dimensions.".to_string(),
        ));
    }

    // Phase 47: NON ADDITIVE BY appears after USING when both are present, so
    // peel it off the tail first.
    let mut non_additive_by: Vec<NonAdditiveDim> = Vec::new();
    let before_na = {
        let mut nab_cur = Cursor::new(before_as, before_base);
        if let Some((na_first, na_last)) = nab_cur.find_kw_seq_depth0(&["NON", "ADDITIVE", "BY"]) {
            nab_cur.advance_past_byte(na_last.end);
            // The token where `(` is expected; caret recovered from it (P-4).
            let after_na_abs = nab_cur.abs(nab_cur.byte_pos());
            if !nab_cur.peek_is_symbol(b'(') {
                return Err(ParseError {
                    message: format!(
                        "Expected '(' after NON ADDITIVE BY in metric entry '{entry}'."
                    ),
                    position: Some(after_na_abs),
                });
            }
            let Some(inner) = nab_cur.take_parens() else {
                return Err(ParseError {
                    message: format!(
                        "Unclosed '(' after NON ADDITIVE BY in metric entry '{entry}'."
                    ),
                    position: Some(after_na_abs),
                });
            };
            non_additive_by =
                parse_non_additive_dims(inner, entry_offset + byte_offset_within(entry, inner))?;
            // F-3 (code-review 2026-07-16): NON ADDITIVE BY is the final clause
            // before AS, so nothing may follow its `(...)`. Previously the
            // cursor stopped at the closing paren and any trailing text
            // (`... NON ADDITIVE BY (d) junk AS ...`) was silently discarded.
            if let Some(tok) = nab_cur.peek() {
                let residue = before_as[tok.start..].trim();
                return Err(ParseError {
                    message: format!(
                        "Unexpected text '{residue}' after NON ADDITIVE BY (...) in metric entry '{entry}'."
                    ),
                    position: Some(nab_cur.abs(tok.start)),
                });
            }
            before_as[..na_first.start].trim()
        } else {
            before_as
        }
    };

    // Phase 48: OVER clause combined with NON ADDITIVE BY produces error (mutually exclusive)
    if window_spec.is_some() && !non_additive_by.is_empty() {
        let name_part = before_na.trim();
        return Err(ParseError {
            message: format!(
                "Cannot combine OVER clause with NON ADDITIVE BY on metric '{name_part}'. \
                 Use one or the other.",
            ),
            position: Some(entry_offset),
        });
    }

    // USING (...) sits between the name and NON ADDITIVE BY. Scope a cursor to
    // `before_na`.
    let mut using_relationships: Vec<String> = Vec::new();
    let final_name_portion = {
        let na_base = entry_offset + byte_offset_within(entry, before_na);
        let mut using_cur = Cursor::new(before_na, na_base);
        if let Some(using_tok) = using_cur.find_kw_depth0("USING") {
            using_cur.advance_past_byte(using_tok.end);
            let after_using_abs = using_cur.abs(using_cur.byte_pos());
            if !using_cur.peek_is_symbol(b'(') {
                return Err(ParseError {
                    message: format!("Expected '(' after USING in metric entry '{entry}'."),
                    position: Some(after_using_abs),
                });
            }
            let Some(inner) = using_cur.take_parens() else {
                return Err(ParseError {
                    message: format!("Unclosed '(' after USING in metric entry '{entry}'."),
                    position: Some(after_using_abs),
                });
            };
            using_relationships = split_at_depth0_commas(inner)?
                .into_iter()
                .map(|(_, rel)| rel.to_string())
                .collect();
            // F-3 (code-review 2026-07-16): the only clause that may follow
            // USING (...) is NON ADDITIVE BY, which was already peeled off into
            // `before_na` above — so nothing may remain here. Previously
            // `... USING (r) junk AS ...` silently discarded the junk.
            if let Some(tok) = using_cur.peek() {
                let residue = before_na[tok.start..].trim();
                return Err(ParseError {
                    message: format!(
                        "Unexpected text '{residue}' after USING (...) in metric entry '{entry}'."
                    ),
                    position: Some(using_cur.abs(tok.start)),
                });
            }
            before_na[..using_tok.start].trim()
        } else {
            before_na
        }
    };

    // The `before_as.is_empty()` guard above ran BEFORE `USING (...)` and
    // `NON ADDITIVE BY (...)` were peeled off, so an entry made of nothing but
    // those clauses (`USING () AS 1`, `PRIVATE NON ADDITIVE BY () AS 1`) cleared
    // it with a non-empty `before_as` and arrives here with the name slot empty.
    // `identifier_slot_error` returns `None` for an empty slot, so without this
    // check the metric was stored NAMELESS -- and `render_ddl` emits an empty
    // name as `""`, which this parser rejects as an empty quoted identifier. The
    // nameless metric therefore rendered to DDL that no longer parsed, breaking
    // the `render(parse(render(d))) == render(d)` fixpoint (CI
    // fuzz_render_roundtrip, 2026-08-08). Same diagnostic as the guard above:
    // from the user's side the defect is identical, the name is missing.
    if final_name_portion.trim().is_empty() {
        return Err(ParseError {
            message: format!("Missing metric name before 'AS' in entry '{entry}'."),
            position: Some(entry_offset),
        });
    }

    // Distinguish qualified (`alias.name`) from unqualified (derived) at the
    // first `.` SYMBOL token — quote-aware (a dot inside `"a.b"` is inert).
    let name_base = entry_offset + byte_offset_within(entry, final_name_portion);
    let name_cur = Cursor::new(final_name_portion, name_base);
    if let Some(dot_tok) = name_cur.find_symbol(b'.') {
        // Qualified: alias.name
        let source_alias = final_name_portion[..dot_tok.start].trim().to_string();
        let bare_name = final_name_portion[dot_tok.end..].trim().to_string();

        if source_alias.is_empty() {
            return Err(ParseError {
                message: format!("Source alias before '.' is empty in metric entry '{entry}'."),
                position: Some(entry_offset),
            });
        }
        if bare_name.is_empty() {
            return Err(name_cur.err(
                dot_tok.end,
                format!("Missing bare name between '.' and 'AS' in metric entry '{entry}'."),
            ));
        }
        // F-9 / F-11 (code-review 2026-07-16): alias and name must each be a
        // single well-formed identifier — `o.d junk AS ...` previously stored
        // the two-word name `"d junk"`, and an empty quoted `""` slid through.
        if let Some(reason) = super::scan::ddl_identifier_slot_error(&source_alias) {
            return Err(ParseError {
                message: format!("Invalid source alias in metric entry '{entry}': {reason}."),
                position: Some(entry_offset),
            });
        }
        if let Some(reason) = super::scan::ddl_identifier_slot_error(&bare_name) {
            return Err(name_cur.err(
                dot_tok.end,
                format!("Invalid name in metric entry '{entry}': {reason}."),
            ));
        }

        Ok(ParsedMetric {
            source_alias: Some(source_alias),
            name: bare_name,
            expr,
            using_relationships,
            comment: annotations.comment,
            synonyms: annotations.synonyms,
            tags: annotations.tags,
            access,
            non_additive_by,
            window_spec,
        })
    } else {
        // Unqualified: just name (derived metric)
        // USING is not allowed on derived metrics
        if !using_relationships.is_empty() {
            return Err(ParseError {
                message: format!(
                    "USING clause not allowed on derived metric '{final_name_portion}'. \
                     Only qualified metrics (alias.name) can use USING.",
                ),
                position: Some(entry_offset),
            });
        }
        // NON ADDITIVE BY is not allowed on derived metrics
        if !non_additive_by.is_empty() {
            return Err(ParseError {
                message: format!(
                    "NON ADDITIVE BY clause not allowed on derived metric '{final_name_portion}'. \
                     Only qualified metrics (alias.name) can use NON ADDITIVE BY.",
                ),
                position: Some(entry_offset),
            });
        }
        // OVER clause is not allowed on derived metrics
        if window_spec.is_some() {
            return Err(ParseError {
                message: format!(
                    "OVER clause not allowed on derived metric '{final_name_portion}'. \
                     Only qualified metrics (alias.name) can use OVER.",
                ),
                position: Some(entry_offset),
            });
        }
        // F-9 / F-11: a derived metric name must be a single well-formed
        // identifier too (`total junk AS ...` is not a legal name).
        if let Some(reason) = super::scan::ddl_identifier_slot_error(final_name_portion) {
            return Err(ParseError {
                message: format!("Invalid derived metric name in entry '{entry}': {reason}."),
                position: Some(entry_offset),
            });
        }
        let bare_name = final_name_portion.to_string();
        Ok(ParsedMetric {
            source_alias: None,
            name: bare_name,
            expr,
            using_relationships: vec![],
            comment: annotations.comment,
            synonyms: annotations.synonyms,
            tags: annotations.tags,
            access,
            non_additive_by: vec![],
            window_spec: None,
        })
    }
}

/// A `USING (...)` or `NON ADDITIVE BY (...)` at depth 0 of a metric's
/// expression: its offset in `expr` and the message refusing it. Both clauses
/// belong before `AS` (TECH-DEBT #38), so the metric parser refuses them after
/// it, and YAML, whose expression the render writes after `AS`, refuses them
/// too.
pub(crate) fn misplaced_metric_clause(expr: &str) -> Option<(usize, &'static str)> {
    // Strip redundant outer parens first: `AS (sum(v) USING (r))` wraps the
    // whole expression, which would push the keywords to depth 1 and hide them
    // from the scan — the same bug through another door (Copilot review, #181).
    // Only a WHOLLY-parenthesised expression is unwrapped, so a structural
    // `(...)` — a subquery, an argument list — keeps its contents nested and a
    // genuinely nested `USING` inside a JOIN stays legal.
    let mut scan_src = expr.trim();
    let mut scan_abs = byte_offset_within(expr, scan_src);
    loop {
        let probe = Cursor::new(scan_src, scan_abs);
        if !probe.is_wholly_parenthesised() {
            break;
        }
        let inner = scan_src[1..scan_src.len() - 1].trim();
        scan_abs += byte_offset_within(scan_src, inner);
        scan_src = inner;
    }
    let expr_cur = Cursor::new(scan_src, scan_abs);
    if let Some((tok, _)) = expr_cur.find_kw_seq_depth0(&["NON", "ADDITIVE", "BY"]) {
        return Some((
            expr_cur.abs(tok.start),
            "'NON ADDITIVE BY (...)' must come BEFORE 'AS' in a metric entry. \
             Form: 'alias.name [USING (...)] [NON ADDITIVE BY (...)] AS expr'.",
        ));
    }
    expr_cur.find_kw_depth0("USING").map(|tok| {
        (
            expr_cur.abs(tok.start),
            "'USING (...)' must come BEFORE 'AS' in a metric entry. \
             Form: 'alias.name [USING (...)] [NON ADDITIVE BY (...)] AS expr'.",
        )
    })
}
