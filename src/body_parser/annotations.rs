//! Trailing COMMENT / WITH SYNONYMS / LABELS / [WITH] TAG annotations and leading
//! access modifiers.

use super::scan::{extract_paren_prefix, is_ident_continuation, QuoteState};
use super::split_at_depth0_commas;
use crate::errors::ParseError;
use crate::model::{AccessModifier, Tag, TagName};

/// Whether a keyword starting at `i` sits on a boundary that can legitimately
/// begin a trailing annotation.
///
/// An identifier character before it disqualifies it (`my_comment`), and so
/// does a `.` — `o.comment` is a QUALIFIED COLUMN REFERENCE, not the COMMENT
/// keyword (PARSE-6, code-review 2026-08-06). Treating `.` as a word boundary
/// made the scanner start the annotation region mid-reference, leaving the
/// dangling `o.` as the stored expression and silently discarding the rest of
/// the user's predicate.
///
/// Whitespace between the qualifier and the member is skipped, because `o .
/// comment` is as qualified as `o.comment` and `DuckDB` accepts both.
fn annotation_boundary_before(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    if is_ident_continuation(bytes[i - 1]) {
        return false;
    }
    let mut j = i;
    while j > 0 && bytes[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    j == 0 || bytes[j - 1] != b'.'
}

/// Trailing metadata annotations parsed from a DDL entry.
/// Used internally to collect COMMENT, SYNONYMS, LABELS and TAG from entry text.
#[derive(Debug, Default)]
pub(super) struct ParsedAnnotations {
    pub(super) comment: Option<String>,
    pub(super) synonyms: Vec<String>,
    /// `[WITH] TAG (name = 'value', ...)`, in the order written.
    pub(super) tags: Vec<Tag>,
    /// `LABELS = (FILTER)` was present — the member is a named filter.
    pub(super) is_filter: bool,
}

/// Extract a single-quoted string value, handling '' escape sequences.
/// Input starts with the opening quote: 'text here'
/// Returns the unescaped string content. Text after the closing quote is
/// ignored (COMMENT extraction hands this function the whole annotation
/// tail, e.g. `'x' WITH SYNONYMS = (...)`).
///
/// `base_offset` is the absolute byte offset of `s[0]` in the original query,
/// so the error carets point at the offending quote (F-15, code-review
/// 2026-07-16).
///
/// Thin adapter over the shared UTF-8-correct extractor: this used to be
/// the byte-wise copy that the WR-04 fix missed, Latin-1-izing every
/// non-ASCII codepoint in COMMENT/SYNONYMS payloads (PA-2, code-review
/// 2026-07-02).
fn extract_single_quoted_string(s: &str, base_offset: usize) -> Result<String, ParseError> {
    match crate::util::extract_single_quoted_prefix(s) {
        Ok((content, _consumed)) => Ok(content),
        Err(crate::util::SingleQuoteError::NotQuoted) => Err(ParseError {
            message: "Expected single-quoted string.".to_string(),
            position: Some(base_offset),
        }),
        Err(crate::util::SingleQuoteError::Unterminated) => Err(ParseError {
            message: "Unclosed single-quoted string.".to_string(),
            position: Some(base_offset),
        }),
    }
}

/// Parse comma-separated single-quoted strings from inside parentheses.
/// Input: "'syn1', 'syn2'" (already extracted from parens).
///
/// `base_offset` is the absolute byte offset of `content[0]` in the original
/// query; each synonym's caret is recovered from its position within `content`
/// (F-15, code-review 2026-07-16).
fn parse_synonym_list(content: &str, base_offset: usize) -> Result<Vec<String>, ParseError> {
    let entries = split_at_depth0_commas(content)?;
    let mut result = Vec::new();
    for (_, entry) in entries {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry_offset = base_offset + crate::util::byte_offset_within(content, trimmed);
        result.push(extract_single_quoted_string(trimmed, entry_offset)?);
    }
    Ok(result)
}

/// Parse a `LABELS = (...)` list, returning whether it declares `FILTER`.
///
/// Only `FILTER` is supported. Snowflake's other label values (tags and the
/// like) are out of scope, and an unrecognised label is REJECTED rather than
/// ignored: silently dropping a label the user wrote would let a definition
/// round-trip having quietly lost meaning. Labels are bare keywords, not
/// quoted strings, so this does not reuse `parse_synonym_list`.
fn parse_label_list(content: &str, base_offset: usize) -> Result<bool, ParseError> {
    let entries = split_at_depth0_commas(content)?;
    let mut is_filter = false;
    for (_, entry) in entries {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry_offset = base_offset + crate::util::byte_offset_within(content, trimmed);
        if trimmed.eq_ignore_ascii_case("FILTER") {
            if is_filter {
                return Err(ParseError {
                    message: "Duplicate FILTER label.".to_string(),
                    position: Some(entry_offset),
                });
            }
            is_filter = true;
        } else {
            return Err(ParseError {
                message: format!(
                    "Unsupported label '{trimmed}'. Only FILTER is supported in LABELS = (...)."
                ),
                position: Some(entry_offset),
            });
        }
    }
    if !is_filter {
        return Err(ParseError {
            message: "LABELS = () is empty. Expected LABELS = (FILTER).".to_string(),
            position: Some(base_offset),
        });
    }
    Ok(is_filter)
}

/// Parse the inside of a `TAG ( ... )` list: `name = 'value'` pairs, each name
/// `tag`, `schema.tag` or `database.schema.tag` with any part double-quoted.
///
/// A quoted part is stored without its quotes and a bare one as written, so the
/// stored name is the tag's identity whichever way it was spelled. The list must
/// pass [`crate::model::tag_error`] (no empty or duplicate name, no value over
/// 256 characters), the same rule the YAML import applies.
fn parse_tag_list(content: &str, base_offset: usize) -> Result<Vec<Tag>, ParseError> {
    let mut tags = Vec::new();
    let mut offsets = Vec::new();
    // Every entry ends with its quoted value, so a list ending in a comma has
    // an empty last entry (which the split does not report).
    if content.trim_end().ends_with(',') {
        return Err(ParseError {
            message: "Empty entry in TAG list. Expected TAG (name = 'value', ...).".to_string(),
            position: Some(base_offset + content.trim_end().len() - 1),
        });
    }
    for (_, entry) in split_at_depth0_commas(content)? {
        let trimmed = entry.trim();
        let entry_offset = base_offset + crate::util::byte_offset_within(content, trimmed);
        if trimmed.is_empty() {
            return Err(ParseError {
                message: "Empty entry in TAG list. Expected TAG (name = 'value', ...).".to_string(),
                position: Some(entry_offset),
            });
        }
        tags.push(parse_tag_entry(trimmed, entry_offset)?);
        offsets.push(entry_offset);
    }
    if tags.is_empty() {
        return Err(ParseError {
            message: "TAG () is empty. Expected TAG (name = 'value', ...).".to_string(),
            position: Some(base_offset),
        });
    }
    if let Some((i, message)) = crate::model::tag_error(&tags) {
        return Err(ParseError {
            message: format!("Invalid TAG: {message}."),
            position: Some(offsets[i]),
        });
    }
    Ok(tags)
}

/// One `name = 'value'` entry of a TAG list (already trimmed).
fn parse_tag_entry(entry: &str, base_offset: usize) -> Result<Tag, ParseError> {
    // The `=` that ends the name: the first one outside a quoted part.
    let mut in_quotes = false;
    let eq = entry.bytes().position(|b| {
        if b == b'"' {
            in_quotes = !in_quotes;
        }
        b == b'=' && !in_quotes
    });
    let Some(eq) = eq else {
        return Err(ParseError {
            message: format!("Expected '=' in TAG entry '{entry}'. Expected name = 'value'."),
            position: Some(base_offset),
        });
    };
    let name_text = entry[..eq].trim();
    let name = parse_tag_name(name_text).map_err(|e| ParseError {
        message: format!("Invalid tag name '{name_text}': {e}."),
        position: Some(base_offset),
    })?;
    let after_eq = entry[eq + 1..].trim_start();
    let value_offset = base_offset + crate::util::byte_offset_within(entry, after_eq);
    let (value, consumed) =
        crate::util::extract_single_quoted_prefix(after_eq).map_err(|e| ParseError {
            message: match e {
                crate::util::SingleQuoteError::NotQuoted => {
                    format!("Expected a single-quoted string as the value of tag '{name_text}'.")
                }
                crate::util::SingleQuoteError::Unterminated => {
                    "Unclosed single-quoted string.".to_string()
                }
            },
            position: Some(value_offset),
        })?;
    let rest = after_eq[consumed..].trim();
    if !rest.is_empty() {
        return Err(ParseError {
            message: format!("Unexpected text '{rest}' after the value of tag '{name_text}'."),
            position: Some(base_offset + crate::util::byte_offset_within(entry, rest)),
        });
    }
    Ok(Tag { name, value })
}

/// A tag name: one to three dot-separated parts, each a bare identifier or a
/// double-quoted one.
fn parse_tag_name(text: &str) -> Result<TagName, String> {
    if text.is_empty() {
        return Err("the name is missing".to_string());
    }
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
    }
    let mut parts: Vec<String> = parts.into_iter().map(|(p, _)| p).collect();
    let tag = parts.pop().unwrap_or_default();
    let schema = parts.pop();
    let database = parts.pop();
    if !parts.is_empty() {
        return Err("a tag name has at most three parts (database.schema.tag)".to_string());
    }
    Ok(TagName {
        database,
        schema,
        tag,
    })
}

/// Byte offset in `text` where an entry's trailing annotations begin: the
/// first depth-0, unquoted `COMMENT`, `LABELS`, `WITH SYNONYMS`, `WITH TAG` or
/// `TAG (` at a word boundary. Everything before it is the entry's expression.
///
/// Shared with the YAML choke point (`crate::model`), which must refuse an
/// expression the keyword grammar would split here.
pub(crate) fn find_annotation_start(text: &str) -> Option<usize> {
    // Find the FIRST occurrence of COMMENT or WITH SYNONYMS at depth-0 with word boundaries.
    // Scan forward tracking depth to find annotation region start. Quote-aware
    // (PA-6/PA-9): keyword text inside `'...'` string literals or `"..."`
    // quoted identifiers does not match — a column literally named `comment`
    // is usable at depth 0 when quoted (`o."comment"`).
    let mut depth: i32 = 0;
    let mut st = QuoteState::default();
    let bytes = text.as_bytes();
    let upper = text.to_ascii_uppercase();
    let upper_bytes = upper.as_bytes();
    let mut annotation_start: Option<usize> = None;
    let mut i = 0;

    while i < bytes.len() {
        let (next, live) = st.step(bytes, i);
        if live {
            match bytes[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }

            // At depth 0, outside quoted regions, check for COMMENT or WITH keyword
            if depth == 0 {
                // Check for COMMENT keyword with word boundaries
                if i + 7 <= bytes.len() && &upper_bytes[i..i + 7] == b"COMMENT" {
                    let before_ok = annotation_boundary_before(bytes, i);
                    let after_ok = i + 7 == bytes.len() || !is_ident_continuation(bytes[i + 7]);
                    if before_ok && after_ok && annotation_start.is_none() {
                        annotation_start = Some(i);
                    }
                }
                // Check for LABELS keyword (LABELS = (FILTER))
                if i + 6 <= bytes.len() && &upper_bytes[i..i + 6] == b"LABELS" {
                    let before_ok = annotation_boundary_before(bytes, i);
                    let after_ok = i + 6 == bytes.len() || !is_ident_continuation(bytes[i + 6]);
                    if before_ok && after_ok && annotation_start.is_none() {
                        annotation_start = Some(i);
                    }
                }
                // Check for WITH keyword (for WITH SYNONYMS / WITH TAG)
                if i + 4 <= bytes.len() && &upper_bytes[i..i + 4] == b"WITH" {
                    let before_ok = annotation_boundary_before(bytes, i);
                    let after_ok = i + 4 == bytes.len() || !is_ident_continuation(bytes[i + 4]);
                    if before_ok && after_ok {
                        // Verify it's WITH SYNONYMS or WITH TAG, not just any WITH
                        let after_with = upper[i + 4..].trim_start();
                        if (after_with.starts_with("SYNONYMS")
                            || starts_with_keyword(after_with, "TAG"))
                            && annotation_start.is_none()
                        {
                            annotation_start = Some(i);
                        }
                    }
                }
                // Check for a bare TAG keyword. Only `TAG (<name> =` counts: `TAG`
                // alone is an ordinary name (`o.d AS tag`), and `tag(o.x)` is a
                // call to a macro or UDF named `tag`.
                if i + 3 <= bytes.len()
                    && &upper_bytes[i..i + 3] == b"TAG"
                    && annotation_boundary_before(bytes, i)
                    && opens_tag_list(&text[i + 3..])
                    && annotation_start.is_none()
                {
                    annotation_start = Some(i);
                }
            }
        }
        i = next;
    }
    annotation_start
}

/// Separate the SQL expression from trailing COMMENT / WITH SYNONYMS annotations.
///
/// Input: the text after "AS" in an entry (e.g., "SUM(o.amount) COMMENT = 'test' WITH SYNONYMS = ('a')")
/// Output: (`clean_expression`, `ParsedAnnotations`)
///
/// Handles:
/// - COMMENT = 'string with ''escaped'' quotes'
/// - WITH SYNONYMS = ('syn1', 'syn2')
/// - LABELS = (FILTER)
/// - Any order (the clauses must tile the annotation region, but not in a fixed sequence)
/// - No annotations at all (returns original expression with empty annotations)
/// - COMMENT as an identifier inside expressions (only matches at depth-0 with word boundaries)
///
/// Once the annotation region begins, it must be tiled exactly by recognized
/// clauses separated by whitespace: a duplicate `COMMENT` / `WITH SYNONYMS`, a
/// malformed clause, or any leftover text is an error (P-2) — none of it is
/// silently dropped.
///
/// `base_offset` is the absolute byte offset of `text[0]` in the original
/// query; every error caret in this subtree is recovered from the offending
/// token's position within `text` (F-15, code-review 2026-07-16).
pub(super) fn parse_trailing_annotations(
    text: &str,
    base_offset: usize,
) -> Result<(String, ParsedAnnotations), ParseError> {
    parse_annotations(text, base_offset, false)
}

/// [`parse_trailing_annotations`] for text known to begin at an annotation
/// keyword, such as the view-level annotations after the last clause, where no
/// expression can precede them. There a bare `TAG` always opens the clause, so
/// a malformed list (`TAG ()`) is reported as a TAG error rather than being
/// taken for a call to a function named `tag`.
pub(super) fn parse_leading_annotations(
    text: &str,
    base_offset: usize,
) -> Result<(String, ParsedAnnotations), ParseError> {
    parse_annotations(text, base_offset, true)
}

#[allow(clippy::too_many_lines)]
fn parse_annotations(
    text: &str,
    base_offset: usize,
    at_keyword: bool,
) -> Result<(String, ParsedAnnotations), ParseError> {
    // Re-anchor past any leading whitespace `trim` will drop, so `base_offset`
    // remains the absolute offset of `text[0]` after trimming.
    let base_offset = base_offset + (text.len() - text.trim_start().len());
    let text = text.trim();
    // Absolute caret for any subslice of `text` (F-15).
    let pos_of = |sub: &str| base_offset + crate::util::byte_offset_within(text, sub);
    let annotation_start = if at_keyword {
        Some(0)
    } else {
        find_annotation_start(text)
    };

    let (expr_text, annotation_text) = if let Some(start) = annotation_start {
        (text[..start].trim(), &text[start..])
    } else {
        return Ok((text.to_string(), ParsedAnnotations::default()));
    };

    // Parse the annotation region as a sequence of clauses that must TILE it:
    // each is `COMMENT = '...'` or `WITH SYNONYMS = (...)`, separated only by
    // whitespace. A duplicate clause, a malformed clause, or ANY leftover
    // non-whitespace text is a hard error rather than being silently discarded
    // (P-2, code-review 2026-07-11). Previously only the FIRST COMMENT / first
    // WITH SYNONYMS was read: a second `COMMENT = '...'` was dropped and
    // trailing junk (`COMMENT = 'a' banana`) was accepted.
    let mut comment: Option<String> = None;
    let mut synonyms: Option<Vec<String>> = None;
    let mut labels_seen = false;
    let mut is_filter = false;
    let mut tags: Option<Vec<Tag>> = None;
    let mut rest = annotation_text;

    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let rest_upper = rest.to_ascii_uppercase();

        if starts_with_keyword(&rest_upper, "COMMENT") {
            if comment.is_some() {
                return Err(ParseError {
                    message: "Duplicate COMMENT annotation.".to_string(),
                    position: Some(pos_of(rest)),
                });
            }
            // `COMMENT` is 7 ASCII bytes, so slicing at 7 is on a char boundary.
            let after_kw = rest[7..].trim_start();
            let Some(after_eq) = after_kw.strip_prefix('=') else {
                return Err(ParseError {
                    message: "Expected '=' after COMMENT keyword.".to_string(),
                    position: Some(pos_of(after_kw)),
                });
            };
            let after_eq = after_eq.trim_start();
            if !after_eq.starts_with('\'') {
                return Err(ParseError {
                    message: "Expected single-quoted string after COMMENT =.".to_string(),
                    position: Some(pos_of(after_eq)),
                });
            }
            let (content, consumed) =
                crate::util::extract_single_quoted_prefix(after_eq).map_err(|e| ParseError {
                    message: match e {
                        crate::util::SingleQuoteError::NotQuoted => {
                            "Expected single-quoted string after COMMENT =.".to_string()
                        }
                        crate::util::SingleQuoteError::Unterminated => {
                            "Unclosed single-quoted string.".to_string()
                        }
                    },
                    position: Some(pos_of(after_eq)),
                })?;
            comment = Some(content);
            rest = &after_eq[consumed..];
        } else if starts_with_keyword(&rest_upper, "WITH") {
            // `WITH` is 4 ASCII bytes.
            let after_with = rest[4..].trim_start();
            if starts_with_keyword(&after_with.to_ascii_uppercase(), "TAG") {
                // `WITH TAG (...)`: the same clause as a bare `TAG (...)`.
                rest = parse_tag_clause(after_with, &mut tags, pos_of(after_with))?;
                continue;
            }
            if !starts_with_keyword(&after_with.to_ascii_uppercase(), "SYNONYMS") {
                return Err(ParseError {
                    message: "Expected SYNONYMS or TAG after WITH keyword.".to_string(),
                    position: Some(pos_of(after_with)),
                });
            }
            if synonyms.is_some() {
                return Err(ParseError {
                    message: "Duplicate WITH SYNONYMS annotation.".to_string(),
                    position: Some(pos_of(rest)),
                });
            }
            // `SYNONYMS` is 8 ASCII bytes. Snowflake makes the `=` optional, so
            // both `WITH SYNONYMS = (...)` and `WITH SYNONYMS (...)` are
            // accepted (F-12, code-review 2026-07-16).
            let after_synonyms = after_with[8..].trim_start();
            let after_eq = after_synonyms
                .strip_prefix('=')
                .unwrap_or(after_synonyms)
                .trim_start();
            let (content, consumed) = extract_paren_prefix(after_eq).ok_or_else(|| ParseError {
                message: "Expected parenthesized list after WITH SYNONYMS.".to_string(),
                position: Some(pos_of(after_eq)),
            })?;
            synonyms = Some(parse_synonym_list(content, pos_of(content))?);
            rest = &after_eq[consumed..];
        } else if starts_with_keyword(&rest_upper, "TAG") {
            rest = parse_tag_clause(rest, &mut tags, pos_of(rest))?;
        } else if starts_with_keyword(&rest_upper, "LABELS") {
            if labels_seen {
                return Err(ParseError {
                    message: "Duplicate LABELS annotation.".to_string(),
                    position: Some(pos_of(rest)),
                });
            }
            // `LABELS` is 6 ASCII bytes. The `=` is optional, matching the
            // WITH SYNONYMS relaxation (F-12).
            let after_kw = rest[6..].trim_start();
            let after_eq = after_kw.strip_prefix('=').unwrap_or(after_kw).trim_start();
            let (content, consumed) = extract_paren_prefix(after_eq).ok_or_else(|| ParseError {
                message: "Expected parenthesized list after LABELS.".to_string(),
                position: Some(pos_of(after_eq)),
            })?;
            is_filter = parse_label_list(content, pos_of(content))?;
            labels_seen = true;
            rest = &after_eq[consumed..];
        } else {
            return Err(ParseError {
                message: format!(
                    "Unexpected text in annotations: '{rest}'. Expected COMMENT = '...', WITH SYNONYMS = (...), LABELS = (FILTER), or [WITH] TAG (name = 'value', ...)."
                ),
                position: Some(pos_of(rest)),
            });
        }
    }

    Ok((
        expr_text.to_string(),
        ParsedAnnotations {
            comment,
            synonyms: synonyms.unwrap_or_default(),
            tags: tags.unwrap_or_default(),
            is_filter,
        },
    ))
}

/// Read one `TAG ( ... )` clause from `text`, which starts at the `TAG` keyword
/// (`WITH`, if written, is already consumed), into `tags`. Returns the text
/// after the closing parenthesis. `base_offset` is the absolute offset of
/// `text[0]`.
fn parse_tag_clause<'t>(
    text: &'t str,
    tags: &mut Option<Vec<Tag>>,
    base_offset: usize,
) -> Result<&'t str, ParseError> {
    if tags.is_some() {
        return Err(ParseError {
            message: "Duplicate TAG annotation.".to_string(),
            position: Some(base_offset),
        });
    }
    // `TAG` is 3 ASCII bytes.
    let after_kw = text[3..].trim_start();
    let list_offset = base_offset + crate::util::byte_offset_within(text, after_kw);
    let (content, consumed) = extract_paren_prefix(after_kw).ok_or_else(|| ParseError {
        message: "Expected parenthesized list after TAG.".to_string(),
        position: Some(list_offset),
    })?;
    *tags = Some(parse_tag_list(
        content,
        base_offset + crate::util::byte_offset_within(text, content),
    )?);
    Ok(&after_kw[consumed..])
}

/// Whether `rest` (the text after a bare `TAG`) opens a tag list: `(`, a
/// possibly qualified, possibly quoted name, then `=` (not `==`). That is what
/// tells `TAG (t = 'v')` from a call such as `tag(o.x)`.
fn opens_tag_list(rest: &str) -> bool {
    let Some(inner) = rest.trim_start().strip_prefix('(') else {
        return false;
    };
    let bytes = inner.trim_start().as_bytes();
    let mut pos = 0;
    loop {
        if bytes.get(pos) == Some(&b'"') {
            pos += 1;
            loop {
                match bytes.get(pos) {
                    None => return false,
                    Some(b'"') if bytes.get(pos + 1) == Some(&b'"') => pos += 2,
                    Some(b'"') => {
                        pos += 1;
                        break;
                    }
                    Some(_) => pos += 1,
                }
            }
        } else {
            // A part may be empty here (`TAG (a..b = '1')`, `TAG (= '1')`): no
            // call reads that way, and the clause parser reports it precisely.
            while bytes.get(pos).is_some_and(|&b| is_ident_continuation(b)) {
                pos += 1;
            }
        }
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        match bytes.get(pos) {
            Some(b'.') => {
                pos += 1;
                while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
                    pos += 1;
                }
            }
            Some(b'=') => return bytes.get(pos + 1) != Some(&b'='),
            _ => return false,
        }
    }
}

/// True when `upper` (already ASCII-uppercased) begins with `keyword` (also
/// uppercase) at a word boundary — i.e. the byte after the keyword, if any, is
/// not an identifier-continuation byte. Prevents `COMMENTARY` from matching
/// `COMMENT` / `WITHDRAW` from matching `WITH`.
fn starts_with_keyword(upper: &str, keyword: &str) -> bool {
    let ub = upper.as_bytes();
    let kb = keyword.as_bytes();
    ub.len() >= kb.len()
        && &ub[..kb.len()] == kb
        && (ub.len() == kb.len() || !is_ident_continuation(ub[kb.len()]))
}

/// Check for a leading PRIVATE or PUBLIC keyword on an entry.
/// Returns (`AccessModifier`, `remaining_entry_text`).
/// Disambiguates table aliases starting with "private" or "public" by checking
/// if the next non-whitespace character is '.' (indicating a qualified identifier).
pub(super) fn parse_leading_access_modifier(entry: &str) -> (AccessModifier, &str) {
    let entry_upper = entry.to_ascii_uppercase();
    if entry_upper.starts_with("PRIVATE") {
        let after = &entry["PRIVATE".len()..];
        if after.starts_with(|c: char| c.is_ascii_whitespace()) {
            let trimmed_after = after.trim_start();
            if trimmed_after.starts_with('.') {
                // 'PRIVATE' is a table alias like private_table.metric
                (AccessModifier::Public, entry)
            } else {
                (AccessModifier::Private, trimmed_after)
            }
        } else if after.is_empty() {
            (AccessModifier::Private, after)
        } else {
            // e.g., "PRIVATEMETRIC" or "PRIVATE.x" -- not a keyword
            (AccessModifier::Public, entry)
        }
    } else if entry_upper.starts_with("PUBLIC") {
        let after = &entry["PUBLIC".len()..];
        if after.starts_with(|c: char| c.is_ascii_whitespace()) {
            let trimmed_after = after.trim_start();
            if trimmed_after.starts_with('.') {
                (AccessModifier::Public, entry)
            } else {
                (AccessModifier::Public, trimmed_after)
            }
        } else if after.is_empty() {
            (AccessModifier::Public, after)
        } else {
            (AccessModifier::Public, entry)
        }
    } else {
        (AccessModifier::Public, entry)
    }
}

#[cfg(test)]
mod label_tests {
    use super::parse_trailing_annotations;

    fn parse(text: &str) -> (String, bool) {
        let (expr, ann) = parse_trailing_annotations(text, 0).expect("should parse");
        (expr, ann.is_filter)
    }

    #[test]
    fn labels_filter_sets_the_flag_and_is_stripped_from_the_expression() {
        let (expr, is_filter) = parse("o.amount > 100 LABELS = (FILTER)");
        assert_eq!(expr, "o.amount > 100");
        assert!(is_filter);
    }

    #[test]
    fn labels_is_case_insensitive_and_the_equals_is_optional() {
        // The `=` relaxation mirrors WITH SYNONYMS (F-12).
        for text in [
            "x LABELS = (FILTER)",
            "x labels = (filter)",
            "x LABELS (FILTER)",
            "x Labels(Filter)",
        ] {
            let (expr, is_filter) = parse(text);
            assert_eq!(expr, "x", "for {text:?}");
            assert!(is_filter, "for {text:?}");
        }
    }

    #[test]
    fn absent_labels_leaves_the_flag_false() {
        let (expr, is_filter) = parse("o.amount > 100");
        assert_eq!(expr, "o.amount > 100");
        assert!(!is_filter);
    }

    #[test]
    fn labels_composes_with_comment_and_synonyms_in_any_order() {
        for text in [
            "x LABELS = (FILTER) COMMENT = 'c' WITH SYNONYMS = ('s')",
            "x COMMENT = 'c' LABELS = (FILTER) WITH SYNONYMS = ('s')",
            "x WITH SYNONYMS = ('s') COMMENT = 'c' LABELS = (FILTER)",
        ] {
            let (expr, ann) = parse_trailing_annotations(text, 0).expect("should parse");
            assert_eq!(expr, "x", "for {text:?}");
            assert!(ann.is_filter, "for {text:?}");
            assert_eq!(ann.comment.as_deref(), Some("c"), "for {text:?}");
            assert_eq!(ann.synonyms, vec!["s".to_string()], "for {text:?}");
        }
    }

    #[test]
    fn an_unsupported_label_is_rejected_rather_than_ignored() {
        // Silently dropping a label would let a definition round-trip having
        // quietly lost meaning. Snowflake's tags and other LABELS values are
        // out of scope, so they must fail loudly.
        let err = parse_trailing_annotations("x LABELS = (TAG)", 0).unwrap_err();
        assert!(
            err.message.contains("Unsupported label 'TAG'"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn an_empty_label_list_is_rejected() {
        let err = parse_trailing_annotations("x LABELS = ()", 0).unwrap_err();
        assert!(err.message.contains("empty"), "got: {}", err.message);
    }

    #[test]
    fn a_duplicate_labels_clause_is_rejected() {
        let err =
            parse_trailing_annotations("x LABELS = (FILTER) LABELS = (FILTER)", 0).unwrap_err();
        assert!(
            err.message.contains("Duplicate LABELS"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn labels_as_a_quoted_column_name_is_not_an_annotation() {
        // The detection scan is quote-aware (PA-6/PA-9): a column literally
        // named `labels` stays part of the expression.
        let (expr, is_filter) = parse("o.\"labels\" IS NOT NULL");
        assert_eq!(expr, "o.\"labels\" IS NOT NULL");
        assert!(!is_filter);
    }
}

#[cfg(test)]
mod tag_tests {
    use super::parse_trailing_annotations;
    use crate::model::{Tag, TagName};

    fn tag(database: Option<&str>, schema: Option<&str>, name: &str, value: &str) -> Tag {
        Tag {
            name: TagName {
                database: database.map(str::to_string),
                schema: schema.map(str::to_string),
                tag: name.to_string(),
            },
            value: value.to_string(),
        }
    }

    fn tags_of(text: &str) -> (String, Vec<Tag>) {
        let (expr, ann) = parse_trailing_annotations(text, 0).expect("should parse");
        (expr, ann.tags)
    }

    fn err_of(text: &str) -> (String, Option<usize>) {
        let err = parse_trailing_annotations(text, 0).unwrap_err();
        (err.message, err.position)
    }

    #[test]
    fn with_tag_and_bare_tag_both_parse_and_are_stripped_from_the_expression() {
        for text in [
            "o.region WITH TAG (s_layer_kind = 'dimension')",
            "o.region TAG (s_layer_kind = 'dimension')",
            "o.region with tag(s_layer_kind='dimension')",
            "o.region Tag (s_layer_kind = 'dimension')",
        ] {
            let (expr, tags) = tags_of(text);
            assert_eq!(expr, "o.region", "for {text:?}");
            assert_eq!(
                tags,
                vec![tag(None, None, "s_layer_kind", "dimension")],
                "for {text:?}"
            );
        }
    }

    #[test]
    fn tags_keep_their_order_and_qualified_names_split_into_parts() {
        let (_, tags) = tags_of(
            "x WITH TAG (S_LAYER_KIND = 'k', gov.pii = 'none', db.gov.\"Owner Tag\" = 'a''b')",
        );
        assert_eq!(
            tags,
            vec![
                tag(None, None, "S_LAYER_KIND", "k"),
                tag(None, Some("gov"), "pii", "none"),
                tag(Some("db"), Some("gov"), "Owner Tag", "a'b"),
            ]
        );
    }

    #[test]
    fn tag_composes_with_the_other_annotations_in_any_order() {
        for text in [
            "x WITH SYNONYMS = ('s') WITH TAG (t = 'v') COMMENT = 'c'",
            "x COMMENT = 'c' TAG (t = 'v') WITH SYNONYMS = ('s')",
            "x TAG (t = 'v') LABELS = (FILTER) COMMENT = 'c' WITH SYNONYMS = ('s')",
        ] {
            let (expr, ann) = parse_trailing_annotations(text, 0).expect("should parse");
            assert_eq!(expr, "x", "for {text:?}");
            assert_eq!(ann.tags, vec![tag(None, None, "t", "v")], "for {text:?}");
            assert_eq!(ann.comment.as_deref(), Some("c"), "for {text:?}");
            assert_eq!(ann.synonyms, vec!["s".to_string()], "for {text:?}");
        }
    }

    #[test]
    fn a_name_called_tag_is_still_an_expression() {
        // Only `TAG (` begins the clause; a bare or qualified `tag` is a name.
        let mut failures = Vec::new();
        for text in [
            "tag",
            "o.tag",
            "o.tag + 1",
            "CONCAT(tag, 'x')",
            "o.\"tag\" (1)",
            // A call to a user macro or UDF named `tag` is an expression too:
            // only `TAG (` followed by a tag name and `=` begins the clause.
            "tag(o.x)",
            "SUM(o.a) + tag (o.y)",
            "tag()",
            "tag(o.x, 'k')",
            "tag(\"o\".x)",
        ] {
            // Every case is checked before failing, so each one reports.
            match parse_trailing_annotations(text, 0) {
                Ok((expr, ann)) if expr == text && ann.tags.is_empty() => {}
                other => failures.push(format!("{text:?}: {other:?}")),
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_tag_value_may_hold_annotation_keywords_and_punctuation() {
        let (expr, tags) = tags_of("x TAG (t = 'COMMENT = ''y'', TAG (z), WITH SYNONYMS')");
        assert_eq!(expr, "x");
        assert_eq!(
            tags,
            vec![tag(
                None,
                None,
                "t",
                "COMMENT = 'y', TAG (z), WITH SYNONYMS"
            )]
        );
    }

    #[test]
    fn a_value_of_256_characters_is_accepted_and_257_is_refused() {
        let ok = format!("x TAG (t = '{}')", "é".repeat(256));
        assert_eq!(tags_of(&ok).1[0].value.chars().count(), 256);
        let long = format!("x TAG (t = '{}')", "é".repeat(257));
        let (message, position) = err_of(&long);
        assert!(message.contains("257 characters"), "got: {message}");
        assert_eq!(position, Some(7), "caret at the entry");
    }

    #[test]
    fn duplicate_tag_names_are_refused_case_insensitively() {
        let (message, position) = err_of("x TAG (a.t = '1', A.\"T\" = '2')");
        assert!(message.contains("duplicate tag 'A.T'"), "got: {message}");
        assert_eq!(position, Some(18));
        // Different qualifiers are different names.
        assert_eq!(tags_of("x TAG (a.t = '1', t = '2')").1.len(), 2);
    }

    #[test]
    fn a_second_tag_clause_is_refused() {
        let (message, _) = err_of("x TAG (a = '1') WITH TAG (b = '2')");
        assert!(message.contains("Duplicate TAG"), "got: {message}");
    }

    #[test]
    fn malformed_tag_lists_are_refused() {
        // Lists that do not begin `<name> =` are written with WITH: after a
        // member's expression a bare `TAG (` opens the clause only when a name
        // and `=` follow (otherwise it is a call to a function named `tag`).
        for (text, needle) in [
            ("x WITH TAG ()", "TAG () is empty"),
            ("x TAG (a = '1',)", "Empty entry"),
            ("x WITH TAG (a '1')", "Expected '='"),
            ("x TAG (a = 1)", "single-quoted string"),
            ("x TAG (a = '1' b)", "Unexpected text 'b'"),
            ("x TAG (= '1')", "the name is missing"),
            ("x WITH TAG (a b = '1')", "is not an identifier"),
            ("x WITH TAG (1a = '1')", "is not an identifier"),
            ("x TAG (a.b.c.d = '1')", "at most three parts"),
            ("x TAG (a..b = '1')", "Invalid tag name"),
            ("x TAG (\"\" = '1')", "Invalid tag name"),
            (
                "x WITH TAG a = '1'",
                "Expected parenthesized list after TAG",
            ),
            (
                "x COMMENT = 'c' WITH TAGS (a = '1')",
                "Expected SYNONYMS or TAG",
            ),
        ] {
            let (message, _) = err_of(text);
            assert!(message.contains(needle), "for {text:?}: got {message}");
        }
    }

    #[test]
    fn a_quoted_name_may_hold_an_equals_sign() {
        let (_, tags) = tags_of("x TAG (\"a=b\" = 'v')");
        assert_eq!(tags, vec![tag(None, None, "a=b", "v")]);
    }
}
