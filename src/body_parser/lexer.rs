//! Token stream over SQL clause text (§6.1 incremental lexer + cursor
//! migration, code-review 2026-07-11).
//!
//! This is the shared tokenizer the clause parsers are being migrated onto,
//! one clause per phase (TABLES first). It replaces the family of ad-hoc
//! quote-state loops and "find keyword anywhere and slice" scanners that let
//! the P-1/P-2/P-3 silent-discard bug class re-emerge in every new grammar
//! slot: once identifiers, strings, and punctuation are *tokens*, a keyword
//! search can only ever match a real keyword token — never a substring inside
//! a quoted identifier or a string literal — and a clause parser consumes its
//! tokens in order, so "text between the name and the constraint" becomes a
//! visible unexpected token rather than a region silently skipped over.
//!
//! ## Scope
//!
//! The token kinds are exactly what the migrated clauses need today:
//! double-quoted / bare identifiers, single-quoted string literals, and
//! single-byte symbols. Comment handling deliberately stays upstream in
//! [`crate::util::blank_sql_comments`] — §6.1 phase 8 (2026-07-15) evaluated
//! folding it into this lexer and declined. Blanking is a whole-query,
//! length-preserving pre-pass shared by statement detection
//! (`parse::detect`) and the CREATE front door (`parse::rewrite`), neither of
//! which tokenizes through this lexer, so a fold would not remove the
//! pre-pass; and this lexer only ever receives already-blanked text, so
//! comment handling here would be dead code on its own path. Length
//! preservation is also load-bearing: stored expressions are re-sliced from
//! raw source and error carets are computed on the blanked text, so offsets
//! must map 1:1 onto the original bytes (pinned by
//! `caret_after_in_body_comment_is_honest`) — a lexer that merely skipped
//! comment tokens would let raw slices re-absorb comment bytes. Revisit only
//! as part of a universal-front-door refactor in which detect/rewrite also
//! tokenize through this lexer and every raw-slice consumer reads via a
//! comment-aware token layer. Numeric literals still tokenize as bare
//! identifiers (harmless for the identifier-only clauses migrated so far);
//! dollar-quoted literals get their own [`TokenKind::DollarString`] (PARSE-1,
//! code-review 2026-07-18) so a delimiter or keyword inside `$tag$ ... $tag$`
//! is inert to the structural scan, exactly like the contents of a `'string'`.
//!
//! ## UTF-8 safety
//!
//! Only ASCII bytes (`"`, `'`, whitespace, punctuation) are ever compared, and
//! [`crate::util::is_ident_byte`] classifies every byte `>= 0x80` as an
//! identifier byte, so a multi-byte UTF-8 codepoint is consumed whole into a
//! bare-identifier token. Token spans therefore always land on char
//! boundaries — no byte-slice ever cuts a codepoint (the PA-1/PA-2 class).

use crate::util::is_ident_byte;

/// What a [`Token`] is. `Symbol` carries the raw punctuation byte so a cursor
/// can match `(`, `)`, `,`, `.`, `=` etc. directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenKind {
    /// A bare (`orders`) or double-quoted (`"my table"`) identifier. `quoted`
    /// distinguishes the two so keyword matching only ever fires on bare
    /// idents — a quoted `"PRIMARY"` is data, never the keyword.
    Ident { quoted: bool },
    /// A single-quoted string literal `'...'`, `''` treated as an escape.
    String,
    /// A dollar-quoted string `$tag$ ... $tag$` (PARSE-1, code-review
    /// 2026-07-18). `terminated` is false when the opening `$tag$` had no
    /// matching close tag before end-of-input (the parser errors on it). The
    /// whole literal is ONE opaque token so a `)` / `,` / keyword inside it is
    /// never a matchable `Symbol` / `Ident` — the same guarantee `'...'` and
    /// `"..."` already give. The tag grammar is shared with `QuoteState` and the
    /// comment-blanker via [`crate::util::read_dollar_tag_len`].
    DollarString { terminated: bool },
    /// A single punctuation byte outside any quoted region.
    Symbol(u8),
    /// An unterminated `"..."` (`ident = true`) or `'...'` (`ident = false`)
    /// region. Spans from the opening quote to end-of-input; the clause parser
    /// turns this into a context-specific "Unterminated quoted identifier" /
    /// "Unterminated string literal" error rather than the lexer guessing the
    /// message.
    Unterminated { ident: bool },
}

/// A lexed token: its [`TokenKind`] and its half-open byte span `[start, end)`
/// in the lexed source string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Token {
    pub(super) kind: TokenKind,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// Tokenize `src`. Infallible: unterminated quotes/strings become
/// [`TokenKind::Unterminated`] tokens (so the parser owns the error message)
/// and whitespace is skipped. Spans are byte offsets into `src`.
pub(super) fn lex(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                let start = i;
                i += 1;
                let mut terminated = false;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        // `""` is an escape — stay inside the quoted region.
                        if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        terminated = true;
                        break;
                    }
                    i += 1;
                }
                toks.push(Token {
                    kind: if terminated {
                        TokenKind::Ident { quoted: true }
                    } else {
                        TokenKind::Unterminated { ident: true }
                    },
                    start,
                    end: i,
                });
            }
            b'\'' => {
                let start = i;
                // `E'…'` — a backslash escapes the next byte (PARSE-7).
                let is_escape = crate::util::opens_escape_string(bytes, start);
                i += 1;
                let mut terminated = false;
                while i < bytes.len() {
                    if let Some(next) = crate::util::escaped_pair_end(bytes, i, is_escape) {
                        i = next;
                        continue;
                    }
                    if bytes[i] == b'\'' {
                        // `''` is an escape — stay inside the string.
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        terminated = true;
                        break;
                    }
                    i += 1;
                }
                toks.push(Token {
                    kind: if terminated {
                        TokenKind::String
                    } else {
                        TokenKind::Unterminated { ident: false }
                    },
                    start,
                    end: i,
                });
            }
            b'$' if crate::util::read_dollar_tag_len(bytes, i).is_some() => {
                // `$tag$ ... $tag$` — one opaque token (a lone `$` / `$1`
                // positional parameter is not a tag opener and falls through to
                // the Symbol arm below).
                let tok = lex_dollar_string(bytes, i);
                i = tok.end;
                toks.push(tok);
            }
            _ if is_ident_byte(b) => {
                let start = i;
                // A `$` continues the name (`price$usd$`), as in DuckDB.
                while i < bytes.len() && crate::util::is_ident_continue(bytes[i]) {
                    i += 1;
                }
                toks.push(Token {
                    kind: TokenKind::Ident { quoted: false },
                    start,
                    end: i,
                });
            }
            _ => {
                toks.push(Token {
                    kind: TokenKind::Symbol(b),
                    start: i,
                    end: i + 1,
                });
                i += 1;
            }
        }
    }
    toks
}

/// Lex one `$tag$ ... $tag$` dollar-quoted string starting at `start`, which
/// MUST be a valid tag opener (`read_dollar_tag_len(bytes, start).is_some()`).
/// Scans to the matching close tag, or to end-of-input if it never appears
/// (`terminated == false`, which the parser turns into an error). Only the
/// IDENTICAL tag closes the region — a different inner tag (`$z$`) does not.
fn lex_dollar_string(bytes: &[u8], start: usize) -> Token {
    let open_len =
        crate::util::read_dollar_tag_len(bytes, start).expect("caller guards a valid tag opener");
    let tag = &bytes[start..start + open_len];
    let mut j = start + open_len;
    let mut terminated = false;
    while j < bytes.len() {
        if bytes[j] == b'$' && bytes[j..].starts_with(tag) {
            j += open_len;
            terminated = true;
            break;
        }
        j += 1;
    }
    Token {
        kind: TokenKind::DollarString { terminated },
        start,
        end: j,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).into_iter().map(|t| t.kind).collect()
    }

    /// Every token span lands on a char boundary and the spans tile the
    /// non-whitespace bytes left to right without overlap — i.e. the only bytes
    /// NOT covered by a token are whitespace. Asserting the gaps are whitespace
    /// (not just that tokens don't overlap) is what makes this catch a `lex()`
    /// regression that silently drops a non-whitespace byte.
    fn assert_well_formed(src: &str) {
        let toks = lex(src);
        let mut prev_end = 0;
        for t in &toks {
            assert!(
                src.is_char_boundary(t.start),
                "start not on boundary: {src:?}"
            );
            assert!(src.is_char_boundary(t.end), "end not on boundary: {src:?}");
            assert!(t.start >= prev_end, "overlap in {src:?}");
            assert!(t.end > t.start, "empty token in {src:?}");
            assert!(
                src[prev_end..t.start]
                    .bytes()
                    .all(|b| b.is_ascii_whitespace()),
                "non-whitespace bytes dropped in gap {prev_end}..{} of {src:?}",
                t.start
            );
            prev_end = t.end;
        }
        assert!(
            src[prev_end..].bytes().all(|b| b.is_ascii_whitespace()),
            "non-whitespace trailing bytes dropped after {prev_end} in {src:?}"
        );
    }

    #[test]
    fn bare_idents_and_symbols() {
        assert_eq!(
            kinds("o AS orders"),
            vec![
                TokenKind::Ident { quoted: false },
                TokenKind::Ident { quoted: false },
                TokenKind::Ident { quoted: false },
            ]
        );
    }

    #[test]
    fn dotted_name_is_ident_dot_ident() {
        let toks = lex("schema.orders");
        assert_eq!(
            toks.iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![
                TokenKind::Ident { quoted: false },
                TokenKind::Symbol(b'.'),
                TokenKind::Ident { quoted: false },
            ]
        );
        // Contiguity: no whitespace gaps between the three tokens.
        assert_eq!(toks[0].end, toks[1].start);
        assert_eq!(toks[1].end, toks[2].start);
    }

    #[test]
    fn quoted_ident_keeps_quotes_and_inner_bytes() {
        let toks = lex("\"my table\"");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::Ident { quoted: true });
        assert_eq!(&"\"my table\""[toks[0].start..toks[0].end], "\"my table\"");
    }

    #[test]
    fn quoted_ident_doubled_quote_escape() {
        // `"a""b"` is ONE token — the `""` does not close it.
        let toks = lex("\"a\"\"b\"");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::Ident { quoted: true });
        assert_eq!(toks[0].end, "\"a\"\"b\"".len());
    }

    #[test]
    fn keyword_inside_quoted_ident_is_one_token() {
        // The whole point: `"PRIMARY KEY (id)"` never surfaces PRIMARY/KEY as
        // matchable keyword tokens.
        let toks = lex("\"PRIMARY KEY (id)\"");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::Ident { quoted: true });
    }

    #[test]
    fn string_literal_with_escape_and_keywords() {
        let toks = lex("'a UNIQUE ''x'' mention'");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::String);
        assert_eq!(toks[0].end, "'a UNIQUE ''x'' mention'".len());
    }

    #[test]
    fn dollar_string_is_one_opaque_token() {
        // A `)` / `,` / keyword inside $$...$$ is not a matchable token.
        let toks = lex("$$a) b, AS c$$");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::DollarString { terminated: true });
        assert_eq!(
            &"$$a) b, AS c$$"[toks[0].start..toks[0].end],
            "$$a) b, AS c$$"
        );
    }

    #[test]
    fn tagged_dollar_string_closes_on_matching_tag_only() {
        // $t$ ... $t$ ; the inner $z$ does not close the region.
        let src = "$t$x $z$ y$t$";
        let toks = lex(src);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::DollarString { terminated: true });
        assert_eq!(toks[0].end, src.len());
    }

    #[test]
    fn unterminated_dollar_string_spans_to_eof() {
        let src = "$$unclosed ) , AS";
        let toks = lex(src);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::DollarString { terminated: false });
        assert_eq!(toks[0].end, src.len());
    }

    #[test]
    fn lone_dollar_and_positional_param_are_symbols() {
        // `$1` is a positional parameter, not a dollar-quote tag.
        let toks = lex("$1");
        assert_eq!(toks[0].kind, TokenKind::Symbol(b'$'));
        assert_eq!(toks[1].kind, TokenKind::Ident { quoted: false });
        // A lone trailing `$` (no closable tag) is just a symbol.
        assert_eq!(
            lex("$").first().map(|t| t.kind),
            Some(TokenKind::Symbol(b'$'))
        );
    }

    #[test]
    fn unterminated_quote_and_string() {
        assert_eq!(
            lex("\"unclosed").first().map(|t| t.kind),
            Some(TokenKind::Unterminated { ident: true })
        );
        assert_eq!(
            lex("'unclosed").first().map(|t| t.kind),
            Some(TokenKind::Unterminated { ident: false })
        );
        // Doubled-quote then no close is still unbalanced → unterminated.
        assert_eq!(
            lex("\"a\"\"b").first().map(|t| t.kind),
            Some(TokenKind::Unterminated { ident: true })
        );
    }

    // PARSE-7: `\'` inside a DuckDB escape string (`E'...'`) is an escaped
    // quote, so `e'\''` is a TERMINATED one-character literal. Reading `\`
    // as an ordinary byte made the middle and closing quotes look like one
    // `''` escape pair, so the token ran to EOF as Unterminated.
    #[test]
    fn escape_string_with_backslash_escaped_quote_is_terminated() {
        let toks = lex("e'\\''");
        // `e` lexes as a bare ident, then the string.
        assert_eq!(
            toks.last().map(|t| t.kind),
            Some(TokenKind::String),
            "tokens: {toks:?}"
        );
    }

    // An escape string whose LAST byte is the backslash has nothing to escape.
    // The two-byte advance must clamp, or the token's `end` lands one past the
    // buffer and any later `src[start..end]` panics. Raised by review on #205;
    // `skip_single_quoted` is safe by construction (it returns `len` after its
    // loop) and the other two scanners already clamped, so the lexer was the
    // one place the divergence bit.
    #[test]
    fn unterminated_escape_string_ending_in_a_backslash_stays_in_bounds() {
        let src = "e'\\";
        for t in lex(src) {
            assert!(
                t.end <= src.len(),
                "token {t:?} ends past the buffer (len {})",
                src.len()
            );
            // The span must also be sliceable, which is the actual failure mode.
            let _ = &src[t.start..t.end];
        }
    }

    // Same shape one byte further in, where the backslash escapes the quote and
    // the literal is genuinely unterminated.
    #[test]
    fn escape_string_with_escaped_final_quote_is_unterminated_and_in_bounds() {
        let src = "e'\\'";
        let toks = lex(src);
        assert_eq!(
            toks.last().map(|t| t.kind),
            Some(TokenKind::Unterminated { ident: false })
        );
        for t in toks {
            assert!(t.end <= src.len(), "token {t:?} ends past the buffer");
        }
    }

    // Control: `\\` escapes the backslash, so the next `'` still closes.
    #[test]
    fn escape_string_with_escaped_backslash_is_terminated() {
        assert_eq!(
            lex("e'\\\\'").last().map(|t| t.kind),
            Some(TokenKind::String)
        );
    }

    // Control: a typed literal's `E` belongs to the type name, so ordinary
    // string rules apply and a trailing backslash does not escape the close.
    #[test]
    fn typed_literal_is_not_an_escape_string() {
        assert_eq!(
            lex("DATE'2020-01-01'").last().map(|t| t.kind),
            Some(TokenKind::String)
        );
        // `'a\'` is a complete ordinary literal; nothing may follow it inside.
        assert_eq!(lex("'a\\'").last().map(|t| t.kind), Some(TokenKind::String));
    }

    #[test]
    fn non_ascii_is_one_bare_ident_no_midcodepoint_span() {
        let toks = lex("café東京");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::Ident { quoted: false });
        assert_eq!(&"café東京"[toks[0].start..toks[0].end], "café東京");
    }

    #[test]
    fn paren_group_tokens() {
        assert_eq!(
            kinds("(a, b)"),
            vec![
                TokenKind::Symbol(b'('),
                TokenKind::Ident { quoted: false },
                TokenKind::Symbol(b','),
                TokenKind::Ident { quoted: false },
                TokenKind::Symbol(b')'),
            ]
        );
    }

    #[test]
    fn well_formed_over_hostile_inputs() {
        for s in [
            "",
            "   ",
            "o AS \"weird PRIMARY KEY name\" PRIMARY KEY (id)",
            "\"a\"\"b\".\"c d\" UNIQUE (\"x)y\")",
            "COMMENT = 'the PRIMARY KEY (id) lives here'",
            "café AS \"東京 table\"",
            "'unterminated and PRIMARY KEY (id)",
            "\"unterminated ident",
            "o.a AS $$p, q.r AS s$$",
            "$t$x $z$ y$t$",
            "$$unterminated ) , AS",
            "$1 + $$tag$$",
        ] {
            assert_well_formed(s);
        }
    }

    // The tiling invariant is the whole point of the lexer — it must hold for
    // ARBITRARY input, not just the curated cases above (PR #100 review). These
    // proptests make the "no byte-slice ever cuts a codepoint" / "no
    // non-whitespace byte is dropped" guarantees generative rather than sampled.
    mod proptests {
        use super::assert_well_formed;
        use proptest::prelude::*;

        proptest! {
            /// A dense hostile alphabet: `"`/`'` (forming `""`/`''` escapes and
            /// unterminated regions), `(` `)` `;` `.` `=` `-`, unicode, and
            /// whitespace — the bytes whose handling the lexer is responsible for.
            #[test]
            fn tiling_holds_over_hostile_alphabet(s in r#"[-a-zé_ "'(),.;=$]{0,48}"#) {
                assert_well_formed(&s);
            }

            /// Fully arbitrary Unicode (control bytes, astral-plane codepoints)
            /// the curated alphabet can't reach — the generative form of the
            /// char-boundary guarantee.
            #[test]
            fn tiling_holds_over_arbitrary_unicode(s in r"[\s\S]{0,32}") {
                assert_well_formed(&s);
            }
        }
    }

    proptest! {
        /// Every token span must be sliceable, for ANY input.
        ///
        /// The escape-string advance (PARSE-7) is the second two-byte step in
        /// this lexer and the first that could run off the end: the `''` step
        /// is guarded by `i + 1 < len`, while the escape step needed an
        /// explicit clamp it did not originally have (raised by review on
        /// #205). Pinning the invariant covers the whole token stream rather
        /// than the one shape that happened to break it — a `.get()` rather
        /// than a length check, so a span landing inside a multi-byte
        /// codepoint fails here too.
        ///
        /// The alphabet is deliberately TINY and quote-heavy. A general
        /// `".{0,64}"` generator passes this property even with the clamp
        /// removed — it essentially never emits an escape string ending in a
        /// backslash, so it would have been a vacuous guard that merely looked
        /// like coverage. Verified: with the clamp reverted this generator
        /// fails, that one does not.
        #[test]
        fn every_token_span_is_sliceable(src in "[eE'\\\\\"$,a\u{e9}]{0,24}") {
            for t in lex(&src) {
                prop_assert!(t.start <= t.end, "inverted span {t:?}");
                prop_assert!(
                    src.get(t.start..t.end).is_some(),
                    "span {t:?} is not sliceable from {src:?} (len {})",
                    src.len()
                );
            }
        }
    }

    #[test]
    fn dollar_inside_a_name_continues_the_identifier() {
        let toks = lex("price$usd$ x");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].kind, TokenKind::Ident { quoted: false });
        assert_eq!((toks[0].start, toks[0].end), (0, 10));
    }
}
