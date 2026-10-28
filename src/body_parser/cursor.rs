//! A forward token cursor over a lexed clause (§6.1 incremental lexer + cursor
//! migration, code-review 2026-07-11).
//!
//! The cursor is the layer the recursive-descent clause parsers talk to. It
//! owns the token vector produced by [`super::lexer::lex`] and exposes exactly
//! the primitives a clause parser needs: look at the next token, consume it,
//! recognize a bare keyword, scan ahead for a keyword, consume a balanced
//! `(...)` group, and build a [`ParseError`] whose caret is a real token
//! offset (no manual `+2` / `- len` byte arithmetic — the source of the P-4
//! caret-drift class).
//!
//! `base` is the absolute byte offset of `src[0]` within the original query,
//! so [`Cursor::err`] and [`Cursor::abs`] map a source-relative offset back to
//! a caret position in the user's SQL.

use super::lexer::{lex, Token, TokenKind};
use crate::errors::ParseError;

/// The kind of unterminated quoted region a token stream ends in, returned by
/// [`Cursor::unterminated_tail`] so a failed `take_parens` reports a precise
/// "unterminated ..." message instead of a misleading "unclosed paren".
#[derive(Debug, PartialEq, Eq)]
pub(super) enum UnterminatedRegion {
    /// An unterminated double-quoted identifier `"...`.
    Ident,
    /// An unterminated single-quoted string literal `'...`.
    String,
    /// An unterminated dollar-quoted string `$tag$...` (PARSE-1).
    Dollar,
}

pub(super) struct Cursor<'a> {
    src: &'a str,
    base: usize,
    toks: Vec<Token>,
    idx: usize,
}

impl<'a> Cursor<'a> {
    /// Lex `src` and position the cursor at the first token. `base` is the
    /// absolute offset of `src[0]` in the original query (for carets).
    pub(super) fn new(src: &'a str, base: usize) -> Self {
        Cursor {
            src,
            base,
            toks: lex(src),
            idx: 0,
        }
    }

    /// The next unconsumed token, if any.
    pub(super) fn peek(&self) -> Option<Token> {
        self.toks.get(self.idx).copied()
    }

    /// Consume and return the next token, if any.
    pub(super) fn bump(&mut self) -> Option<Token> {
        let t = self.toks.get(self.idx).copied();
        if t.is_some() {
            self.idx += 1;
        }
        t
    }

    /// The source text of `t`.
    pub(super) fn text(&self, t: Token) -> &'a str {
        &self.src[t.start..t.end]
    }

    /// Byte offset (in `src`) of the next unconsumed token, or `src.len()` when
    /// the cursor is exhausted. This is where "the rest of the input" begins.
    pub(super) fn byte_pos(&self) -> usize {
        self.toks.get(self.idx).map_or(self.src.len(), |t| t.start)
    }

    /// The remaining source from [`Cursor::byte_pos`] to end — handed verbatim
    /// to a sub-parser (e.g. the shared trailing-annotation parser) so
    /// uninterpreted tails stay re-sliced source, never re-tokenized twice.
    pub(super) fn rest(&self) -> &'a str {
        &self.src[self.byte_pos()..]
    }

    /// Absolute caret position for a source-relative byte offset.
    pub(super) fn abs(&self, off: usize) -> usize {
        self.base + off
    }

    /// Absolute caret position of `sub`, which MUST be a subslice of this
    /// cursor's `src` (as produced by slicing / `.trim()` of `src`). Lets a
    /// caller hand a re-sliced tail to a sub-parser together with the absolute
    /// offset of its first byte, so the sub-parser can anchor its own carets
    /// (F-15, code-review 2026-07-16).
    pub(super) fn abs_of(&self, sub: &str) -> usize {
        self.base + crate::util::byte_offset_within(self.src, sub)
    }

    /// Build a [`ParseError`] anchored at a source-relative offset.
    pub(super) fn err(&self, off: usize, message: String) -> ParseError {
        ParseError {
            message,
            position: Some(self.abs(off)),
        }
    }

    /// Is `t` a *bare* identifier equal to `kw` (ASCII case-insensitive)? A
    /// double-quoted `"..."` never matches — quoting means the text is data,
    /// not a keyword.
    pub(super) fn is_kw(&self, t: Token, kw: &str) -> bool {
        matches!(t.kind, TokenKind::Ident { quoted: false })
            && self.text(t).eq_ignore_ascii_case(kw)
    }

    /// The first token at/after the cursor that is a bare keyword `kw`,
    /// without consuming anything. Quote-aware by construction (a quoted
    /// `"UNIQUE"` is not a keyword token).
    pub(super) fn find_kw(&self, kw: &str) -> Option<Token> {
        self.toks[self.idx..]
            .iter()
            .copied()
            .find(|&t| self.is_kw(t, kw))
    }

    /// The first token starting at or after source offset `from`, without
    /// consuming anything.
    pub(super) fn token_at_or_after(&self, from: usize) -> Option<Token> {
        self.toks.iter().copied().find(|t| t.start >= from)
    }

    /// The first bare keyword `kw` starting at or after source offset `from`,
    /// without consuming anything.
    pub(super) fn find_kw_at_or_after(&self, kw: &str, from: usize) -> Option<Token> {
        self.toks
            .iter()
            .copied()
            .find(|&t| t.start >= from && self.is_kw(t, kw))
    }

    /// The first `Symbol(b)` token at/after the cursor, without consuming.
    /// Quote-aware by construction — a `b` inside a string/quoted-ident is part
    /// of that one token, never a `Symbol`. Used to locate a structural
    /// delimiter (e.g. the qualifier `.`) that a name run splits on.
    pub(super) fn find_symbol(&self, b: u8) -> Option<Token> {
        self.toks[self.idx..]
            .iter()
            .copied()
            .find(|t| t.kind == TokenKind::Symbol(b))
    }

    /// Is the next token `Symbol(b)`?
    pub(super) fn peek_is_symbol(&self, b: u8) -> bool {
        matches!(self.peek(), Some(t) if t.kind == TokenKind::Symbol(b))
    }

    /// Is the next token a non-symbol value token (bare/quoted identifier,
    /// string, or an unterminated region) — i.e. a name/value rather than
    /// punctuation? Used where a clause expects an identifier next.
    pub(super) fn peek_is_value(&self) -> bool {
        matches!(self.peek(), Some(t) if !matches!(t.kind, TokenKind::Symbol(_)))
    }

    /// Advance the cursor past every token that starts before `byte` (an offset
    /// in src). Used to resync the token index after a name run captured by
    /// source-slice (e.g. "everything up to the `AS` / `(`").
    pub(super) fn advance_past_byte(&mut self, byte: usize) {
        while self.toks.get(self.idx).is_some_and(|t| t.start < byte) {
            self.idx += 1;
        }
    }

    /// The first token at/after the cursor that is a bare keyword `kw1`
    /// *immediately followed* by a bare keyword `kw2` (only whitespace between
    /// them, since whitespace is the only thing that separates two adjacent
    /// tokens). Reproduces the two-word keyword match (`PRIMARY KEY`) without a
    /// substring scan.
    pub(super) fn find_kw_pair(&self, kw1: &str, kw2: &str) -> Option<Token> {
        let rest = &self.toks[self.idx..];
        rest.windows(2)
            .find(|w| self.is_kw(w[0], kw1) && self.is_kw(w[1], kw2))
            .map(|w| w[0])
    }

    /// The first bare keyword `kw` token at/after the cursor that sits at
    /// bracket-depth 0 (not inside any `(...)`, `[...]`, or `{...}` group). Used
    /// for `OVER`, which must be outside the window-function call's own
    /// parentheses. All three bracket kinds count as nesting, matching
    /// [`super::split_at_depth0_commas`] and the retired `find_depth0_keyword`.
    /// Whether the remaining text is ONE redundant `(...)` wrapper — the first
    /// token is an opening bracket whose match is the last token.
    ///
    /// Used to unwrap an expression before a depth-0 clause scan: parenthesising
    /// the whole thing would otherwise push every keyword to depth 1 and hide it
    /// (Copilot review, #181). Deliberately narrow — `(a) + (b)` is NOT wholly
    /// parenthesised, and a `(...)` that is structural rather than a wrapper
    /// (a subquery, an argument list) keeps its contents nested, which is what
    /// lets a genuinely nested `USING` inside a JOIN stay legal.
    pub(super) fn is_wholly_parenthesised(&self) -> bool {
        let toks = &self.toks[self.idx..];
        let (Some(first), Some(last)) = (toks.first(), toks.last()) else {
            return false;
        };
        if !matches!(first.kind, TokenKind::Symbol(b'(' | b'[' | b'{')) {
            return false;
        }
        // Walk from the opening bracket; the depth returns to 0 exactly once,
        // and that must be at the final token for the wrapper to be redundant.
        let mut depth = 0i32;
        for (i, &t) in toks.iter().enumerate() {
            match t.kind {
                TokenKind::Symbol(b'(' | b'[' | b'{') => depth += 1,
                TokenKind::Symbol(b')' | b']' | b'}') => {
                    depth -= 1;
                    if depth == 0 {
                        return i == toks.len() - 1 && t.start == last.start;
                    }
                }
                _ => {}
            }
        }
        false
    }

    pub(super) fn find_kw_depth0(&self, kw: &str) -> Option<Token> {
        let mut depth = 0i32;
        for &t in &self.toks[self.idx..] {
            match t.kind {
                TokenKind::Symbol(b'(' | b'[' | b'{') => depth += 1,
                TokenKind::Symbol(b')' | b']' | b'}') => depth -= 1,
                _ if depth == 0 && self.is_kw(t, kw) => return Some(t),
                _ => {}
            }
        }
        None
    }

    /// Every bare keyword token at/after the cursor that equals ANY of `kws`
    /// AND sits at bracket-depth 0, in source order. The depth-0 tiling variant
    /// of [`Cursor::find_any_kw`]: it locates a fixed set of sub-clause
    /// keywords (`TABLE` / `DIMENSIONS` / `METRICS`) that partition a body, so a
    /// keyword-like name nested inside one of their `(...)` lists is inert.
    /// Replaces the ad-hoc quote+depth `find_sub_keyword_positions` scan
    /// (code-review 2026-07-11). All three bracket kinds count as nesting,
    /// matching [`Cursor::find_kw_depth0`].
    pub(super) fn find_all_kw_depth0(&self, kws: &[&str]) -> Vec<Token> {
        let mut depth = 0i32;
        let mut out = Vec::new();
        for &t in &self.toks[self.idx..] {
            match t.kind {
                TokenKind::Symbol(b'(' | b'[' | b'{') => depth += 1,
                TokenKind::Symbol(b')' | b']' | b'}') => depth -= 1,
                _ if depth == 0 && kws.iter().any(|kw| self.is_kw(t, kw)) => out.push(t),
                _ => {}
            }
        }
        out
    }

    /// The first token at/after the cursor that is a bare keyword equal to ANY
    /// of `kws`. Used to locate the earliest of several possible boundary
    /// keywords (e.g. `ORDER` / `ROWS` / `RANGE` / `GROUPS`).
    pub(super) fn find_any_kw(&self, kws: &[&str]) -> Option<Token> {
        self.toks[self.idx..]
            .iter()
            .copied()
            .find(|&t| kws.iter().any(|kw| self.is_kw(t, kw)))
    }

    /// The first run of consecutive bare keyword tokens matching `kws` in order
    /// (only whitespace between them, since that is all that separates adjacent
    /// tokens). Returns `(first_token, last_token)`. Used for multi-word
    /// keywords like `NON ADDITIVE BY` — quote-aware by construction.
    pub(super) fn find_kw_seq(&self, kws: &[&str]) -> Option<(Token, Token)> {
        let toks = &self.toks[self.idx..];
        if kws.is_empty() || toks.len() < kws.len() {
            return None;
        }
        (0..=toks.len() - kws.len())
            .find(|&w| {
                kws.iter()
                    .enumerate()
                    .all(|(k, kw)| self.is_kw(toks[w + k], kw))
            })
            .map(|w| (toks[w], toks[w + kws.len() - 1]))
    }

    /// Like [`Cursor::find_kw_seq`], but only matches a run that starts at
    /// bracket-depth 0 (not inside any `(...)`/`[...]`/`{...}` group). The
    /// consecutive keyword tokens have no bracket tokens between them, so if the
    /// first is at depth 0 the whole run is. Used for `NON ADDITIVE BY`, which
    /// must be the metric entry's own tail keyword — not a like-named run nested
    /// inside a preceding `USING (...)` list.
    pub(super) fn find_kw_seq_depth0(&self, kws: &[&str]) -> Option<(Token, Token)> {
        let toks = &self.toks[self.idx..];
        let n = kws.len();
        if n == 0 || toks.len() < n {
            return None;
        }
        let mut depth = 0i32;
        for w in 0..=toks.len() - n {
            if depth == 0
                && kws
                    .iter()
                    .enumerate()
                    .all(|(k, kw)| self.is_kw(toks[w + k], kw))
            {
                return Some((toks[w], toks[w + n - 1]));
            }
            match toks[w].kind {
                TokenKind::Symbol(b'(' | b'[' | b'{') => depth += 1,
                TokenKind::Symbol(b')' | b']' | b'}') => depth -= 1,
                _ => {}
            }
        }
        None
    }

    /// If the token stream ends in an unterminated quoted region, classify it.
    /// A `TokenKind::Unterminated` token (`"..."` / `'...'`) and a dollar-quoted
    /// literal whose close tag never appeared (`TokenKind::DollarString {
    /// terminated: false }`, PARSE-1) each span to end-of-input, so the open
    /// region is necessarily the last token. Used to turn a failed
    /// [`Cursor::take_parens`] into a precise "unterminated ..." error rather
    /// than a misleading "unclosed paren".
    pub(super) fn unterminated_tail(&self) -> Option<UnterminatedRegion> {
        match self.toks.last()?.kind {
            TokenKind::Unterminated { ident: true } => Some(UnterminatedRegion::Ident),
            TokenKind::Unterminated { ident: false } => Some(UnterminatedRegion::String),
            TokenKind::DollarString { terminated: false } => Some(UnterminatedRegion::Dollar),
            _ => None,
        }
    }

    /// If the next token is `(`, consume the whole balanced `(...)` group and
    /// return its inner source slice (between the parens). Advances past the
    /// matching `)`. Returns `None` when the next token is not `(` *or* the
    /// group never closes — the caller distinguishes the two with a prior
    /// [`Cursor::peek`] and reports "Expected '('" vs "Unclosed '('".
    ///
    /// Quote-awareness is free: a `)` inside a string or quoted identifier is
    /// part of that single token, never a `Symbol(b')')`, so it cannot close
    /// the group.
    pub(super) fn take_parens(&mut self) -> Option<&'a str> {
        let open = self.peek()?;
        if open.kind != TokenKind::Symbol(b'(') {
            return None;
        }
        self.bump();
        let inner_start = open.end;
        let mut depth = 1i32;
        while let Some(t) = self.peek() {
            match t.kind {
                TokenKind::Symbol(b'(') => depth += 1,
                TokenKind::Symbol(b')') => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &self.src[inner_start..t.start];
                        self.bump();
                        return Some(inner);
                    }
                }
                _ => {}
            }
            self.bump();
        }
        None
    }

    /// The first bracket token, from the cursor on, that does not nest by
    /// kind: a closer that is not the partner of the innermost open bracket, a
    /// closer with nothing open, or an opener still open at the end. Brackets
    /// inside strings, quoted names and dollar-quoted strings are part of
    /// those tokens and never seen here.
    ///
    /// The depth scanners count `(`, `[` and `{` as one depth and let any
    /// closer close any opener, while [`Cursor::take_parens`] counts only
    /// `(`/`)`. On text whose brackets nest by kind the two agree; on other
    /// text they split it differently (fuzz #277, #285), so a clause must
    /// pass this check before they run on it.
    pub(super) fn bracket_mismatch(&self) -> Option<BracketMismatch> {
        let mut open: Vec<Token> = Vec::new();
        for &t in &self.toks[self.idx..] {
            let TokenKind::Symbol(b) = t.kind else {
                continue;
            };
            match b {
                b'(' | b'[' | b'{' => open.push(t),
                b')' | b']' | b'}' => match open.pop() {
                    None => return Some(BracketMismatch::Unmatched(t)),
                    Some(o) if closer_of(self.symbol(o)) != b => {
                        return Some(BracketMismatch::Mismatched {
                            closer: t,
                            opener: o,
                        });
                    }
                    Some(_) => {}
                },
                _ => {}
            }
        }
        open.pop().map(BracketMismatch::Unclosed)
    }

    /// The byte at the start of `t` (a [`TokenKind::Symbol`] is one byte).
    pub(super) fn symbol(&self, t: Token) -> u8 {
        self.src.as_bytes()[t.start]
    }
}

/// The closing bracket that partners `open` (`(`, `[` or `{`).
pub(super) fn closer_of(open: u8) -> u8 {
    match open {
        b'(' => b')',
        b'[' => b']',
        _ => b'}',
    }
}

/// A bracket that does not nest by kind, from [`Cursor::bracket_mismatch`].
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BracketMismatch {
    /// `closer` is not the partner of `opener`, the innermost open bracket.
    Mismatched { closer: Token, opener: Token },
    /// A closer with no bracket open.
    Unmatched(Token),
    /// An opener (the innermost one) never closed.
    Unclosed(Token),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peek_bump_and_kw() {
        let mut c = Cursor::new("o AS orders", 100);
        let a = c.peek().unwrap();
        assert_eq!(c.text(a), "o");
        assert!(c.is_kw(c.peek().unwrap(), "o"));
        c.bump();
        assert!(c.is_kw(c.peek().unwrap(), "as")); // case-insensitive
        c.bump();
        assert_eq!(c.text(c.peek().unwrap()), "orders");
    }

    #[test]
    fn abs_carets_are_base_relative() {
        let c = Cursor::new("xy", 100);
        assert_eq!(c.abs(0), 100);
        assert_eq!(c.err(1, "boom".to_string()).position, Some(101));
    }

    #[test]
    fn find_kw_pair_only_matches_adjacent_words() {
        let c = Cursor::new("orders PRIMARY KEY (id)", 0);
        let t = c.find_kw_pair("PRIMARY", "KEY").unwrap();
        assert_eq!(c.text(t), "PRIMARY");
        // `PRIMARY , KEY` — a comma between → not a pair.
        let c2 = Cursor::new("PRIMARY , KEY", 0);
        assert!(c2.find_kw_pair("PRIMARY", "KEY").is_none());
    }

    #[test]
    fn find_kw_seq_matches_consecutive_words() {
        // Successful 3-word match returns the first and last keyword tokens.
        let c = Cursor::new("revenue NON ADDITIVE BY date", 0);
        let (first, last) = c.find_kw_seq(&["NON", "ADDITIVE", "BY"]).unwrap();
        assert_eq!(c.text(first), "NON");
        assert_eq!(c.text(last), "BY");
        // Case-insensitive.
        assert!(Cursor::new("non additive by", 0)
            .find_kw_seq(&["NON", "ADDITIVE", "BY"])
            .is_some());
        // Interrupted by an intervening token → no match.
        assert!(Cursor::new("NON ADDITIVE junk BY", 0)
            .find_kw_seq(&["NON", "ADDITIVE", "BY"])
            .is_none());
        // Too short → no match.
        assert!(Cursor::new("NON ADDITIVE", 0)
            .find_kw_seq(&["NON", "ADDITIVE", "BY"])
            .is_none());
        // Quote-aware: a quoted "NON" is not a keyword token.
        assert!(Cursor::new("\"NON\" ADDITIVE BY", 0)
            .find_kw_seq(&["NON", "ADDITIVE", "BY"])
            .is_none());
    }

    #[test]
    fn find_all_kw_depth0_tiles_and_ignores_nested_and_quoted() {
        // TABLE / DIMENSIONS / METRICS at depth 0 are found in order; a
        // keyword-like *name* nested inside a (...) list (`METRICS` as a dim
        // name at depth 1) or hidden inside a quoted ident is inert.
        let c = Cursor::new(
            "TABLE t, DIMENSIONS (metrics, \"TABLE\"), METRICS (total)",
            0,
        );
        let kws = c.find_all_kw_depth0(&["TABLE", "DIMENSIONS", "METRICS"]);
        let texts: Vec<&str> = kws.iter().map(|&t| c.text(t)).collect();
        // The nested (depth-1) `metrics` name and the quoted `"TABLE"` do not appear.
        assert_eq!(texts, vec!["TABLE", "DIMENSIONS", "METRICS"]);
    }

    #[test]
    fn find_kw_seq_depth0_ignores_nested_run() {
        // A `NON ADDITIVE BY` run nested inside a preceding `(...)` is inert;
        // the depth-0 run at the tail is the match.
        let c = Cursor::new("m USING (NON ADDITIVE BY) NON ADDITIVE BY (d)", 0);
        let (first, last) = c.find_kw_seq_depth0(&["NON", "ADDITIVE", "BY"]).unwrap();
        // The match is the depth-0 run, which starts after the `)` at byte 26.
        assert_eq!(first.start, 26);
        assert_eq!(c.text(first), "NON");
        assert_eq!(c.text(last), "BY");
        // With ONLY a nested run, there is no depth-0 match.
        let c2 = Cursor::new("m USING (NON ADDITIVE BY)", 0);
        assert!(c2.find_kw_seq_depth0(&["NON", "ADDITIVE", "BY"]).is_none());
        // A plain depth-0 run still matches (parity with find_kw_seq).
        let c3 = Cursor::new("revenue NON ADDITIVE BY date", 0);
        assert!(c3.find_kw_seq_depth0(&["NON", "ADDITIVE", "BY"]).is_some());
    }

    #[test]
    fn find_kw_skips_quoted_and_strings() {
        // A quoted "UNIQUE" and a string 'UNIQUE' are not keyword tokens.
        let c = Cursor::new("\"UNIQUE\" 'UNIQUE'", 0);
        assert!(c.find_kw("UNIQUE").is_none());
    }

    #[test]
    fn take_parens_is_quote_and_nest_aware() {
        let mut c = Cursor::new("(a, (b), \"x)y\")", 0);
        assert_eq!(c.take_parens(), Some("a, (b), \"x)y\""));
        assert!(c.peek().is_none());
    }

    #[test]
    fn take_parens_unclosed_is_none() {
        let mut c = Cursor::new("(a, b", 0);
        assert_eq!(c.take_parens(), None);
    }

    #[test]
    fn unterminated_tail_reports_open_quote_kind() {
        // A `(...` that never closes because a quote / dollar-quote swallowed
        // the rest.
        assert_eq!(
            Cursor::new("(a, \"b c", 0).unterminated_tail(),
            Some(UnterminatedRegion::Ident)
        );
        assert_eq!(
            Cursor::new("(a, 'b c", 0).unterminated_tail(),
            Some(UnterminatedRegion::String)
        );
        assert_eq!(
            Cursor::new("(a, $$b c", 0).unterminated_tail(),
            Some(UnterminatedRegion::Dollar)
        );
        // A plain unclosed paren (no open quote) is not an unterminated tail.
        assert_eq!(Cursor::new("(a, b", 0).unterminated_tail(), None);
        // A fully-terminated stream has no unterminated tail.
        assert_eq!(Cursor::new("(a, \"b\")", 0).unterminated_tail(), None);
    }

    #[test]
    fn take_parens_not_a_paren_is_none() {
        let mut c = Cursor::new("id", 0);
        assert_eq!(c.take_parens(), None);
        // Cursor did not advance.
        assert_eq!(c.text(c.peek().unwrap()), "id");
    }

    #[test]
    fn rest_returns_remaining_source() {
        let mut c = Cursor::new("a b   c", 0);
        c.bump();
        assert_eq!(c.rest(), "b   c");
    }
}
