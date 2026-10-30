//! Prefix detection for `CREATE … SEMANTIC VIEW` (AR-1).
//!
//! The grammar productions (`crate::grammar`) recognise every semantic-view
//! statement. CREATE's body still goes to the Rust body parser as text, and
//! the helpers here find where that text's prefix ends: keyword prefix
//! matching ([`match_keyword_prefix`]), whitespace skipping
//! ([`skip_leading_whitespace`]) and the longest-first CREATE prefix table
//! ([`detect_create_prefix`]).

use super::DdlKind;
use crate::util::is_ident_byte;

/// Match a fixed sequence of keyword tokens at the start of `input`, tolerating
/// arbitrary ASCII whitespace between tokens.
///
/// Returns `Some(bytes_consumed)` if all keywords matched (case-insensitively),
/// where `bytes_consumed` is the number of bytes consumed by the keyword prefix
/// (including inter-keyword whitespace). Returns `None` otherwise.
///
/// The match anchors at position 0. Leading whitespace in `input` is consumed
/// as part of the match (counted in the returned byte count). If the caller has
/// already trimmed leading whitespace, the returned count is from offset 0 of
/// the trimmed slice.
///
/// Anti-pattern avoided: does NOT scan at increasing offsets (no O(n^2) behavior).
/// If keyword[0] doesn't match at the start (after whitespace), returns None.
///
/// Note: only handles ASCII whitespace (0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x20).
/// In the extension the text is rebuilt from `DuckDB`'s tokens, which carry no
/// Unicode whitespace.
pub(crate) fn match_keyword_prefix(input: &[u8], keywords: &[&[u8]]) -> Option<usize> {
    let mut pos = 0;
    for (i, &kw) in keywords.iter().enumerate() {
        // Skip ASCII whitespace (but not before the first keyword -- caller is
        // responsible for leading whitespace; we skip INTER-keyword whitespace
        // only for i > 0).
        if i > 0 {
            // Require at least one whitespace character between keywords.
            if pos >= input.len() || !input[pos].is_ascii_whitespace() {
                return None;
            }
            while pos < input.len() && input[pos].is_ascii_whitespace() {
                pos += 1;
            }
        }
        // Match keyword case-insensitively.
        if input.len() < pos + kw.len() {
            return None;
        }
        if !input[pos..pos + kw.len()].eq_ignore_ascii_case(kw) {
            return None;
        }
        pos += kw.len();
    }
    // Require a word boundary after the FINAL keyword. Inter-keyword
    // boundaries are already enforced by the mandatory whitespace above,
    // but without this check `CREATE SEMANTIC VIEWfoo` matched, and the
    // plural typo `DROP SEMANTIC VIEWS` matched the `DROP SEMANTIC VIEW`
    // prefix and dropped a view named `s` (PA-4, code-review 2026-07-02).
    // Non-ASCII bytes (>= 0x80) are identifier continuation in DuckDB, so
    // they are NOT boundaries either; ASCII punctuation (whitespace, `(`,
    // `;`, `"`) is a legitimate token boundary and stays accepted.
    if pos < input.len() && is_ident_byte(input[pos]) {
        return None;
    }
    Some(pos)
}

/// Return the byte offset of the first non-whitespace character in `input`.
///
/// Every caller runs [`crate::util::blank_sql_comments`] first, which replaces
/// each comment byte with a space (byte-length-preserving), so by the time input
/// reaches here `-- ...` and `/* ... */` comments are already whitespace. This
/// therefore only needs to skip leading ASCII whitespace — it formerly
/// re-implemented comment scanning inline, but that branch was dead (comments
/// were already blanked) *and* wrong: it treated `/* */` as non-nesting, the
/// opposite of the nesting semantics `blank_sql_comments` (and PostgreSQL/DuckDB)
/// actually apply (P-5, code-review 2026-07-11).
///
/// The returned offset is into the *original* (blanked, length-preserving)
/// slice, so error-caret positions still point into the statement's text after
/// leading whitespace and comments are skipped.
pub(crate) fn skip_leading_whitespace(input: &str) -> usize {
    input.bytes().take_while(u8::is_ascii_whitespace).count()
}

/// Detect which CREATE form a statement is, and how many bytes its prefix
/// takes.
///
/// The input must already be trimmed of leading/trailing whitespace and
/// trailing semicolons. Returns `Some((DdlKind, consumed_bytes))` where
/// `consumed_bytes` is the number of bytes consumed by the matched prefix
/// (including any inter-keyword whitespace in the input). Returns `None`
/// if the statement is not a CREATE.
///
/// Longest-first ordering prevents prefix overlap.
pub(crate) fn detect_create_prefix(trimmed: &str) -> Option<(DdlKind, usize)> {
    let b = trimmed.as_bytes();

    // CREATE OR REPLACE SEMANTIC VIEW (5 keywords) -- before CREATE SEMANTIC VIEW
    if let Some(n) = match_keyword_prefix(b, &[b"create", b"or", b"replace", b"semantic", b"view"])
    {
        return Some((DdlKind::CreateOrReplace, n));
    }
    // CREATE OR ALTER SEMANTIC VIEW (5 keywords) -- before CREATE SEMANTIC VIEW
    if let Some(n) = match_keyword_prefix(b, &[b"create", b"or", b"alter", b"semantic", b"view"]) {
        return Some((DdlKind::CreateOrAlter, n));
    }
    // CREATE SEMANTIC VIEW IF NOT EXISTS (6 keywords) -- before CREATE SEMANTIC VIEW
    if let Some(n) = match_keyword_prefix(
        b,
        &[b"create", b"semantic", b"view", b"if", b"not", b"exists"],
    ) {
        return Some((DdlKind::CreateIfNotExists, n));
    }
    // CREATE SEMANTIC VIEW (3 keywords)
    if let Some(n) = match_keyword_prefix(b, &[b"create", b"semantic", b"view"]) {
        return Some((DdlKind::Create, n));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::detect_create_prefix;
    use crate::parse::DdlKind;

    fn kind(s: &str) -> Option<DdlKind> {
        detect_create_prefix(s).map(|(k, _)| k)
    }

    #[test]
    fn each_create_form_is_detected_longest_first() {
        assert_eq!(kind("CREATE SEMANTIC VIEW v AS"), Some(DdlKind::Create));
        assert_eq!(
            kind("CREATE OR REPLACE SEMANTIC VIEW v AS"),
            Some(DdlKind::CreateOrReplace)
        );
        assert_eq!(
            kind("CREATE SEMANTIC VIEW IF NOT EXISTS v AS"),
            Some(DdlKind::CreateIfNotExists)
        );
        assert_eq!(
            kind("CREATE OR ALTER SEMANTIC VIEW v AS"),
            Some(DdlKind::CreateOrAlter)
        );
    }

    #[test]
    fn keywords_match_in_any_case_and_across_any_whitespace() {
        let (k, n) = detect_create_prefix("create\t Semantic\n\r VIEW v").unwrap();
        assert_eq!(k, DdlKind::Create);
        assert_eq!(n, "create\t Semantic\n\r VIEW".len());
    }

    #[test]
    fn a_keyword_must_end_at_a_word_boundary() {
        assert_eq!(kind("CREATE SEMANTIC VIEWS v"), None);
        assert_eq!(kind("CREATE SEMANTICS VIEW v"), None);
    }

    #[test]
    fn other_statements_are_not_creates() {
        assert_eq!(kind("DROP SEMANTIC VIEW v"), None);
        assert_eq!(kind("SELECT 1"), None);
        assert_eq!(kind("CREATE VIEW v AS SELECT 1"), None);
        assert_eq!(kind(""), None);
    }
}
