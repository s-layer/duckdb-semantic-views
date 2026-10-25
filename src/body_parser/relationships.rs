//! RELATIONSHIPS clause parsing.
//!
//! §6.1 (phase 2, code-review 2026-07-11): migrated onto the shared
//! [`Cursor`]/lexer. The grammar is
//! `rel_name AS from_alias(fk_cols) REFERENCES to_alias[(ref_cols)]`; parsing
//! it through tokens fixes the non-quote-aware `after_as.find('(')` (P-11 — a
//! quoted `from_alias` containing `(` mis-split) and closes the silent-discard
//! gap between the FK list and `REFERENCES` (text there was dropped, the P-1
//! class): `REFERENCES` must now be the token immediately following the FK
//! `(...)`. Every error still anchors at `entry_offset`, as before.

use super::cursor::Cursor;
use super::split_at_depth0_commas;
use crate::errors::ParseError;
use crate::model::{Cardinality, Join};

/// Parse the content inside RELATIONSHIPS (...). Returns empty vec for empty body.
///
/// Each entry has the form:
///   `rel_name AS from_alias(fk_col1, fk_col2) REFERENCES to_alias`
pub(crate) fn parse_relationships_clause(
    body: &str,
    base_offset: usize,
) -> Result<Vec<Join>, ParseError> {
    if body.trim().is_empty() {
        return Ok(vec![]);
    }

    let entries = split_at_depth0_commas(body)?;
    let mut result = Vec::new();

    for (entry_start, entry) in entries {
        let entry_offset = base_offset + entry_start;
        let join = parse_single_relationship_entry(entry, entry_offset)?;
        result.push(join);
    }

    Ok(result)
}

/// Parse one RELATIONSHIPS entry: `rel_name AS from_alias(fk_cols) REFERENCES to_alias[(ref_cols)]`
///
/// Phase 33: Cardinality keywords (MANY TO ONE, etc.) are no longer accepted.
/// Cardinality is inferred from PK/UNIQUE constraints at parse time.
/// Optional `REFERENCES target(col1, col2)` syntax stores explicit `ref_columns`.
fn parse_single_relationship_entry(entry: &str, entry_offset: usize) -> Result<Join, ParseError> {
    let entry = entry.trim();
    let mut cur = Cursor::new(entry, entry_offset);

    // `rel_name AS ...` — the relationship name is everything before the first
    // `AS` keyword token (quote-aware: an `AS` inside a quoted name is not a
    // keyword). No `AS` at all ⇒ the whole entry is malformed.
    let Some(as_tok) = cur.find_kw("AS") else {
        return Err(cur.err(
            0,
            format!(
                "Missing relationship name: expected 'rel_name AS from_alias(fk_cols) REFERENCES to_alias', got '{entry}'.",
            ),
        ));
    };
    let rel_name = entry[..as_tok.start].trim();
    if rel_name.is_empty() {
        return Err(cur.err(
            0,
            "Relationship name is required; found 'AS' without a preceding name.".to_string(),
        ));
    }
    // F-9 / F-11 (code-review 2026-07-16): the relationship name must be a
    // single well-formed identifier — `foo bar AS ...` previously stored the
    // two-word name `"foo bar"`, and an empty quoted `""` slid through.
    if let Some(reason) = super::scan::ddl_identifier_slot_error(rel_name) {
        return Err(cur.err(
            0,
            format!("Invalid relationship name in entry '{entry}': {reason}."),
        ));
    }
    let after_as = entry[as_tok.end..].trim_start();
    cur.advance_past_byte(as_tok.end);

    let from_alias = take_from_alias(&mut cur, rel_name, after_as)?;
    let fk_columns = take_columns(
        &mut cur,
        entry_offset,
        format!("Unclosed '(' in FK column list for relationship '{rel_name}'."),
    )?;

    // `REFERENCES` must immediately follow the FK list (any token in between is
    // rejected rather than silently skipped past — the old anywhere-scan).
    match cur.peek() {
        Some(t) if cur.is_kw(t, "REFERENCES") => {
            cur.bump();
        }
        _ => {
            return Err(cur.err(
                0,
                format!("Expected 'REFERENCES' after FK columns in relationship '{rel_name}'."),
            ));
        }
    }

    // `to_alias[(ref_cols)]` — a single alias token, then an optional explicit
    // reference-column list.
    let to_alias = match cur.peek() {
        Some(t) if cur.peek_is_value() => {
            cur.bump();
            cur.text(t)
        }
        _ => {
            return Err(cur.err(
                0,
                format!("Expected target alias after REFERENCES in relationship '{rel_name}'."),
            ));
        }
    };
    let ref_columns = if cur.peek_is_symbol(b'(') {
        take_columns(
            &mut cur,
            entry_offset,
            format!("Unclosed '(' in REFERENCES column list for relationship '{rel_name}'."),
        )?
    } else {
        vec![]
    };

    // Anything left is trailing garbage (retired cardinality keywords, etc.).
    let leftover = cur.rest().trim();
    if !leftover.is_empty() {
        return Err(cur.err(
            0,
            format!(
                "Unexpected tokens after REFERENCES target in relationship '{rel_name}': '{leftover}'. \
                 Cardinality is now inferred from PK/UNIQUE constraints; explicit keywords are no longer supported.",
            ),
        ));
    }

    Ok(Join {
        table: to_alias.to_string(),
        from_alias: from_alias.to_string(),
        fk_columns,
        ref_columns,
        name: Some(rel_name.to_string()),
        cardinality: Cardinality::default(), // will be set by inference
    })
}

/// Capture the from-alias: a SINGLE value token (a table alias is one
/// identifier, matching TABLES) that must be immediately followed by `(`.
/// Quote-awareness is structural — a `(` inside a quoted alias is part of that
/// one token (the P-11 fix) — and a multi-token run like `a b(...)` / `a.b(...)`
/// is rejected rather than captured as a bogus alias that only fails later at
/// resolution (PR #101 review). Leaves the cursor positioned at the `(`.
/// `after_as` is echoed in the "Expected '('" error to show the offending tail.
fn take_from_alias<'a>(
    cur: &mut Cursor<'a>,
    rel_name: &str,
    after_as: &str,
) -> Result<&'a str, ParseError> {
    let expected_paren =
        || format!("Expected '(' after from_alias in relationship '{rel_name}'. Got: '{after_as}'");
    if cur.peek_is_symbol(b'(') {
        return Err(cur.err(
            0,
            format!("Expected from_alias before '(' in relationship '{rel_name}'."),
        ));
    }
    let from_alias = match cur.peek() {
        Some(t) if cur.peek_is_value() => {
            cur.bump();
            cur.text(t)
        }
        _ => return Err(cur.err(0, expected_paren())),
    };
    if !cur.peek_is_symbol(b'(') {
        return Err(cur.err(0, expected_paren()));
    }
    Ok(from_alias)
}

/// Consume a `(col, col, ...)` list at the cursor's current position (which
/// must be `(`). `unclosed_msg` fires when the group never closes; all
/// relationship errors anchor at `entry_offset`.
fn take_columns(
    cur: &mut Cursor,
    entry_offset: usize,
    unclosed_msg: String,
) -> Result<Vec<String>, ParseError> {
    let Some(inner) = cur.take_parens() else {
        return Err(ParseError {
            message: unclosed_msg,
            position: Some(entry_offset),
        });
    };
    Ok(split_at_depth0_commas(inner)?
        .into_iter()
        .map(|(_, col)| col.to_string())
        .collect())
}
