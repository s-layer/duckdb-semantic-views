//! RELATIONSHIPS clause parsing.
//!
//! §6.1 (phase 2, code-review 2026-07-11): migrated onto the shared
//! [`Cursor`]/lexer. The grammar is
//! `[rel_name AS] from_alias(fk_cols) REFERENCES to_alias[(ref_cols)]`; parsing
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

    for (index, (entry_start, entry)) in entries.into_iter().enumerate() {
        let entry_offset = base_offset + entry_start;
        let mut join = parse_single_relationship_entry(entry, entry_offset)?;
        if join.generated_name {
            join.name = Some(join.generated_name_at(index));
        }
        result.push(join);
    }

    Ok(result)
}

/// Parse one RELATIONSHIPS entry: `[rel_name AS] from_alias(fk_cols) REFERENCES to_alias[(ref_cols)]`
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
    // No `AS` at all: an unnamed relationship (`from_alias(fk_cols)
    // REFERENCES to_alias`), as Snowflake allows. It is named in messages by
    // its entry and gets a generated name once parsed.
    let Some(as_tok) = cur.find_kw("AS") else {
        let mut join = parse_relationship_body(&mut cur, entry, entry, entry_offset)?;
        join.generated_name = true;
        return Ok(join);
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
    let mut join = parse_relationship_body(&mut cur, rel_name, after_as, entry_offset)?;
    join.name = Some(rel_name.to_string());
    Ok(join)
}

/// The part of a relationship entry after its `name AS`, or all of an unnamed
/// one: `from_alias(fk_cols) REFERENCES to_alias[(ref_cols)]`. `rel_name`
/// names it in errors. The returned join has no name.
fn parse_relationship_body(
    cur: &mut Cursor,
    rel_name: &str,
    after_as: &str,
    entry_offset: usize,
) -> Result<Join, ParseError> {
    let from_alias = take_from_alias(cur, rel_name, after_as)?;
    let fk_columns = take_columns(
        cur,
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
    let mut ref_columns = if cur.peek_is_symbol(b'(') {
        take_columns(
            cur,
            entry_offset,
            format!("Unclosed '(' in REFERENCES column list for relationship '{rel_name}'."),
        )?
    } else {
        vec![]
    };
    let asof = take_asof_marker(&mut ref_columns, rel_name, entry_offset)?;
    let range_end =
        take_range_reference(&mut ref_columns, &fk_columns, asof, rel_name).map_err(|message| {
            ParseError {
                message,
                position: Some(entry_offset),
            }
        })?;
    if fk_columns.iter().any(|c| strip_asof(c).is_some()) {
        return Err(ParseError {
            message: format!(
                "ASOF in relationship '{rel_name}' must mark a column of the REFERENCES list, \
                 not a foreign-key column: write 'REFERENCES t (key, ASOF column)'."
            ),
            position: Some(entry_offset),
        });
    }

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
        name: None,
        cardinality: Cardinality::default(), // will be set by inference
        asof,
        range_end,
        generated_name: false,
    })
}

/// Phase 7.2: the `start AND end EXCLUSIVE` that follows a `BETWEEN` keyword
/// (already consumed), as in a range relationship's `REFERENCES p (BETWEEN s
/// AND e EXCLUSIVE)` and a table's `CONSTRAINT c DISTINCT RANGE BETWEEN s AND
/// e EXCLUSIVE`. Each bound is one column, kept as written, and the two must
/// differ. The cursor is left after `EXCLUSIVE`. `what` names the construct in
/// the errors.
pub(super) fn take_range_bounds(cur: &mut Cursor, what: &str) -> Result<(String, String), String> {
    let usage = || format!("{what} must read 'BETWEEN start_column AND end_column EXCLUSIVE'");
    let column = |cur: &mut Cursor| match cur.peek() {
        Some(t) if cur.peek_is_value() && !cur.is_kw(t, "AND") && !cur.is_kw(t, "EXCLUSIVE") => {
            cur.bump();
            Ok(cur.text(t).to_string())
        }
        _ => Err(format!("{}.", usage())),
    };
    let start = column(cur)?;
    match cur.peek() {
        Some(t) if cur.is_kw(t, "AND") => {
            cur.bump();
        }
        _ => return Err(format!("{}.", usage())),
    }
    let end = column(cur)?;
    match cur.peek() {
        Some(t) if cur.is_kw(t, "EXCLUSIVE") => {
            cur.bump();
        }
        _ => {
            return Err(format!(
                "{}: only half-open ranges, which include the start and exclude the end, \
                 are supported.",
                usage()
            ))
        }
    }
    if crate::ident::ident_matches(&start, &end) {
        return Err(format!(
            "{what} names '{start}' as both its start and its end; a range needs two \
             columns (use ASOF for one)."
        ));
    }
    Ok((start, end))
}

/// Why a range relationship's stored bounds would not read back from
/// `GET_DDL`'s `(BETWEEN start AND end EXCLUSIVE)` as the same pair, if they
/// would not: a keyword or a repeated column a YAML definition can hold. The
/// bounds are taken to be single columns already, which `GET_DDL` writes as
/// stored (PR #338 review).
pub(crate) fn range_reference_error(rel_name: &str, start: &str, end: &str) -> Option<String> {
    let what = format!("The range in relationship '{rel_name}'");
    match range_item(&format!("BETWEEN {start} AND {end} EXCLUSIVE"), &what) {
        Ok(Some((s, e))) if s == start && e == end => None,
        Ok(_) => Some(format!(
            "relationship '{rel_name}' range BETWEEN {start} AND {end} does not read back \
             from GET_DDL as written; quote a bound that is a keyword"
        )),
        Err(m) => Some(m),
    }
}

/// The bounds of `col` when it is a `BETWEEN ...` item, `None` when it is a
/// plain column. A column that is itself named `between` (bare, with nothing
/// after it, or quoted) is a column, not the keyword.
fn range_item(col: &str, what: &str) -> Result<Option<(String, String)>, String> {
    let mut cur = Cursor::new(col.trim(), 0);
    match cur.peek() {
        Some(t) if cur.is_kw(t, "BETWEEN") => {
            cur.bump();
            if cur.peek().is_none() {
                return Ok(None);
            }
            let bounds = take_range_bounds(&mut cur, what)?;
            if !cur.rest().trim().is_empty() {
                return Err(format!(
                    "Unexpected text '{}' after EXCLUSIVE in {}.",
                    cur.rest().trim(),
                    what.to_ascii_lowercase()
                ));
            }
            Ok(Some(bounds))
        }
        _ => Ok(None),
    }
}

/// Phase 7.2: a range relationship, `REFERENCES p (BETWEEN s AND e
/// EXCLUSIVE)`. Its REFERENCES list is that one item and its foreign-key list
/// one column: the referenced table's `DISTINCT RANGE` constraint is the key,
/// and it has no other columns to pair. On a match the item becomes the start
/// column in `ref_columns`, and the end column is returned.
fn take_range_reference(
    ref_columns: &mut [String],
    fk_columns: &[String],
    asof: Option<usize>,
    rel_name: &str,
) -> Result<Option<String>, String> {
    let what = format!("The range in relationship '{rel_name}'");
    for fk in fk_columns {
        if range_item(fk, &what)?.is_some() {
            return Err(format!(
                "BETWEEN in relationship '{rel_name}' must be the REFERENCES list, not a \
                 foreign-key column: write 'e (ts) REFERENCES p (BETWEEN s AND e EXCLUSIVE)'."
            ));
        }
    }
    let mut found = None;
    for (i, col) in ref_columns.iter().enumerate() {
        if let Some(bounds) = range_item(col, &what)? {
            found = Some((i, bounds));
            break;
        }
    }
    let Some((i, (start, end))) = found else {
        return Ok(None);
    };
    if asof.is_some() {
        return Err(format!(
            "Relationship '{rel_name}' has both ASOF and BETWEEN; a relationship is either \
             an ASOF relationship or a range relationship."
        ));
    }
    if ref_columns.len() != 1 || fk_columns.len() != 1 {
        return Err(format!(
            "A range relationship pairs one column with one BETWEEN range: write \
             'e (ts) REFERENCES p (BETWEEN s AND e EXCLUSIVE)'. Relationship '{rel_name}' has \
             {} foreign-key column(s) and {} referenced item(s).",
            fk_columns.len(),
            ref_columns.len()
        ));
    }
    ref_columns[i] = start;
    Ok(Some(end))
}

/// The column text after a leading `ASOF` keyword, or `None` when `col` does
/// not start with one. A column that is itself named `asof` (bare, with
/// nothing after it, or quoted) is a column, not the keyword.
fn strip_asof(col: &str) -> Option<&str> {
    let col = col.trim();
    let (head, rest) = col.split_at(col.find(char::is_whitespace)?);
    let rest = rest.trim_start();
    (head.eq_ignore_ascii_case("ASOF") && !rest.is_empty()).then_some(rest)
}

/// Phase 7.1: find the one `ASOF` marker in a REFERENCES column list, strip
/// it from its column and return that column's position. Snowflake allows the
/// keyword before any column of the list, and at most once.
fn take_asof_marker(
    ref_columns: &mut [String],
    rel_name: &str,
    entry_offset: usize,
) -> Result<Option<usize>, ParseError> {
    let mut asof = None;
    for (i, col) in ref_columns.iter_mut().enumerate() {
        let Some(rest) = strip_asof(col) else {
            continue;
        };
        if asof.is_some() {
            return Err(ParseError {
                message: format!(
                    "Relationship '{rel_name}' has more than one ASOF column; \
                     at most one referenced column can be ASOF."
                ),
                position: Some(entry_offset),
            });
        }
        *col = rest.to_string();
        asof = Some(i);
    }
    Ok(asof)
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
