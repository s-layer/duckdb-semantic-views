//! Shared string utilities for fuzzy matching and identifier-boundary
//! classification.
//!
//! Extracted from `expand.rs` to break the expand <-> graph circular dependency.
//! Both `expand` and `graph` modules import from here.

/// Suggest the closest matching name from `available` using Levenshtein distance.
///
/// Returns `Some(name)` (with original casing) if the best match has an edit
/// distance of 3 or fewer characters. Returns `None` if no candidate is close
/// enough. Both the query and candidates are lowercased for comparison.
#[must_use]
pub fn suggest_closest(name: &str, available: &[String]) -> Option<String> {
    let query = name.to_ascii_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for candidate in available {
        let dist = strsim::levenshtein(&query, &candidate.to_ascii_lowercase());
        if dist <= 3 {
            if let Some((best_dist, _)) = best {
                if dist < best_dist {
                    best = Some((dist, candidate));
                }
            } else {
                best = Some((dist, candidate));
            }
        }
    }
    best.map(|(_, s)| s.to_string())
}

/// Is `b` an identifier-continuation byte?
///
/// **This is the single source of truth for "what byte continues a SQL
/// identifier"** across the whole crate — the DDL keyword scanners
/// (`body_parser::scan::is_ident_continuation` delegates here), the
/// prefix matcher (`parse::match_keyword_prefix`), and the reference
/// tokenizer that drives fact/derived-metric inlining (`expr_tokens`) all
/// resolve through it.
/// Keeping one definition is what prevents the recurring boundary-drift
/// bug class (PR #50 review): a keyword must not match immediately before
/// an identifier byte, or `AS`/`BY`/`id` matches inside `ASx`/`BYé`/`idΩ`.
///
/// Continuation = ASCII alphanumerics, `_`, AND every non-ASCII byte
/// (>= 0x80): `DuckDB` identifiers may contain any non-ASCII character, so
/// UTF-8 lead/continuation bytes are identifier bytes, never boundaries.
#[must_use]
pub fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Whether the `'` at `quote` opens a `DuckDB` **escape string** (`E'…'`).
///
/// `DuckDB` accepts the Postgres spelling in which `\'` is an escaped quote, so
/// `e'\''` is a terminated one-character literal rather than the start of an
/// unterminated one. Every scanner in this crate previously read `\` as an
/// ordinary byte and consumed the middle and closing quotes as a single `''`
/// pair, so the literal ran to the end of the buffer and everything after it
/// was treated as string content — the PA-3/P-1 silent-mis-split class
/// (PARSE-7, code-review 2026-08-06).
///
/// The `e` must sit on a token boundary. `DATE'2020-01-01'` is a valid TYPED
/// LITERAL whose `E` belongs to the type name, and it follows the ORDINARY
/// string rules — so a preceding identifier byte disqualifies the introducer.
///
/// Note that `''` remains an escape inside an escape string too: `e'a''b'` is
/// `a'b` in `DuckDB`. Both forms must be honoured, not one or the other.
#[must_use]
pub fn opens_escape_string(bytes: &[u8], quote: usize) -> bool {
    if quote == 0 {
        return false;
    }
    let e = bytes[quote - 1];
    if e != b'e' && e != b'E' {
        return false;
    }
    quote < 2 || !is_ident_byte(bytes[quote - 2])
}

/// Byte offset just past a backslash escape at `i` inside an escape string, or
/// `None` when `i` is not such an escape.
///
/// `in_escape_string` is the caller's [`opens_escape_string`] verdict for the
/// literal currently open — in an ORDINARY literal a backslash is data, so this
/// must not fire there.
///
/// The result is CLAMPED to `bytes.len()`: an input whose last byte is the
/// backslash (`e'\`) has nothing to escape, and an unclamped `i + 2` lands one
/// past the buffer. Every scanner shares this one advance precisely so that
/// bound cannot drift between them — when the four loops each carried their own
/// copy, the lexer's was the one that forgot to clamp and produced a token
/// whose `end` could not be sliced (raised by review on #205).
#[must_use]
pub fn escaped_pair_end(bytes: &[u8], i: usize, in_escape_string: bool) -> Option<usize> {
    (in_escape_string && bytes[i] == b'\\').then(|| (i + 2).min(bytes.len()))
}

/// Byte offset of the subslice `inner` within `outer`.
///
/// `inner` MUST be a subslice of `outer` (borrowed from the same allocation,
/// as produced by `&outer[a..b]` / `.trim()` / `.split` etc.). Used to recover
/// an absolute error position from a re-sliced token without threading manual
/// byte counters through every clause scanner (R-2): the parser slices its way
/// down to the offending token, and this maps that token back to an offset in
/// the original query for the caret. `debug_assert`s the subslice relationship;
/// a non-subslice argument is a caller bug, not a runtime condition.
#[must_use]
pub fn byte_offset_within(outer: &str, inner: &str) -> usize {
    let outer_start = outer.as_ptr() as usize;
    let inner_start = inner.as_ptr() as usize;
    debug_assert!(
        inner_start >= outer_start && inner_start + inner.len() <= outer_start + outer.len(),
        "byte_offset_within: `inner` is not a subslice of `outer`"
    );
    inner_start - outer_start
}

/// Is `b` a word-boundary byte — i.e. NOT an [`is_ident_byte`]? The
/// primitive used by the `expand::facts` COUNT / name matchers so they share
/// the parser's notion of an identifier boundary.
#[must_use]
pub fn is_word_boundary_char(b: u8) -> bool {
    !is_ident_byte(b)
}

/// Does `s` start with the ASCII keyword `kw`, case-insensitively?
///
/// Compares raw *bytes*, so it is safe on any UTF-8 input: the old
/// `s[..kw.len()].eq_ignore_ascii_case(kw)` pattern panicked ("byte index N
/// is not a char boundary") whenever a multi-byte character straddled the
/// keyword length (PA-1, code-review 2026-07-02 — e.g. `SHOW SEMANTIC VIEWS
/// aΩΩ`). A multi-byte character can never byte-match an ASCII keyword, so
/// the comparison is also *correct* on non-ASCII input: it simply fails.
///
/// After a `true` return, slicing `s` at `kw.len()` is guaranteed safe —
/// the matched prefix is pure ASCII, so `kw.len()` lands on a char boundary.
#[must_use]
pub fn starts_with_keyword_ci(s: &str, kw: &str) -> bool {
    let n = kw.len();
    s.len() >= n && s.as_bytes()[..n].eq_ignore_ascii_case(kw.as_bytes())
}

/// Length in bytes of the dollar-quote opener `$tag$` at `bytes[start]`, or
/// `None` if there is no valid opener there.
///
/// Returns `None` when `start` is out of bounds or `bytes[start]` is not `$`,
/// so callers may probe any offset without a prior bounds/`$` check. When it
/// returns `Some(len)`, `bytes[start]` was `$` and `&bytes[start..start + len]`
/// is the opener (e.g. `$$`, `$yaml$`). The tag body is ASCII alphanumerics and
/// `_` and may **not** start with a digit — `$1` is a positional parameter, not
/// a dollar-quote tag — matching `PostgreSQL`/`DuckDB`. The empty tag `$$` is
/// valid; the returned length includes both `$` delimiters. A `$` that
/// follows an identifier byte (with only `$`s between) is part of that
/// identifier and opens nothing.
///
/// **Single source of truth for dollar-quote tags** (P-6, code-review
/// 2026-07-11), shared by [`blank_sql_comments`] and the CREATE-body
/// `extract_dollar_quoted` extractor so the two can never disagree about what
/// a valid tag is. Previously the extractor accepted any run between two `$`
/// (including `$1$` and `$ta g$`) while comment-blanking recognized only the
/// stricter form; a body opened with a tag the blanker rejected had its `--`
/// runs blanked as SQL before the extractor stored the (now corrupted) text.
#[must_use]
pub fn read_dollar_tag_len(bytes: &[u8], start: usize) -> Option<usize> {
    if start >= bytes.len() || bytes[start] != b'$' {
        return None;
    }
    // A `$` inside an identifier continues it (`v$a$`, `v$$x$$`), as in
    // DuckDB's scanner, so no dollar quote opens after an identifier byte,
    // with any `$`s between.
    let mut k = start;
    while k > 0 && bytes[k - 1] == b'$' {
        k -= 1;
    }
    if k > 0 && is_ident_byte(bytes[k - 1]) {
        return None;
    }
    let mut j = start + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        // A tag may not START with a digit ($1 is a parameter, not a tag).
        if j == start + 1 && bytes[j].is_ascii_digit() {
            return None;
        }
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'$' {
        Some(j - start + 1)
    } else {
        None
    }
}

/// Byte-scan state for SQL text: tracks single-quoted string literals and
/// double-quoted identifiers, honouring the SQL escape doubling (`''` inside
/// a string, `""` inside a quoted identifier).
///
/// This is the ONE quote-tracking implementation for SQL-text scanning across
/// the crate (PA-6, code-review 2026-07-02; promoted out of `body_parser::scan`
/// for EXP-16/EXP-17/PARSE-4, code-review 2026-08-03): scanners that tracked only
/// single quotes — or nothing — mis-split on quoted identifiers containing
/// commas / parens / dots (`o."a,b"`, `o AS "tbl)x"`, `"a.b"`) and matched
/// keywords inside string literals (PA-3: a `COMMENT = 'the PRIMARY KEY (id)
/// lives here'` fabricated a primary key from comment text).
///
/// Multi-byte UTF-8 is safe by construction: only ASCII bytes are compared,
/// and continuation bytes (>= 0x80) never equal an ASCII quote.
///
/// Dollar-quoted strings (`$tag$ ... $tag$`, PARSE-1 / code-review 2026-07-18)
/// are tracked too: a `,` / `)` / keyword inside one is inert, matching the
/// comment-blanker and the CREATE-body extractor which already share the tag
/// grammar via [`read_dollar_tag_len`]. Without this a comma inside
/// a dimension/metric expression's `$$...$$` split one entry into two garbage
/// entries (the P-1/P-2 silent-mis-parse class).
#[derive(Default, Clone, Copy)]
pub(crate) struct QuoteState {
    pub(crate) in_string: bool,
    pub(crate) in_ident: bool,
    /// When inside a `$tag$ ... $tag$` region, the byte span `[start, end)` of
    /// the OPENING tag within the buffer being scanned; `None` otherwise. Stored
    /// as offsets (not the tag bytes) so `QuoteState` stays `Copy`; the offsets
    /// index the same `bytes` slice passed to every `step` call, which every
    /// scan-in-a-loop caller preserves.
    dollar_open: Option<(usize, usize)>,
    /// Whether the single-quoted literal currently open is a `DuckDB` escape
    /// string (`E'…'`), in which `\\` escapes the following byte. See
    /// [`opens_escape_string`]. Meaningless unless `in_string`.
    string_is_escape: bool,
}

impl QuoteState {
    /// True while inside an unterminated (still-open) dollar-quoted region.
    pub(crate) fn in_dollar(&self) -> bool {
        self.dollar_open.is_some()
    }

    /// Consume the byte at `i`, updating quote state. Returns
    /// `(next_index, is_live_code)` where `is_live_code` is true only when
    /// byte `i` is outside every quoted region and is not itself a quote
    /// delimiter. Escape pairs / whole dollar tags are consumed at once.
    pub(crate) fn step(&mut self, bytes: &[u8], i: usize) -> (usize, bool) {
        let b = bytes[i];
        if let Some((ts, te)) = self.dollar_open {
            // Inside `$tag$...$tag$`: only the IDENTICAL closing tag ends the
            // region — a different inner tag ($z$) or a lone `$` does not.
            if b == b'$' && bytes[i..].starts_with(&bytes[ts..te]) {
                self.dollar_open = None;
                return (i + (te - ts), false); // consume the whole closing tag
            }
            return (i + 1, false);
        }
        if self.in_string {
            // In an escape string a backslash escapes the NEXT byte, whatever it
            // is — including a quote that would otherwise close the literal, and
            // including another backslash (so `\\` is one escaped backslash and
            // does not escape a quote after it).
            if let Some(next) = escaped_pair_end(bytes, i, self.string_is_escape) {
                return (next, false);
            }
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    return (i + 2, false); // '' escape — stay in string (both forms apply)
                }
                self.in_string = false;
            }
            (i + 1, false)
        } else if self.in_ident {
            if b == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    return (i + 2, false); // "" escape — stay in ident
                }
                self.in_ident = false;
            }
            (i + 1, false)
        } else {
            match b {
                b'\'' => {
                    self.in_string = true;
                    self.string_is_escape = opens_escape_string(bytes, i);
                    (i + 1, false)
                }
                b'"' => {
                    self.in_ident = true;
                    (i + 1, false)
                }
                b'$' => {
                    // A valid `$tag$` opener starts a dollar-quoted region; a
                    // lone `$` or a `$1` positional parameter (rejected by
                    // read_dollar_tag_len) is ordinary live code.
                    if let Some(len) = read_dollar_tag_len(bytes, i) {
                        self.dollar_open = Some((i, i + len));
                        (i + len, false) // consume the whole opening tag
                    } else {
                        (i + 1, true)
                    }
                }
                _ => (i + 1, true),
            }
        }
    }
}

/// Blank SQL comments out of `input`, byte-for-byte length-preserving.
///
/// Every byte of a comment — `-- ...` to end of line (the line-ending byte
/// itself is kept; a bare `\r` ends the comment as well as `\n`, matching
/// `DuckDB`'s scanner, PARSE-13), and `/* ... */` including the delimiters — is
/// replaced with a space. Block comments NEST, matching `PostgreSQL`/`DuckDB` semantics (the SQL
/// standard): `/* a /* b */ c */` is one comment. An unterminated block
/// comment blanks to end of input.
///
/// Comment markers inside `'...'` string literals (with `''` escape),
/// `"..."` quoted identifiers (with `""` escape), and `$tag$ ... $tag$`
/// dollar-quoted strings are inert, and quote characters inside comments are
/// inert.
///
/// Because the output length equals the input length and all replaced
/// regions are bounded by ASCII delimiters, every byte offset into the
/// output is valid for the input — error-caret positions computed on the
/// blanked text reference the original query correctly.
///
/// This is the single comment-handling pass for the DDL surface (PA-7,
/// code-review 2026-07-02): applied once at the parse entry points it makes
/// every downstream scanner comment-immune, stops trailing comments being
/// absorbed into stored expressions (`ALTER ... RENAME TO x -- oops` renamed
/// to `x -- oops`), and fixes non-nesting block-comment handling (PA-10).
///
/// This pre-pass is the settled design, not a stopgap: §6.1 phase 8
/// (2026-07-15) evaluated folding it into the body-parser lexer and declined
/// — see the decision record in `crate::body_parser::lexer`'s module docs.
#[must_use]
pub fn blank_sql_comments(input: &str) -> std::borrow::Cow<'_, str> {
    blank_sql_comments_tracking(input).0
}

/// [`blank_sql_comments`], also returning the byte offset of the `'`, `"` or
/// `$tag$` that opens a quoted region still open at the end of `input`.
///
/// The pass cannot tell a comment from string content after an unterminated
/// quote, so it blanks nothing past one. A caller that plans the text (as
/// `plan_create` does) must refuse it rather than plan comment text as code.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn blank_sql_comments_tracking(input: &str) -> (std::borrow::Cow<'_, str>, Option<usize>) {
    #[derive(PartialEq)]
    enum St {
        Code,
        InString,
        InIdent,
    }

    let bytes = input.as_bytes();
    let mut out: Option<Vec<u8>> = None; // allocated lazily on first comment
    let mut st = St::Code;
    let mut string_is_escape = false;
    let mut dollar_tag: Option<&[u8]> = None;
    let mut open_at = 0;
    let mut i = 0;

    while i < bytes.len() {
        if let Some(tag) = dollar_tag {
            // Inside $tag$ ... $tag$ — scan for the closing tag.
            if bytes[i] == b'$' && bytes[i..].starts_with(tag) {
                i += tag.len();
                dollar_tag = None;
            } else {
                i += 1;
            }
            continue;
        }
        match st {
            St::InString => {
                // `\\` escapes the next byte inside an escape string (PARSE-7).
                if let Some(next) = escaped_pair_end(bytes, i, string_is_escape) {
                    i = next;
                    continue;
                }
                if bytes[i] == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    st = St::Code;
                }
                i += 1;
            }
            St::InIdent => {
                if bytes[i] == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    st = St::Code;
                }
                i += 1;
            }
            St::Code => match bytes[i] {
                b'\'' => {
                    st = St::InString;
                    string_is_escape = opens_escape_string(bytes, i);
                    open_at = i;
                    i += 1;
                }
                b'"' => {
                    st = St::InIdent;
                    open_at = i;
                    i += 1;
                }
                b'$' => {
                    if let Some(len) = read_dollar_tag_len(bytes, i) {
                        dollar_tag = Some(&bytes[i..i + len]);
                        open_at = i;
                        i += len;
                    } else {
                        i += 1;
                    }
                }
                b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                    // Line comment: blank to (not including) the line ending.
                    // PARSE-13: a bare `\r` ends the comment too — DuckDB
                    // inherits the PostgreSQL scanner rule, and blanking past
                    // it swallowed live code (a clause after `-- c\r`, or the
                    // whole statement after a `-- note\r` prefix) while DuckDB
                    // itself executed it. Both line-ending bytes are KEPT, so
                    // the pass stays length-preserving.
                    let buf = out.get_or_insert_with(|| bytes.to_vec());
                    while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                        buf[i] = b' ';
                        i += 1;
                    }
                }
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                    // Block comment — nesting; unterminated blanks to end.
                    let buf = out.get_or_insert_with(|| bytes.to_vec());
                    let mut depth = 0usize;
                    while i < bytes.len() {
                        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                            depth += 1;
                            buf[i] = b' ';
                            buf[i + 1] = b' ';
                            i += 2;
                        } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            depth -= 1;
                            buf[i] = b' ';
                            buf[i + 1] = b' ';
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        } else {
                            buf[i] = b' ';
                            i += 1;
                        }
                    }
                }
                _ => i += 1,
            },
        }
    }

    let unterminated = (st != St::Code || dollar_tag.is_some()).then_some(open_at);
    let text = match out {
        // Only whole comment regions (bounded by ASCII delimiters) were
        // overwritten with ASCII spaces, so the buffer remains valid UTF-8.
        Some(buf) => std::borrow::Cow::Owned(
            String::from_utf8(buf).expect("blanking comment bytes preserves UTF-8 validity"),
        ),
        None => std::borrow::Cow::Borrowed(input),
    };
    (text, unterminated)
}

/// Failure modes of [`extract_single_quoted_prefix`]. Callers map these onto
/// their local error types/messages (`ParseError` in the body parser, plain
/// `String` in the SHOW-clause parser).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleQuoteError {
    /// The input does not begin with `'`.
    NotQuoted,
    /// No unescaped closing `'` before end of input.
    Unterminated,
}

/// Extract a single-quoted SQL string literal from the start of `input`.
///
/// Returns `(unescaped_content, bytes_consumed)` where `bytes_consumed`
/// includes both the opening and closing quotes. SQL-standard escaping is
/// honoured: `''` inside the literal is a single literal `'`. Content after
/// the closing quote is not inspected — callers decide what trailing text
/// means.
///
/// Walks the input as a `char` stream (UTF-8 scalar values), never as raw
/// bytes: this is the single shared implementation mandated by ST-4
/// (code-review 2026-07-02). Two earlier per-site copies cast
/// `bytes[i] as char`, silently Latin-1-izing every non-ASCII codepoint
/// (`'café'` → `cafÃ©` — WR-04/PA-2); do not re-inline this logic.
pub fn extract_single_quoted_prefix(input: &str) -> Result<(String, usize), SingleQuoteError> {
    let mut chars = input.char_indices();
    match chars.next() {
        Some((_, '\'')) => {}
        _ => return Err(SingleQuoteError::NotQuoted),
    }
    let mut result = String::new();
    while let Some((i, ch)) = chars.next() {
        if ch == '\'' {
            // Peek without consuming; only advance the real iterator on a hit.
            let mut peek = chars.clone();
            if matches!(peek.next(), Some((_, '\''))) {
                result.push('\'');
                chars = peek;
            } else {
                // `i` is the byte offset of the closing quote; the quote is
                // one ASCII byte, so total consumed = i + 1.
                return Ok((result, i + 1));
            }
        } else {
            result.push(ch);
        }
    }
    Err(SingleQuoteError::Unterminated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // -------------------------------------------------------------------
    // byte_offset_within tests
    // -------------------------------------------------------------------

    #[test]
    fn byte_offset_within_returns_slice_offset() {
        let outer = "DROP SEMANTIC VIEW foo bar";
        let inner = &outer[19..22]; // "foo"
        assert_eq!(byte_offset_within(outer, inner), 19);
        assert_eq!(byte_offset_within(outer, outer), 0);
        // A trailing-trimmed token still maps back to its original offset.
        let tail = outer[18..].trim_start(); // "foo bar" at offset 19
        assert_eq!(byte_offset_within(outer, tail), 19);
    }

    #[test]
    fn byte_offset_within_handles_multibyte_token() {
        // 'Ω' is 2 bytes; the trailing token starts at byte 20.
        let outer = "SHOW SEMANTIC VIEWS Ωx";
        let inner = &outer[20..]; // "Ωx"
        assert_eq!(byte_offset_within(outer, inner), 20);
    }

    // -------------------------------------------------------------------
    // QuoteState escape-string tests (PARSE-7)
    // -------------------------------------------------------------------

    /// Live-code byte offsets of `s`, as `QuoteState` sees them.
    fn live_offsets(s: &str) -> Vec<usize> {
        let bytes = s.as_bytes();
        let mut st = QuoteState::default();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let (next, live) = st.step(bytes, i);
            if live {
                out.push(i);
            }
            i = next;
        }
        out
    }

    // The failure that matters: a `,` after an escape string must be seen as
    // live code. Read as an unterminated literal, everything after it is inert,
    // so a multi-entry DDL body silently mis-splits (the PA-3/P-1 class).
    #[test]
    fn quote_state_sees_a_comma_after_an_escape_string_as_live_code() {
        let s = "e'\\'',x";
        let comma = s.find(',').unwrap();
        assert!(
            live_offsets(s).contains(&comma),
            "comma after an escape string must be live code: {:?}",
            live_offsets(s)
        );
    }

    #[test]
    fn quote_state_escape_string_closes_and_leaves_string_state() {
        let bytes = b"e'\\''";
        let mut st = QuoteState::default();
        let mut i = 0;
        while i < bytes.len() {
            let (next, _) = st.step(bytes, i);
            i = next;
        }
        assert!(
            !st.in_string,
            "an escape string with a backslash-escaped quote is terminated"
        );
    }

    // Control: `\\` escapes the backslash, so the NEXT quote closes the string.
    #[test]
    fn quote_state_escaped_backslash_does_not_swallow_the_close() {
        let bytes = b"e'\\\\'";
        let mut st = QuoteState::default();
        let mut i = 0;
        while i < bytes.len() {
            let (next, _) = st.step(bytes, i);
            i = next;
        }
        assert!(
            !st.in_string,
            "e'\\\\' is a terminated one-backslash literal"
        );
    }

    // Control: a typed literal's `E` is part of the type name.
    #[test]
    fn quote_state_typed_literal_is_not_an_escape_string() {
        let s = "DATE'2020-01-01',x";
        let comma = s.find(',').unwrap();
        assert!(live_offsets(s).contains(&comma));
    }

    // Control: in an ORDINARY string a backslash is just a byte, so a trailing
    // `\` does not escape the closing quote. (DuckDB: `SELECT 'a\'` is `a\`.)
    #[test]
    fn quote_state_ordinary_string_treats_backslash_as_data() {
        let s = "'a\\',x";
        let comma = s.find(',').unwrap();
        assert!(
            live_offsets(s).contains(&comma),
            "a backslash in a plain string must not escape the close"
        );
    }

    // -------------------------------------------------------------------
    // starts_with_keyword_ci tests
    // -------------------------------------------------------------------

    #[test]
    fn ident_byte_is_the_single_boundary_definition() {
        // is_word_boundary_char is exactly the inverse of is_ident_byte, and
        // the classification is: ASCII alnum / `_` / all non-ASCII bytes are
        // identifier bytes; ASCII punctuation and whitespace are boundaries.
        for b in 0u8..=255 {
            assert_eq!(is_word_boundary_char(b), !is_ident_byte(b), "byte {b}");
        }
        for &b in b"aZ0_" {
            assert!(is_ident_byte(b));
        }
        assert!(is_ident_byte(0xC3)); // UTF-8 lead byte (é etc.)
        assert!(is_ident_byte(0xA9)); // UTF-8 continuation byte
        for &b in b" \t.,()\";'" {
            assert!(!is_ident_byte(b), "byte {b} must be a boundary");
        }
    }

    #[test]
    fn keyword_ci_matches_case_insensitively() {
        assert!(starts_with_keyword_ci("LIKE 'x'", "LIKE"));
        assert!(starts_with_keyword_ci("like 'x'", "LIKE"));
        assert!(starts_with_keyword_ci("LiKe", "LIKE"));
    }

    #[test]
    fn keyword_ci_rejects_shorter_input() {
        assert!(!starts_with_keyword_ci("LIK", "LIKE"));
        assert!(!starts_with_keyword_ci("", "LIKE"));
    }

    #[test]
    fn keyword_ci_no_panic_on_multibyte_straddle() {
        // "aΩΩ" is 5 bytes; byte 4 is mid-Ω. The old slice pattern panicked
        // here (PA-1); byte comparison just fails.
        assert!(!starts_with_keyword_ci("aΩΩ", "LIKE"));
        assert!(!starts_with_keyword_ci("Ωx", "IN"));
    }

    // -------------------------------------------------------------------
    // read_dollar_tag_len tests (P-6)
    // -------------------------------------------------------------------

    #[test]
    fn dollar_tag_valid_forms() {
        // Empty tag `$$` (len 2) and a named tag `$yaml$` (len 6).
        assert_eq!(read_dollar_tag_len(b"$$rest", 0), Some(2));
        assert_eq!(read_dollar_tag_len(b"$yaml$rest", 0), Some(6));
        assert_eq!(read_dollar_tag_len(b"$_t9$x", 0), Some(5));
    }

    #[test]
    fn dollar_tag_rejects_invalid_openers() {
        // P-6: these are the forms the extractor used to accept but the
        // comment-blanker rejected. Both must now agree they are NOT openers.
        assert_eq!(read_dollar_tag_len(b"$1$body$1$", 0), None); // digit-started tag
        assert_eq!(read_dollar_tag_len(b"$ta g$", 0), None); // interior whitespace
        assert_eq!(read_dollar_tag_len(b"$no_close", 0), None); // unterminated opener
        assert_eq!(read_dollar_tag_len(b"nope", 0), None); // no leading `$`
        assert_eq!(read_dollar_tag_len(b"", 0), None); // empty input
    }

    #[test]
    fn blank_comments_and_dollar_tag_agree_on_validity() {
        // A VALID tag makes the payload inert: `--` inside survives.
        let valid = "FROM YAML $y$a: 1 -- keep$y$";
        assert_eq!(blank_sql_comments(valid), valid);
        // An INVALID tag (`$1$`) is not a dollar-quote, so the payload is
        // scanned as SQL and its line comment IS blanked — matching the fact
        // that `extract_dollar_quoted` now rejects `$1$` outright rather than
        // storing this blanked text (P-6).
        let out = blank_sql_comments("$1$a -- x$1$");
        assert!(
            !out.contains("-- x"),
            "invalid tag payload must be treated as SQL: {out}"
        );
    }

    // -------------------------------------------------------------------
    // blank_sql_comments tests
    // -------------------------------------------------------------------

    #[test]
    fn blank_comments_no_comments_borrows() {
        let s = "SELECT 1";
        assert!(matches!(
            blank_sql_comments(s),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn blank_comments_line_comment() {
        let out = blank_sql_comments("DROP SEMANTIC VIEW a -- oops\n;");
        assert_eq!(out, "DROP SEMANTIC VIEW a        \n;");
        assert_eq!(out.len(), "DROP SEMANTIC VIEW a -- oops\n;".len());
    }

    #[test]
    fn blank_comments_block_comment_nested() {
        // Nested per SQL standard (PostgreSQL/DuckDB behaviour).
        let out = blank_sql_comments("a /* x /* y */ z */ b");
        assert_eq!(out, "a                   b");
    }

    #[test]
    fn blank_comments_unterminated_block_blanks_to_end() {
        let out = blank_sql_comments("a /* never closed");
        assert_eq!(out, "a                ");
    }

    #[test]
    fn blank_comments_markers_inside_string_inert() {
        let s = "COMMENT = 'a -- not a comment /* neither */'";
        assert_eq!(blank_sql_comments(s), s);
    }

    #[test]
    fn blank_comments_markers_inside_quoted_ident_inert() {
        let s = "\"weird--name\" AS x";
        assert_eq!(blank_sql_comments(s), s);
    }

    #[test]
    fn blank_comments_markers_inside_dollar_quotes_inert() {
        // YAML bodies ride in $$...$$ — '--' sequences inside must survive.
        let s = "FROM YAML $$name: v\n# yaml comment\nvalue: a--b$$";
        assert_eq!(blank_sql_comments(s), s);
        let s = "FROM YAML $tag$ -- inert $tag$";
        assert_eq!(blank_sql_comments(s), s);
    }

    #[test]
    fn blank_comments_dollar_parameter_not_a_tag() {
        // $1 is a parameter, not a dollar-quote opener; the comment after it
        // must still be blanked.
        let out = blank_sql_comments("WHERE x = $1 -- c");
        assert_eq!(out, "WHERE x = $1     ");
    }

    // PARSE-13 (code-review 2026-08-08): DuckDB's scanner (the PostgreSQL rule)
    // ends a `--` comment at a bare `\r` as well as at `\n`. Blanking only to
    // `\n` swallowed the CODE after a CR-terminated comment: DuckDB executed
    // `b` while every downstream scanner saw spaces, so a clause after
    // `-- c\r` vanished from a parsed body and a `-- note\r` prefix disabled
    // search-path injection for the statement that followed.
    #[test]
    fn blank_comments_line_comment_ends_at_a_bare_carriage_return() {
        let input = "a -- c\rb";
        let out = blank_sql_comments(input);
        assert_eq!(out, "a     \rb");
        // Length preservation is load-bearing (error carets / raw re-slices).
        assert_eq!(out.len(), input.len());
    }

    #[test]
    fn blank_comments_line_comment_ends_at_crlf_keeping_both_bytes() {
        let input = "a -- c\r\nb";
        let out = blank_sql_comments(input);
        assert_eq!(out, "a     \r\nb");
        assert_eq!(out.len(), input.len());
    }

    #[test]
    fn blank_comments_quote_inside_comment_inert() {
        // An apostrophe inside a comment must not open a string region.
        let out = blank_sql_comments("a -- don't\nb 'lit'");
        assert_eq!(out, "a         \nb 'lit'");
    }

    #[test]
    fn blank_comments_tracking_reports_an_unterminated_quote() {
        let open = |s: &str| blank_sql_comments_tracking(s).1;
        assert_eq!(open("v' AS x -- a"), Some(1));
        assert_eq!(open("a \"b -- c"), Some(2));
        assert_eq!(open("a 'it''s' \"q\"\"\" b"), None);
        // Quotes inside a comment or a dollar-quoted string open nothing.
        assert_eq!(open("a -- don't\nb"), None);
        assert_eq!(open("a /* ' */ b"), None);
        assert_eq!(open("$$ it's $$"), None);
        assert_eq!(open("x $a$ y -- c"), Some(2));
        // A `$` inside an identifier opens nothing, so nothing is unterminated.
        assert_eq!(open("v$a$ x -- c"), None);
    }

    #[test]
    fn dollar_tag_does_not_open_inside_an_identifier() {
        // DuckDB reads `v$a$` and `v$$x$$` as identifiers: a `$` right after an
        // identifier byte continues it, so no dollar quote opens there.
        assert_eq!(read_dollar_tag_len(b"v$a$", 1), None);
        assert_eq!(read_dollar_tag_len(b"v$$x$$", 1), None);
        assert_eq!(read_dollar_tag_len(b"v$$x$$", 2), None);
        assert_eq!(read_dollar_tag_len(b"price_9$usd$", 7), None);
        // At a token start it still opens.
        assert_eq!(read_dollar_tag_len(b"x $a$", 2), Some(3));
        assert_eq!(read_dollar_tag_len(b"($a$", 1), Some(3));
        // So a comment after such an identifier is still blanked.
        assert_eq!(blank_sql_comments("v$a$ x -- c"), "v$a$ x     ");
    }

    #[test]
    fn blank_comments_multibyte_inside_comment() {
        let input = "x -- café ☕\ny";
        let out = blank_sql_comments(input);
        assert_eq!(out.len(), input.len());
        assert!(out.starts_with("x  "));
        assert!(out.ends_with("\ny"));
        // Result must be valid UTF-8 by construction (checked by the type),
        // and the non-comment text intact.
        assert_eq!(&out[input.len() - 1..], "y");
    }

    // PARSE-7: DuckDB accepts Postgres-style escape strings (`E'...'`) where
    // `\'` is an escaped quote. Every scanner in the crate treated `\` as an
    // ordinary byte, so in `e'\''` the middle `'` and the closing `'` were
    // consumed as one `''` escape pair and the literal read as UNTERMINATED --
    // everything after it stayed in "string" state. Verified against DuckDB:
    // `SELECT e'\''` returns a single quote character.
    //
    // Here that means a real comment after an escape string was left unblanked.
    #[test]
    fn blank_comments_after_an_escape_string_are_still_blanked() {
        let s = "e'\\'' -- real";
        let out = blank_sql_comments(s);
        assert_eq!(out, "e'\\''        ");
    }

    // `\\` is an escaped BACKSLASH, so it does not escape a following quote --
    // the string ends at the next `'`. (DuckDB: `SELECT E'\\\\'` is one backslash.)
    #[test]
    fn blank_comments_escaped_backslash_does_not_swallow_the_close() {
        let s = "e'\\\\' -- real";
        let out = blank_sql_comments(s);
        assert_eq!(out, "e'\\\\'        ");
    }

    // The `E` of a TYPED LITERAL is part of the type name, not an escape-string
    // introducer: `DATE'2020-01-01'` is valid DuckDB and its backslash rules are
    // the ordinary ones. The introducer therefore requires a token boundary
    // before the `e`.
    #[test]
    fn blank_comments_typed_literal_is_not_an_escape_string() {
        let s = "DATE'2020-01-01' -- real";
        let out = blank_sql_comments(s);
        assert_eq!(out, "DATE'2020-01-01'        ");
    }

    #[test]
    fn blank_comments_escaped_quote_in_string() {
        let s = "'it''s -- fine' -- real";
        let out = blank_sql_comments(s);
        assert_eq!(out, "'it''s -- fine'        ");
    }

    // -------------------------------------------------------------------
    // extract_single_quoted_prefix tests
    // -------------------------------------------------------------------

    #[test]
    fn quoted_prefix_basic() {
        let (s, n) = extract_single_quoted_prefix("'abc' rest").unwrap();
        assert_eq!(s, "abc");
        assert_eq!(n, 5);
    }

    #[test]
    fn quoted_prefix_escaped_quote() {
        let (s, n) = extract_single_quoted_prefix("'a''b'").unwrap();
        assert_eq!(s, "a'b");
        assert_eq!(n, 6);
    }

    #[test]
    fn quoted_prefix_empty_literal() {
        let (s, n) = extract_single_quoted_prefix("''").unwrap();
        assert_eq!(s, "");
        assert_eq!(n, 2);
    }

    #[test]
    fn quoted_prefix_non_ascii_content_survives() {
        // PA-2 regression: the per-site copies Latin-1-ized this to "cafÃ©".
        let (s, n) = extract_single_quoted_prefix("'café et plus'").unwrap();
        assert_eq!(s, "café et plus");
        assert_eq!(n, "'café et plus'".len());

        let (s, _) = extract_single_quoted_prefix("'東京 ☕'").unwrap();
        assert_eq!(s, "東京 ☕");
    }

    #[test]
    fn quoted_prefix_errors() {
        assert_eq!(
            extract_single_quoted_prefix("abc"),
            Err(SingleQuoteError::NotQuoted)
        );
        assert_eq!(
            extract_single_quoted_prefix("'abc"),
            Err(SingleQuoteError::Unterminated)
        );
        assert_eq!(
            extract_single_quoted_prefix(""),
            Err(SingleQuoteError::NotQuoted)
        );
    }

    proptest! {
        // Round-trip: escaping then extracting returns the original content
        // and consumes exactly the literal, for arbitrary unicode content.
        #[test]
        fn quoted_prefix_roundtrips_arbitrary_content(
            content in "\\PC{0,40}",
            tail in "[ a-zA-Z]{0,10}",
        ) {
            let literal = format!("'{}'{}", content.replace('\'', "''"), tail);
            let (extracted, consumed) = extract_single_quoted_prefix(&literal).unwrap();
            prop_assert_eq!(&extracted, &content);
            prop_assert_eq!(&literal[consumed..], &tail);
        }
    }

    // -------------------------------------------------------------------
    // suggest_closest property tests
    // -------------------------------------------------------------------

    proptest! {
        /// Any suggestion returned by suggest_closest must be a member of the
        /// input `available` list. This prevents the function from inventing
        /// names that don't exist in the model.
        #[test]
        fn suggestion_is_always_valid_name(
            query in "[a-z_]{1,20}",
            names in prop::collection::vec("[a-z_]{1,20}", 1..20)
        ) {
            if let Some(suggestion) = suggest_closest(&query, &names) {
                prop_assert!(
                    names.contains(&suggestion),
                    "suggest_closest returned '{}' which is not in available names: {:?}",
                    suggestion,
                    names
                );
            }
        }

        /// An exact match (query == one of the available names) should always
        /// produce a suggestion, since edit distance is 0 which is within the
        /// threshold of 3.
        #[test]
        fn exact_match_always_suggests(
            name in "[a-z_]{1,20}",
            others in prop::collection::vec("[a-z_]{1,20}", 0..10)
        ) {
            let mut names = others;
            names.push(name.clone());
            let suggestion = suggest_closest(&name, &names);
            prop_assert!(
                suggestion.is_some(),
                "exact match '{}' should always produce a suggestion",
                name
            );
            prop_assert_eq!(
                suggestion.unwrap(),
                name,
                "exact match should suggest itself"
            );
        }

        /// When the available list is empty, suggest_closest must return None.
        #[test]
        fn empty_names_returns_none(
            query in "[a-z_]{1,20}"
        ) {
            let names: Vec<String> = vec![];
            prop_assert!(suggest_closest(&query, &names).is_none());
        }
    }
}
