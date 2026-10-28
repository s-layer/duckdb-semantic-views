//! TABLES clause parsing.
//!
//! §6.1 (code-review 2026-07-11): this is the first clause migrated onto the
//! shared [`Cursor`]/lexer. Each entry is parsed by consuming tokens in order
//! — alias, `AS`, source-table name, then the optional `PRIMARY KEY` / `UNIQUE`
//! constraints — so "text between the name and a constraint" is a visible
//! unexpected token rather than a region a `find_primary_key`-anywhere scan
//! could silently slice past (the P-1 silent-discard hole). Quote-awareness is
//! structural: a keyword inside a `"quoted"` name or a `'string'` COMMENT is a
//! single token, never a matchable keyword.
//!
//! The trailing `COMMENT` / `WITH SYNONYMS` region is still handed as verbatim
//! source to the shared [`parse_trailing_annotations`], which tiles it (P-2).

use super::annotations::parse_trailing_annotations;
use super::cursor::Cursor;
use super::lexer::TokenKind;
use super::split_at_depth0_commas;
use crate::errors::ParseError;
use crate::model::TableRef;

/// Parse the content inside TABLES (...).
///
/// Each entry has the form: `alias AS physical_table PRIMARY KEY (col1, col2, ...)`
pub(crate) fn parse_tables_clause(
    body: &str,
    base_offset: usize,
) -> Result<Vec<TableRef>, ParseError> {
    let entries = split_at_depth0_commas(body)?;
    let mut result = Vec::new();

    for (entry_start, entry) in entries {
        let entry_offset = base_offset + entry_start;
        let table_ref = parse_single_table_entry(entry, entry_offset)?;
        result.push(table_ref);
    }

    Ok(result)
}

/// The "physical table name is missing" error is emitted from three guards
/// (no name token, a bare reserved keyword in the name slot, ...) so the exact
/// message stays single-sourced.
fn missing_name_msg(alias: &str) -> String {
    if alias.is_empty() {
        // Alias-less entry (F-7): there is no alias to name in the message.
        "Missing physical table name in TABLES clause.".to_string()
    } else {
        format!("Missing physical table name after AS for alias '{alias}' in TABLES clause.")
    }
}

/// Resolve the `[alias AS] table_name` prefix of a TABLES entry (Snowflake's
/// grammar makes the alias optional — F-7). Advances `cur` past the source-table
/// name so the caller can parse the trailing constraints, and returns
/// `(alias, table_name, name_end)` where `name_end` is the byte offset in `entry`
/// just past the name.
///
/// When the alias is omitted, it defaults to the LAST identifier component of the
/// table name — not the whole (possibly qualified) name: a dotted alias like
/// `schema.orders` would be mis-split by the `alias.name` reference parser (which
/// splits at the first dot, so `schema.orders.region` would resolve alias
/// `schema`). Taking the last component yields the usable `orders` and matches
/// Snowflake's implicit-alias behaviour; quoted parts are preserved.
fn resolve_table_alias_and_name<'a>(
    cur: &mut Cursor<'a>,
    entry: &'a str,
    entry_offset: usize,
) -> Result<(&'a str, &'a str, usize), ParseError> {
    // The first token is either an alias (when `AS` follows) or the start of a
    // bare source-table name. A leading symbol (`(`, `.`, ...) is rejected.
    let Some(first_tok) = cur.peek() else {
        return Err(cur.err(
            0,
            "Expected table alias or name in TABLES entry.".to_string(),
        ));
    };
    if matches!(first_tok.kind, TokenKind::Symbol(_)) {
        return Err(cur.err(
            first_tok.start,
            "Expected table alias or name in TABLES entry.".to_string(),
        ));
    }

    // Decide `alias AS table` vs a bare `table`. The alias, when present, is the
    // first identifier token immediately followed by `AS`. Tokenization gives the
    // word boundary for free: `ASorders` is one bare token (not `AS`), and
    // `AS"my table"` splits into `AS` + the quoted name.
    cur.bump(); // consume the first token
    let has_alias = matches!(cur.peek(), Some(t) if cur.is_kw(t, "AS"));

    if has_alias {
        let alias = cur.text(first_tok);
        cur.bump(); // AS
        let (table_name, name_end) = take_source_table_name(cur, entry, alias)?;
        Ok((alias, table_name, name_end))
    } else {
        // Alias-less: re-scan the name from the start of the entry so a dotted /
        // quoted FQN (`schema.orders`, `"my table"`) is captured whole, then
        // default the alias to the name's last identifier component.
        let mut name_cur = Cursor::new(entry, entry_offset);
        let (table_name, name_end) = take_source_table_name(&mut name_cur, entry, "")?;
        cur.advance_past_byte(name_end); // resync for constraint parsing
        let mut alias = table_name;
        while let Some((_, after)) = super::scan::split_qualified_identifier(alias) {
            alias = after;
        }
        Ok((alias, table_name, name_end))
    }
}

/// Parse a single TABLES clause entry.
///
/// Supports:
/// - `alias AS physical_table PRIMARY KEY (cols) [UNIQUE (cols)]*`
/// - `alias AS physical_table [UNIQUE (cols)]*`   (no PRIMARY KEY -- fact tables)
/// - `alias AS physical_table`                    (bare -- no PK, no UNIQUE)
fn parse_single_table_entry(entry: &str, entry_offset: usize) -> Result<TableRef, ParseError> {
    let entry = entry.trim();
    let mut cur = Cursor::new(entry, entry_offset);

    // Steps 1-3: resolve the `[alias AS] table_name` prefix, advancing `cur`
    // past the source-table name so the constraint scan below continues after it.
    let (alias, table_name, name_end) =
        resolve_table_alias_and_name(&mut cur, entry, entry_offset)?;

    // F-11 (code-review 2026-07-16): the alias and the source-table name must
    // each be a well-formed identifier — an empty quoted `""` in either slot
    // (`TABLES ("" AS orders ...)`) previously parsed. (`take_source_table_name`
    // already rejects a multi-token name run, so F-9 does not recur here.)
    if let Some(reason) = super::scan::identifier_slot_error(alias) {
        return Err(cur.err(
            0,
            format!("Invalid table alias in TABLES entry '{entry}': {reason}."),
        ));
    }
    if let Some(reason) = super::scan::identifier_slot_error(table_name) {
        // Caret at the table-name token (its start = name_end - len), not the
        // entry start, so `alias AS <bad table>` points at the offending name
        // rather than the alias (Copilot review).
        return Err(cur.err(
            name_end - table_name.len(),
            format!("Invalid source-table name in TABLES entry '{entry}': {reason}."),
        ));
    }

    // Step 4: optional PRIMARY KEY. Its keyword pair may appear anywhere in the
    // remaining tokens; any token before it is text that does not belong
    // between the name and the constraint (P-1).
    let mut pk_columns: Vec<String> = Vec::new();
    if let Some(pk_tok) = cur.find_kw_pair("PRIMARY", "KEY") {
        let between = entry[name_end..pk_tok.start].trim();
        if !between.is_empty() {
            let off = cur.peek().map_or(name_end, |t| t.start);
            return Err(cur.err(
                off,
                format!(
                    "Unexpected text '{between}' between source table name and PRIMARY KEY for alias '{alias}' in TABLES clause. Constraints must immediately follow the table name; COMMENT / WITH SYNONYMS come after constraints.",
                ),
            ));
        }
        cur.bump(); // PRIMARY
        cur.bump(); // KEY
        pk_columns = take_columns(
            &mut cur,
            "Expected '(' after PRIMARY KEY in TABLES clause.".to_string(),
            "Unclosed '(' in PRIMARY KEY column list.".to_string(),
        )?;
    }

    // Step 5: zero or more UNIQUE constraints, each immediately following the
    // previous constraint. Text before a UNIQUE keyword is rejected, not
    // discarded (P-1 companion).
    let mut unique_constraints: Vec<Vec<String>> = Vec::new();
    while let Some(u_tok) = cur.find_kw("UNIQUE") {
        let between = entry[cur.byte_pos()..u_tok.start].trim();
        if !between.is_empty() {
            let off = cur.peek().map_or(u_tok.start, |t| t.start);
            return Err(cur.err(
                off,
                format!(
                    "Unexpected text '{between}' before UNIQUE for alias '{alias}' in TABLES clause. Constraints must immediately follow the table name or the preceding constraint; COMMENT / WITH SYNONYMS come after constraints.",
                ),
            ));
        }
        cur.bump(); // UNIQUE
        let cols = take_columns(
            &mut cur,
            format!("Expected '(' after UNIQUE keyword for table alias '{alias}'."),
            format!("Unclosed '(' in UNIQUE column list for table alias '{alias}'."),
        )?;
        unique_constraints.push(cols);
    }

    // Step 6: trailing COMMENT / WITH SYNONYMS annotations. The shared parser
    // tiles the region exactly; any non-annotation text left in front of it is
    // reported here rather than silently dropped (PA-9 companion).
    let rest = cur.rest();
    let (leftover, annotations) = parse_trailing_annotations(rest, cur.abs_of(rest))?;
    if !leftover.trim().is_empty() {
        return Err(ParseError {
            message: format!(
                "Unexpected text '{}' after table declaration for alias '{alias}' in TABLES clause.",
                leftover.trim()
            ),
            position: Some(entry_offset),
        });
    }
    // The shared annotation parser also understands `LABELS`, which only means
    // something on a fact or dimension. `TableRef` carries no `is_filter`, so
    // accepting it here would silently discard it on the way to the model.
    if annotations.is_filter {
        return Err(ParseError {
            message: "LABELS is not valid on a table; it applies to facts and dimensions."
                .to_string(),
            position: Some(entry_offset),
        });
    }
    if let Some(clause) = annotations.member_metadata_clause() {
        return Err(ParseError {
            message: format!(
                "{clause} is not valid on a table; it applies to {}.",
                super::applies_to(clause)
            ),
            position: Some(entry_offset),
        });
    }

    Ok(TableRef {
        alias: alias.to_string(),
        table: table_name.to_string(),
        pk_columns,
        unique_constraints,
        comment: annotations.comment,
        synonyms: annotations.synonyms,
        tags: annotations.tags,
    })
}

/// Capture the source-table name after `AS` — a maximal run of tokens with no
/// whitespace gap, stopping before a `(` / `;` symbol. This reproduces
/// `find_identifier_end`: a dotted / quoted FQN like `"my db"."sch"."t"` is
/// contiguous and captured whole, while a following ` PRIMARY KEY` is separated
/// by whitespace and left for the constraint parser. Returns the verbatim name
/// slice and its end offset (in `entry`) for the caller's "between" check.
fn take_source_table_name<'a>(
    cur: &mut Cursor<'a>,
    entry: &'a str,
    alias: &str,
) -> Result<(&'a str, usize), ParseError> {
    let Some(first) = cur.peek() else {
        return Err(cur.err(cur.byte_pos(), missing_name_msg(alias)));
    };
    if matches!(first.kind, TokenKind::Symbol(_)) {
        // A leading `(` / `.` / `=` / etc. where the name should be. `find_identifier_end`
        // returned 0 only for `(` / `;` / whitespace, but a leading `.foo` / `=x`
        // is a non-name that is better rejected here than accepted as a bogus
        // table name (pinned by `test_leading_symbol_in_name_slot_is_missing_name`).
        return Err(cur.err(first.start, missing_name_msg(alias)));
    }
    let name_start = first.start;
    let mut name_end = first.end;
    let mut unterminated_ident = matches!(first.kind, TokenKind::Unterminated { ident: true });
    cur.bump();
    while let Some(t) = cur.peek() {
        // A whitespace gap or a `(` / `;` symbol ends the name.
        if t.start != name_end || matches!(t.kind, TokenKind::Symbol(b'(' | b';')) {
            break;
        }
        if matches!(t.kind, TokenKind::Unterminated { ident: true }) {
            unterminated_ident = true;
        }
        name_end = t.end;
        cur.bump();
    }
    let table_name = &entry[name_start..name_end];

    // Bare reserved keywords in the name slot surface the missing-name error
    // (Phase 68 A1 / D-03) — `o AS PRIMARY KEY (id)` has no real table name.
    if matches!(
        table_name.to_ascii_uppercase().as_str(),
        "PRIMARY" | "UNIQUE" | "FOREIGN" | "REFERENCES" | "NOT"
    ) {
        return Err(cur.err(name_start, missing_name_msg(alias)));
    }
    // An unterminated `"..."` in the name slot (Phase 68 A4). A doubled-quote
    // `""` is an escape and stays balanced, so only a genuinely open quote trips
    // this — the lexer already encoded that distinction in the token kind.
    if unterminated_ident {
        return Err(cur.err(
            name_start,
            format!(
                "Unterminated quoted identifier in source-table name for alias '{alias}' in TABLES clause.",
            ),
        ));
    }
    Ok((table_name, name_end))
}

/// Consume the `(col, col, ...)` list that must follow a `PRIMARY KEY` /
/// `UNIQUE` keyword. `expected_msg` fires when no `(` follows; `unclosed_msg`
/// when the `(` never closes. Both carets point at where the `(` was expected.
fn take_columns(
    cur: &mut Cursor,
    expected_msg: String,
    unclosed_msg: String,
) -> Result<Vec<String>, ParseError> {
    let off = cur.byte_pos();
    match cur.peek() {
        Some(t) if t.kind == TokenKind::Symbol(b'(') => {}
        _ => return Err(cur.err(off, expected_msg)),
    }
    let Some(inner) = cur.take_parens() else {
        return Err(cur.err(off, unclosed_msg));
    };
    Ok(split_at_depth0_commas(inner)?
        .into_iter()
        .map(|(_, col)| col.to_string())
        .collect())
}
