//! One quote-aware, case-insensitive tokenizer for *identifier references in
//! expression text*, shared by the graph validators and the expression
//! inliners.
//!
//! ## Why this exists (E-2 / E-3 / E-5)
//!
//! Fact and derived-metric expansion works by **textual substitution**: a
//! metric expression like `profit AS revenue - cost` is produced by replacing
//! the identifiers `revenue` / `cost` with their resolved SQL. Before this
//! module that replacement was a family of hand-rolled word-boundary scanners
//! (`util::replace_word_boundary_pairs` and friends — since removed) that were **not**
//! quote-aware and disagreed with the validators on two things:
//!
//! - **E-2** — validation resolved references case-insensitively but the
//!   substitution matched case-sensitively / quote-blindly, so a quoted or
//!   mixed-case reference passed CREATE validation yet was silently not inlined.
//! - **E-3** — `.` was treated as a plain word boundary, so the bare needle
//!   `revenue` matched the column part of an *unrelated* qualified reference
//!   `x.revenue`, and a metric name inside a `'...'` string literal was
//!   substituted into — both producing invalid SQL from a valid definition.
//!
//! The fix is one tokenizer that understands SQL lexical structure:
//! [`scan_references`] walks an expression and yields the identifier
//! **reference chains** in it (`a`, `a.b`, `a.b."C d"`), skipping single-quoted
//! string literals and `$tag$…$tag$` dollar-quoted strings entirely. Two thin
//! consumers sit on top:
//!
//! - [`referenced_names_qualified`] / [`references_ref`] — the FIND primitive
//!   (which of these known names does this expression reference?), for
//!   dependency discovery and validation. A fact/metric matches bare or by its
//!   own `source_table.name`, never by a foreign relation's alias.
//! - [`inline_references`] — the INLINE primitive (splice a replacement over
//!   each reference whose normalized key is in the map).
//!
//! By construction this kills the class:
//! - string-literal / dollar-quote contents are never emitted as references, so
//!   they are never matched or substituted;
//! - `x.revenue` is a two-part chain with key `x.revenue`, so a bare needle
//!   `revenue` can never match it — no `.`-boundary special-case needed;
//! - matching goes through [`crate::ident::normalize_ident_part`], so it is
//!   case-insensitive **and** quote-insensitive (`"Revenue"` matches the
//!   declaration `revenue` — TECH-DEBT #28) with no third case/quote rule.
//!
//! Expressions reaching this layer are already comment-blanked at the parse
//! entry points (PA-7, [`crate::util::blank_sql_comments`]), and a query's
//! `where_clause` where `expand::where_clause` resolves it, so the tokenizer
//! does not handle SQL comments — they are spaces by the time we see them.
//! Only the subquery scan ([`subquery_spans`]) skips them as well, for its
//! callers that pass text with comments in it.
//!
//! The **alias-qualifier** rewriters (`expand::window` / `expand::sql_gen` /
//! `expand::semi_additive`, which rewrite the `a` in `a.city` to a scoped
//! alias `a__dep`) are a *different* operation — they rewrite the *head* part
//! of a chain rather than replacing the whole reference — but they go through
//! this same engine via [`rewrite_qualifier`], so they inherit its
//! literal-/function-/foreign-tail safety instead of the old quote-blind
//! `util::replace_word_boundary` (now retired).

use std::collections::HashMap;

/// One identifier reference found in expression text: a maximal run of
/// dot-joined identifier parts (`a`, `a.b`, `a.b."C d"`), with its byte span in
/// the original expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdentRef<'a> {
    /// Byte offset of the first byte of the chain in the source expression.
    pub(crate) start: usize,
    /// Byte offset one past the last byte of the chain.
    pub(crate) end: usize,
    /// The raw chain text, `&expr[start..end]` (quotes and all).
    pub(crate) raw: &'a str,
}

impl IdentRef<'_> {
    /// The case-folded, quote-stripped match key for this chain, e.g. `x.revenue`
    /// or `revenue`. Delegates to [`crate::ident::normalize_ident_part`], the
    /// single source of the identifier match rule, so FIND and INLINE agree with
    /// name resolution everywhere else.
    pub(crate) fn key(&self) -> String {
        crate::ident::normalize_ident_part(self.raw)
    }

    /// True when this chain is a single, *unqualified* part (`revenue`,
    /// `"Rev"`), false for a dotted chain (`a.b`, `o.amount`). Used by the
    /// derived-metric validator, which treats only bare identifiers as metric
    /// references — a qualified chain in a derived expression is a raw column,
    /// not a metric name.
    pub(crate) fn is_bare(&self) -> bool {
        first_part_len(self.raw) == self.raw.len()
    }

    /// The normalized key of the chain's **first** part — the qualifier:
    /// `o.amount` → `o`, `o.address.city` → `o`, and `"My Tbl".c` → `my tbl`.
    /// A bare chain returns its whole key.
    ///
    /// The mirror of [`last_part_key`](Self::last_part_key), and quote-aware
    /// for the same reason: a dot inside a quoted part does not separate
    /// qualifiers, so splitting the joined key on `.` would be wrong.
    pub(crate) fn first_part_key(&self) -> String {
        crate::ident::normalize_ident_part(&self.raw[..first_part_len(self.raw)])
    }

    /// The normalized (quote-stripped, lowercased) key of the chain's **last**
    /// part — the called name for a function head: `main.sum` → `sum`, but
    /// `"main.sum"` → `main.sum` because the dot lives *inside* a quoted part
    /// and so does not separate qualifiers. Splitting the joined [`key`] on `.`
    /// cannot make this distinction (a quoted dot and a qualifier dot look
    /// identical there), so this parses quote-aware via
    /// [`crate::ident::parse_qualified_identifier_with_quoting`].
    ///
    /// [`key`]: IdentRef::key
    pub(crate) fn last_part_key(&self) -> String {
        match crate::ident::parse_qualified_identifier_with_quoting(self.raw.trim()) {
            Ok(parts) => parts
                .last()
                .map(|(p, _)| p.to_ascii_lowercase())
                .unwrap_or_default(),
            Err(_) => crate::ident::normalize_ident_part(self.raw),
        }
    }
}

/// Scan `expr` and return every identifier reference chain in it, in source
/// order, skipping the contents of single-quoted string literals (`'…'` with
/// `''` escape) and `$tag$…$tag$` dollar-quoted strings.
///
/// A chain is one or more identifier parts joined by `.` with no intervening
/// whitespace; a part is either a bare run of identifier bytes
/// ([`crate::util::is_ident_byte`] — ASCII alphanumerics, `_`, and every
/// non-ASCII byte, so Unicode identifiers scan intact) or a `"…"` quoted part
/// (with `""` escape). The returned spans are non-overlapping. Together with
/// [`scan_function_heads`] they tile the non-literal identifier text, except
/// for the words in a keyword position, which neither returns (COR-3: see
/// [`scan_chains`]).
///
/// A chain immediately followed by `(` (skipping whitespace) is a **function
/// call**, not a name reference (`SUM(x)`, `date_trunc (...)`), and is not
/// emitted — so a fact/metric named like a function (`sum`) is never confused
/// with the call, in either FIND or INLINE. The complementary chains (the call
/// heads themselves) are available via [`scan_function_heads`].
pub(crate) fn scan_references(expr: &str) -> Vec<IdentRef<'_>> {
    scan_chains(expr, ChainKind::Reference)
}

/// Which class of identifier chain a scan collects: [`scan_references`] keeps
/// the chains that are *not* function-call heads; [`scan_function_heads`] keeps
/// the ones that *are*. Every chain is at most one of the two; a chain in a
/// keyword position (a cast's type, an `INTERVAL` unit, …) is neither.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChainKind {
    /// A name reference — a chain not immediately followed by `(`.
    Reference,
    /// A function-call head — a chain immediately followed by `(`.
    FunctionHead,
}

/// Walk `expr` and collect the identifier chains of the requested [`ChainKind`],
/// in source order, skipping single-quoted (`''`-escaped) and `$tag$…$tag$`
/// literal content. The one place that knows the tokenizer's lexical structure;
/// both public scanners are thin polarity choices over it.
///
/// Besides literals, it skips the words that sit in a **keyword position** of
/// `DuckDB`'s expression grammar (COR-3), which are part of the syntax rather
/// than names: a cast's target type, a typed literal's type, the field of
/// `EXTRACT(field FROM …)`, an `INTERVAL`'s unit and `AT TIME ZONE`. A chain
/// there belongs to neither [`ChainKind`].
fn scan_chains(expr: &str, kind: ChainKind) -> Vec<IdentRef<'_>> {
    let bytes = expr.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    // COR-3: the paren depth, and the depths of the argument lists of the open
    // `CAST(` / `TRY_CAST(` calls, so an `AS` directly inside one is known to
    // introduce the cast's target type (and an `AS` anywhere else is not).
    let mut depth = 0usize;
    let mut cast_depths: Vec<usize> = Vec::new();
    // COR-3: start offsets of chains already classified as keyword-position
    // words by a look-ahead (an EXTRACT field, an INTERVAL unit), which the
    // walk reaches later because the text in between still has to be scanned.
    let mut keyword_at: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        match b {
            b'\'' => i = skip_single_quoted(bytes, i),
            b'$' => {
                if let Some(next) = try_skip_dollar_quoted(bytes, i) {
                    i = next;
                } else {
                    i += 1;
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                if cast_depths.last() == Some(&depth) {
                    cast_depths.pop();
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            // COR-3: `::` is the cast operator; what follows is a type.
            b':' if bytes.get(i + 1) == Some(&b':') => {
                i = skip_type(bytes, skip_whitespace(bytes, i + 2));
            }
            _ if b == b'"' || crate::util::is_ident_byte(b) => {
                let end = scan_chain(bytes, i);
                let raw = &expr[i..end];
                // COR-3: an INTERVAL literal's unit (`INTERVAL '1' DAY`, and
                // `INTERVAL'1' DAY` with no space) is marked before the
                // typed-literal rules below swallow the `INTERVAL` keyword itself.
                if raw.eq_ignore_ascii_case("interval") {
                    if let Some(unit) = interval_unit_start(bytes, end) {
                        keyword_at.push(unit);
                        i = end;
                        continue;
                    }
                }
                // PARSE-12 (code-review 2026-08-08): a chain IMMEDIATELY
                // followed by `'` is a literal's introducer, not a name — the
                // escape-string forms `e'…'` / `E'…'` (PARSE-7 made them
                // first-class) and every typed literal (`DATE'2020-01-01'`, and
                // the qualified / quoted type names DuckDB also accepts:
                // `main.date'…'`, `"date"'…'`). Emitting it as a reference let
                // a member named `e` or `date` be substituted OVER the
                // introducer, turning a valid definition into a query-time
                // syntax error, and reported a phantom dependency that (since
                // PAR-6) changes join topology. The literal starts here, so the
                // string skipper runs from the quote — and it derives the
                // backslash rules from the SAME adjacency test
                // (`util::opens_escape_string`), which is what makes `e'\''`
                // (backslash escapes) and `DATE'…'` (backslash is data) both
                // come out right.
                if bytes.get(end) == Some(&b'\'') {
                    i = skip_single_quoted(bytes, end);
                    continue;
                }
                // COR-3: PARSE-12's rule holds with whitespace between too —
                // `DATE '…'` is the usual spelling of a typed literal — and for
                // every word of a multi-word type (`TIMESTAMP WITH TIME ZONE '…'`,
                // `DOUBLE PRECISION '…'`). In a valid expression a NAME is never
                // followed directly by a string literal, so the chain is a type
                // or a keyword (`LIKE '…'`) either way, never a reference. The
                // string itself is left to the walk: it is not an escape string,
                // and the skipper's adjacency test agrees.
                let type_end = skip_type_name_words(bytes, end);
                if bytes.get(skip_whitespace(bytes, type_end)) == Some(&b'\'') {
                    i = type_end;
                    continue;
                }
                if keyword_at.contains(&i) {
                    i = end;
                    continue;
                }
                if cast_depths.last() == Some(&depth) && raw.eq_ignore_ascii_case("as") {
                    i = skip_type(bytes, skip_whitespace(bytes, end));
                    continue;
                }
                if let Some(next) = match_words(bytes, i, &["at", "time", "zone"]) {
                    i = next;
                    continue;
                }
                let is_head = followed_by_open_paren(bytes, end);
                if is_head {
                    if raw.eq_ignore_ascii_case("cast") || raw.eq_ignore_ascii_case("try_cast") {
                        cast_depths.push(depth + 1);
                    } else if raw.eq_ignore_ascii_case("extract") {
                        if let Some(field) = extract_field_start(bytes, end) {
                            keyword_at.push(field);
                        }
                    }
                }
                let want = if is_head {
                    ChainKind::FunctionHead
                } else {
                    ChainKind::Reference
                };
                if want == kind {
                    out.push(IdentRef { start: i, end, raw });
                }
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

/// Scan `expr` and return every identifier chain that is a **function-call
/// head** — a chain immediately followed by `(` (skipping whitespace) — in
/// source order, skipping single-quoted and `$tag$…$tag$` literal content
/// exactly as [`scan_references`] does.
///
/// This is the complement of [`scan_references`] over the chains outside
/// keyword positions: such a chain is *either* a name reference (returned
/// there) *or* a call head (returned here), never both. A chain in a keyword
/// position (COR-3: a cast's type, a typed literal's type, an `EXTRACT` field,
/// an `INTERVAL` unit, `AT TIME ZONE`) is in neither result. It is the
/// engine primitive behind aggregate-function detection — matching the last
/// part of each head against a known-aggregate set — so that detection shares
/// the tokenizer's literal handling (a `sum(` inside `'…'` is not a call) and
/// its single identifier-byte rule (E-5: `Ωsum(` is one chain `Ωsum`, not the
/// aggregate `sum`).
pub(crate) fn scan_function_heads(expr: &str) -> Vec<IdentRef<'_>> {
    scan_chains(expr, ChainKind::FunctionHead)
}

/// Is the next non-whitespace byte at or after `pos` an opening `(`? Used to
/// classify an identifier chain as a function call rather than a name reference.
fn followed_by_open_paren(bytes: &[u8], pos: usize) -> bool {
    let mut j = pos;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    j < bytes.len() && bytes[j] == b'('
}

/// Offset of the first non-whitespace byte at or after `pos` (or `len`).
fn skip_whitespace(bytes: &[u8], pos: usize) -> usize {
    let mut j = pos;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    j
}

/// If the bare words `words` (case-insensitive, whole chains, whitespace
/// between) start at `pos`, the offset just past the last one.
fn match_words(bytes: &[u8], pos: usize, words: &[&str]) -> Option<usize> {
    let mut p = pos;
    let mut last = pos;
    for (n, word) in words.iter().enumerate() {
        if n > 0 {
            p = skip_whitespace(bytes, last);
        }
        if !bytes
            .get(p)
            .copied()
            .is_some_and(crate::util::is_ident_byte)
        {
            return None;
        }
        let end = scan_chain(bytes, p);
        if !bytes[p..end].eq_ignore_ascii_case(word.as_bytes()) {
            return None;
        }
        last = end;
    }
    Some(last)
}

/// The words that continue a multi-word `DuckDB` type name, after its first
/// word (`DOUBLE PRECISION`, `TIMESTAMP WITH TIME ZONE`, `INT ARRAY`).
const TYPE_NAME_CONTINUATIONS: &[&[&str]] = &[
    &["precision"],
    &["varying"],
    &["with", "time", "zone"],
    &["without", "time", "zone"],
    &["array"],
];

/// COR-3: for a chain ending at `end`, the offset just past the words that
/// continue it as a multi-word type name (`end` itself when none do).
fn skip_type_name_words(bytes: &[u8], end: usize) -> usize {
    let mut end = end;
    while let Some(next) = TYPE_NAME_CONTINUATIONS
        .iter()
        .find_map(|words| match_words(bytes, skip_whitespace(bytes, end), words))
    {
        end = next;
    }
    end
}

/// COR-3: skip a type name starting at `start` — a cast's target type — and
/// return the offset just past it. A type is a chain (bare, quoted or
/// qualified: `BLOB`, `"blob"`, `main.blob`) followed by any of a parameter
/// list (`DECIMAL(10, 2)`, `STRUCT(blob BLOB)` — field names, not members),
/// array suffixes (`[]`, `[3]`) and the words of a multi-word name, in any
/// order. Returns `start` unchanged when no chain starts there.
fn skip_type(bytes: &[u8], start: usize) -> usize {
    match bytes.get(start) {
        Some(&b) if b == b'"' || crate::util::is_ident_byte(b) => {}
        _ => return start,
    }
    let mut end = scan_chain(bytes, start);
    loop {
        let j = skip_whitespace(bytes, end);
        end = match bytes.get(j) {
            Some(b'(') => skip_balanced(bytes, j, b'(', b')'),
            Some(b'[') => skip_balanced(bytes, j, b'[', b']'),
            _ => match TYPE_NAME_CONTINUATIONS
                .iter()
                .find_map(|words| match_words(bytes, j, words))
            {
                Some(next) => next,
                None => return end,
            },
        };
    }
}

/// Offset just past the bracket that closes the `open` at `bytes[i]`, skipping
/// literal, quoted-identifier and comment content and nested pairs. Saturates
/// at `len`.
fn skip_balanced(bytes: &[u8], i: usize, open: u8, close: u8) -> usize {
    let len = bytes.len();
    let mut nesting = 0usize;
    let mut j = i;
    while j < len {
        let b = bytes[j];
        if starts_comment(bytes, j) {
            j = skip_whitespace_and_comments(bytes, j);
            continue;
        }
        if b == b'\'' {
            j = skip_single_quoted(bytes, j);
            continue;
        }
        if b == b'"' {
            j = skip_quoted_part(bytes, j);
            continue;
        }
        if b == b'$' {
            if let Some(next) = try_skip_dollar_quoted(bytes, j) {
                j = next;
                continue;
            }
        }
        if b == open {
            nesting += 1;
        } else if b == close {
            nesting -= 1;
            if nesting == 0 {
                return j + 1;
            }
        }
        j += 1;
    }
    len
}

/// `DuckDB`'s `INTERVAL` units, singular and plural.
const INTERVAL_UNITS: &[&str] = &[
    "year",
    "years",
    "month",
    "months",
    "day",
    "days",
    "hour",
    "hours",
    "minute",
    "minutes",
    "second",
    "seconds",
    "millisecond",
    "milliseconds",
    "microsecond",
    "microseconds",
    "week",
    "weeks",
    "quarter",
    "quarters",
    "decade",
    "decades",
    "century",
    "centuries",
    "millennium",
    "millennia",
    "millenniums",
];

/// COR-3: for the keyword `INTERVAL` ending at `end`, the start of its unit
/// word — `INTERVAL 1 DAY`, `INTERVAL '1' DAY`, `INTERVAL (n) DAY` — or `None`
/// when no unit follows. The operand `DuckDB` accepts there is a string, a number
/// or a parenthesized expression; only its extent is measured, so the
/// references inside `(n)` are still scanned by the walk.
fn interval_unit_start(bytes: &[u8], end: usize) -> Option<usize> {
    let op = skip_whitespace(bytes, end);
    let op_end = match bytes.get(op)? {
        b'\'' => skip_single_quoted(bytes, op),
        b'(' => skip_balanced(bytes, op, b'(', b')'),
        b if b.is_ascii_digit() || *b == b'.' => {
            let mut j = op;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                j += 1;
            }
            j
        }
        _ => return None,
    };
    let unit = skip_whitespace(bytes, op_end);
    INTERVAL_UNITS
        .iter()
        .any(|u| match_words(bytes, unit, &[u]).is_some())
        .then_some(unit)
}

/// COR-3: for the head `EXTRACT` ending at `end` (an open paren follows), the
/// start of its field — the word before `FROM` in `EXTRACT(year FROM ts)`, bare
/// or quoted — or `None` for any other argument shape.
fn extract_field_start(bytes: &[u8], end: usize) -> Option<usize> {
    let field = skip_whitespace(bytes, skip_whitespace(bytes, end) + 1);
    match bytes.get(field) {
        Some(&b) if b == b'"' || crate::util::is_ident_byte(b) => {}
        _ => return None,
    }
    let after = skip_whitespace(bytes, scan_chain(bytes, field));
    match_words(bytes, after, &["from"]).map(|_| field)
}

/// Byte offset just past the closing `'` of a single-quoted string starting at
/// `bytes[i] == '\''`, honouring the `''` escape. Saturates at `len` for an
/// unterminated literal.
pub(crate) fn skip_single_quoted(bytes: &[u8], i: usize) -> usize {
    let len = bytes.len();
    // `E'…'` — a backslash escapes the next byte (PARSE-7).
    let is_escape = crate::util::opens_escape_string(bytes, i);
    let mut j = i + 1;
    while j < len {
        if let Some(next) = crate::util::escaped_pair_end(bytes, j, is_escape) {
            j = next;
            continue;
        }
        if bytes[j] == b'\'' {
            if j + 1 < len && bytes[j + 1] == b'\'' {
                j += 2; // '' escape — stay inside the literal
                continue;
            }
            return j + 1; // closing quote consumed
        }
        j += 1;
    }
    len
}

/// If `bytes[i]` opens a valid `$tag$` dollar quote, return the byte offset just
/// past its matching close (saturating at `len` if unterminated); otherwise
/// `None` (so a lone `$` is treated as an ordinary non-identifier byte).
pub(crate) fn try_skip_dollar_quoted(bytes: &[u8], i: usize) -> Option<usize> {
    let tag_len = crate::util::read_dollar_tag_len(bytes, i)?;
    let len = bytes.len();
    let tag = &bytes[i..i + tag_len];
    let mut j = i + tag_len;
    while j + tag_len <= len {
        if &bytes[j..j + tag_len] == tag {
            return Some(j + tag_len);
        }
        j += 1;
    }
    Some(len) // unterminated dollar quote — swallow to end
}

/// Scan one identifier chain starting at `bytes[start]` (a `"` or an identifier
/// byte). Returns the byte offset one past the chain.
fn scan_chain(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start;
    loop {
        if i < len && bytes[i] == b'"' {
            i = skip_quoted_part(bytes, i);
        } else {
            let part_start = i;
            while i < len && crate::util::is_ident_byte(bytes[i]) {
                i += 1;
            }
            if i == part_start {
                // No progress (e.g. a stray `.` with no following part) — stop.
                break;
            }
        }
        // Continue the chain only across a `.` that is *immediately* followed by
        // another part (`a.b`, `a."b"`), never across whitespace (`a . b` is two
        // chains) — mirroring `ident::parse_qualified_identifier`.
        if i + 1 < len
            && bytes[i] == b'.'
            && (bytes[i + 1] == b'"' || crate::util::is_ident_byte(bytes[i + 1]))
        {
            i += 1; // consume the dot; loop for the next part
        } else {
            break;
        }
    }
    i
}

/// Byte offset just past a `"…"` quoted identifier part starting at
/// `bytes[i] == '"'`, honouring the `""` escape. Saturates at `len` for an
/// unterminated quote.
fn skip_quoted_part(bytes: &[u8], i: usize) -> usize {
    let len = bytes.len();
    let mut j = i + 1;
    while j < len {
        if bytes[j] == b'"' {
            if j + 1 < len && bytes[j + 1] == b'"' {
                j += 2; // "" escape
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    len
}

/// Byte length of the **first part** of a reference chain (`a` in `a.city`, the
/// whole `"C d"` in `"C d".x`, or the entire chain when it is bare). `chain` is
/// assumed well-formed as produced by [`scan_chain`] — it starts with a `"` or
/// an identifier byte. Used to isolate the leading qualifier for
/// [`rewrite_qualifier`] and to decide [`IdentRef::is_bare`].
fn first_part_len(chain: &str) -> usize {
    let bytes = chain.as_bytes();
    if bytes.first() == Some(&b'"') {
        skip_quoted_part(bytes, 0)
    } else {
        let mut i = 0;
        while i < bytes.len() && crate::util::is_ident_byte(bytes[i]) {
            i += 1;
        }
        i
    }
}

/// The set of normalized keys of every reference chain in `expr` (bare and
/// qualified), skipping string / dollar-quoted literal content. The FIND
/// building block: a name is referenced iff one of its acceptable keys is in
/// this set.
pub(crate) fn reference_keys(expr: &str) -> std::collections::HashSet<String> {
    scan_references(expr).into_iter().map(|r| r.key()).collect()
}

/// Of `candidates`, those referenced in `expr`, in slice order (each once).
///
/// Each candidate is `(name, source_table)`. It is referenced when a reference
/// chain in `expr` matches either its **bare** key (`name`) or, when it has a
/// source table, its **own-qualified** key (`source_table.name`). A *foreign*
/// qualified reference (`x.name` where `x` is not the candidate's source table)
/// does not match — this is the E-3 fix — and neither does text inside a
/// string / dollar-quoted literal. Matching is quote- and case-insensitive via
/// [`crate::ident::normalize_ident_part`], so `"Revenue"` matches `revenue`.
///
/// This is the shared FIND primitive for fact / metric dependency discovery: a
/// fact or base metric may be referenced bare (`revenue`) or qualified by its
/// own source table (`li.revenue`), but never by another relation's alias.
pub(crate) fn referenced_names_qualified<'a>(
    expr: &str,
    candidates: &[(&'a str, Option<&str>)],
) -> Vec<&'a str> {
    let keys = reference_keys(expr);
    candidates
        .iter()
        .filter(|(name, src)| {
            keys.contains(&crate::ident::normalize_ident_part(name))
                || src.is_some_and(|s| {
                    keys.contains(&crate::ident::normalize_ident_part(&format!("{s}.{name}")))
                })
        })
        .map(|(name, _)| *name)
        .collect()
}

/// Whether `expr` references the fact/metric `name` (bare or qualified by its
/// own `source_table`). The single-candidate FIND convenience.
pub(crate) fn references_ref(expr: &str, name: &str, source_table: Option<&str>) -> bool {
    !referenced_names_qualified(expr, &[(name, source_table)]).is_empty()
}

/// Replace each identifier reference in `expr` whose normalized key
/// ([`IdentRef::key`]) is present in `replacements` with the mapped text, in a
/// single left-to-right pass over the *original* expression.
///
/// Because the expression is tokenized once and replacements are spliced by
/// byte span, inserted text is never rescanned — the double-substitution hazard
/// that forced the former `util::replace_word_boundary_pairs`'s single-pass
/// design (SG-3) cannot occur here, and there is no needle-ordering concern: each chain
/// resolves to exactly one key (`o.net_price` and `net_price` are distinct
/// keys). Literal text and unmatched references are copied verbatim, so a
/// metric name inside `'…'` or a foreign `x.net_price` is left intact.
///
/// Keys are normalized identifier keys (as produced by
/// [`crate::ident::normalize_ident_part`]): a bare `revenue` or a qualified
/// `o.net_price`.
pub(crate) fn inline_references(expr: &str, replacements: &HashMap<String, &str>) -> String {
    splice_references(expr, scan_references(expr), replacements)
}

/// [`inline_references`] for a predicate that may hold subqueries: a reference
/// inside one ([`subquery_spans`]) is the subquery's own name and is left as
/// written.
pub(crate) fn inline_references_outside_subqueries(
    expr: &str,
    replacements: &HashMap<String, &str>,
) -> String {
    splice_references(expr, scan_references_outside_subqueries(expr), replacements)
}

fn splice_references(
    expr: &str,
    refs: Vec<IdentRef<'_>>,
    replacements: &HashMap<String, &str>,
) -> String {
    if replacements.is_empty() {
        return expr.to_string();
    }
    let mut out = String::with_capacity(expr.len());
    let mut copied = 0;
    for r in refs {
        if let Some(repl) = replacements.get(&r.key()) {
            out.push_str(&expr[copied..r.start]);
            out.push_str(repl);
            copied = r.end;
        }
    }
    out.push_str(&expr[copied..]);
    out
}

/// The reserved words that open a query, so that a `(` whose first word is one
/// of them starts a subquery. `DuckDB` also starts a query with unreserved words
/// (`PIVOT`, `SUMMARIZE`, `SHOW`, …), which could equally be a member's name
/// inside ordinary parentheses, so those are not counted.
const QUERY_OPENERS: &[&str] = &["select", "with", "from", "values", "table"];

/// The byte spans `(start, end)` of the subqueries in `expr`, in order: each `(`
/// that opens a query ([`opens_query`]), through its matching `)`. A subquery
/// nested in another lies inside the outer span and is not listed separately.
/// Literal, quoted-identifier and comment content is skipped: `semantic_view()`'s
/// `where_clause` arrives with its comments (only the construct strips them
/// first), and a bracket in one opens or closes nothing.
pub(crate) fn subquery_spans(expr: &str) -> Vec<(usize, usize)> {
    let bytes = expr.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if starts_comment(bytes, i) {
            i = skip_whitespace_and_comments(bytes, i);
            continue;
        }
        match bytes[i] {
            b'\'' => i = skip_single_quoted(bytes, i),
            b'"' => i = skip_quoted_part(bytes, i),
            b'$' => i = try_skip_dollar_quoted(bytes, i).unwrap_or(i + 1),
            b'(' => match opens_query(bytes, i) {
                Opens::Query => {
                    let end = skip_balanced(bytes, i, b'(', b')');
                    out.push((i, end));
                    i = end;
                }
                Opens::NoQueryBefore(next) => i = next,
            },
            _ => i += 1,
        }
    }
    out
}

/// Whether a `(` opens a query, from [`opens_query`].
enum Opens {
    Query,
    /// No `(` from this one up to the offset opens a query, so the scan
    /// resumes there rather than asking again of each bracket in a chain.
    NoQueryBefore(usize),
}

/// `DuckDB` refuses an expression nested deeper than this
/// (`max_expression_depth`), so a longer chain of directly nested brackets is
/// no query that runs; the scan stops following it there.
const MAX_BRACKET_CHAIN: usize = 1000;

/// Whether the `(` at `open` starts a query: its first word is one of
/// [`QUERY_OPENERS`], or it starts with a bracketed query that a set operation,
/// `ORDER BY`, `LIMIT` or `OFFSET` continues (`((SELECT …) UNION SELECT …)`).
/// Comments are skipped.
///
/// The brackets that open directly inside one another (`((( …`) are walked
/// in a loop, not by recursion, since their number comes from the caller's
/// text.
fn opens_query(bytes: &[u8], open: usize) -> Opens {
    let mut chain = vec![open];
    let mut first = skip_whitespace_and_comments(bytes, open + 1);
    while bytes.get(first) == Some(&b'(') {
        if chain.len() == MAX_BRACKET_CHAIN {
            return Opens::NoQueryBefore(first);
        }
        chain.push(first);
        first = skip_whitespace_and_comments(bytes, first + 1);
    }
    let mut word_end = first;
    while word_end < bytes.len() && crate::util::is_ident_byte(bytes[word_end]) {
        word_end += 1;
    }
    let word = String::from_utf8_lossy(&bytes[first..word_end]);
    if !QUERY_OPENERS.iter().any(|w| w.eq_ignore_ascii_case(&word)) {
        // The innermost bracket holds no query, so none around it does.
        return Opens::NoQueryBefore(first);
    }
    // The innermost bracket holds a query. Each one around it does too only if
    // a clause of a query continues the bracketed query just inside it.
    for &inner in &chain[1..] {
        let after = skip_whitespace_and_comments(bytes, skip_balanced(bytes, inner, b'(', b')'));
        if !QUERY_CONTINUATIONS
            .iter()
            .any(|words| match_words(bytes, after, words).is_some())
        {
            return Opens::NoQueryBefore(inner);
        }
    }
    Opens::Query
}

/// Whether a `/* … */` or `-- …` comment starts at `pos`.
fn starts_comment(bytes: &[u8], pos: usize) -> bool {
    bytes[pos..].starts_with(b"/*") || bytes[pos..].starts_with(b"--")
}

/// The words that continue a query after a bracketed one: a set operation, or
/// a clause that orders or limits the bracketed query.
const QUERY_CONTINUATIONS: &[&[&str]] = &[
    &["union"],
    &["intersect"],
    &["except"],
    &["order", "by"],
    &["limit"],
    &["offset"],
];

/// Offset of the first byte at or after `pos` that is neither whitespace nor
/// inside a `/* … */` or `-- …` comment (or `len`).
fn skip_whitespace_and_comments(bytes: &[u8], pos: usize) -> usize {
    let mut j = pos;
    loop {
        j = skip_whitespace(bytes, j);
        if bytes[j..].starts_with(b"/*") {
            j = bytes[j + 2..]
                .windows(2)
                .position(|w| w == b"*/")
                .map_or(bytes.len(), |p| j + 2 + p + 2);
        } else if bytes[j..].starts_with(b"--") {
            j = bytes[j..]
                .iter()
                .position(|&b| b == b'\n' || b == b'\r')
                .map_or(bytes.len(), |p| j + p + 1);
        } else {
            return j;
        }
    }
}

/// [`scan_references`] without the references inside a subquery
/// ([`subquery_spans`]), which name the subquery's own columns and tables.
pub(crate) fn scan_references_outside_subqueries(expr: &str) -> Vec<IdentRef<'_>> {
    let spans = subquery_spans(expr);
    scan_references(expr)
        .into_iter()
        .filter(|r| !spans.iter().any(|&(s, e)| r.start >= s && r.end <= e))
        .collect()
}

/// Rewrite the leading **qualifier** (first part) of every reference chain in
/// `expr` whose first part matches `alias`, replacing just that part with
/// `replacement` and leaving the rest of the chain intact (`a.city` →
/// `<replacement>.city`; a bare `a` → `<replacement>`).
///
/// This is the role-playing alias rewrite (`expand::window` / `sql_gen` /
/// `semi_additive` rewrite a dimension's `source_table` alias to a scoped
/// alias). It matches the first part through [`crate::ident::normalize_ident_part`],
/// so a quoted or mixed-case qualifier (`"A".city`) is rewritten too, and —
/// unlike the retired quote-blind `util::replace_word_boundary` — it operates
/// only on genuine reference chains: text inside a `'…'` / `$tag$…$` literal is
/// never touched, a function-call head (`date(x)` when `alias == "date"`) is
/// left alone, and a *foreign* qualified tail (`x.a` when `alias == "a"`) does
/// not match because only the chain's own first part is compared.
pub(crate) fn rewrite_qualifier(expr: &str, alias: &str, replacement: &str) -> String {
    let want = crate::ident::normalize_ident_part(alias);
    let mut out = String::with_capacity(expr.len());
    let mut copied = 0;
    for r in scan_references(expr) {
        let head_len = first_part_len(r.raw);
        if crate::ident::normalize_ident_part(&r.raw[..head_len]) == want {
            out.push_str(&expr[copied..r.start]);
            out.push_str(replacement);
            out.push_str(&r.raw[head_len..]);
            copied = r.end;
        }
    }
    out.push_str(&expr[copied..]);
    out
}

#[cfg(test)]
mod tests {

    // PARSE-7: in a DuckDB escape string `\'` is an escaped quote, so the
    // literal continues past it. Treating `\` as ordinary consumed the middle
    // and closing quotes as one `''` pair and ran to the end of the buffer.
    #[test]
    fn skip_single_quoted_honours_backslash_escape_in_an_escape_string() {
        let s = "e'\\'',rest";
        let bytes = s.as_bytes();
        let open = s.find('\'').unwrap();
        assert_eq!(
            skip_single_quoted(bytes, open),
            s.find(',').unwrap(),
            "the literal ends before the comma"
        );
    }

    // Control: `\\` escapes the backslash, so the next quote closes.
    #[test]
    fn skip_single_quoted_escaped_backslash_does_not_swallow_the_close() {
        let s = "e'\\\\',rest";
        let bytes = s.as_bytes();
        let open = s.find('\'').unwrap();
        assert_eq!(skip_single_quoted(bytes, open), s.find(',').unwrap());
    }

    // Control: an ordinary literal treats `\` as data.
    #[test]
    fn skip_single_quoted_ordinary_string_treats_backslash_as_data() {
        let s = "'a\\',rest";
        let bytes = s.as_bytes();
        assert_eq!(skip_single_quoted(bytes, 0), s.find(',').unwrap());
    }
    use super::*;
    use proptest::prelude::*;

    fn keys(expr: &str) -> Vec<String> {
        scan_references(expr).iter().map(IdentRef::key).collect()
    }

    #[test]
    fn scans_bare_and_qualified_chains() {
        assert_eq!(keys("revenue - cost"), vec!["revenue", "cost"]);
        // `SUM` is a function call, not a name reference — only its argument is.
        assert_eq!(keys("SUM(o.amount)"), vec!["o.amount"]);
        assert_eq!(keys("a.b.c"), vec!["a.b.c"]);
    }

    #[test]
    fn function_call_heads_are_not_references() {
        // A chain immediately before `(` (any whitespace) is a call, not a ref,
        // so a fact/metric named like a function is never confused with it.
        assert_eq!(keys("sum(x)"), vec!["x"]);
        // The numeric literal `0` scans as a harmless chain (it can never be a
        // declared name, so it never matches); the function head is excluded.
        assert_eq!(keys("COALESCE ( revenue , 0 )"), vec!["revenue", "0"]);
        assert_eq!(keys("schema.date_trunc('day', o.ts)"), vec!["o.ts"]);
        // FIND/INLINE both ignore the function name even if a name collides.
        assert!(!references_ref("SUM(x)", "sum", None));
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("sum".to_string(), "(BAD)");
        assert_eq!(inline_references("SUM(net_price)", &m), "SUM(net_price)");
    }

    #[test]
    fn quoted_parts_fold_and_join() {
        // A quoted part normalizes exactly like an unquoted one (DuckDB rule).
        assert_eq!(keys("\"Revenue\""), vec!["revenue"]);
        assert_eq!(keys("a.\"C d\""), vec!["a.c d"]);
        assert_eq!(keys("SUM(\"Rev\")"), vec!["rev"]);
        // "" escape inside a quoted part stays one part.
        assert_eq!(keys("\"with\"\"q\""), vec!["with\"q"]);
    }

    #[test]
    fn string_literals_yield_no_references() {
        assert!(scan_references("'total revenue'").is_empty());
        assert!(scan_references("'it''s revenue'").is_empty());
        assert_eq!(keys("revenue + 'revenue'"), vec!["revenue"]);
    }

    #[test]
    fn dollar_quoted_strings_yield_no_references() {
        assert!(scan_references("$$revenue$$").is_empty());
        assert!(scan_references("$tag$a.b.c$tag$").is_empty());
        assert_eq!(keys("x $$revenue$$ y"), vec!["x", "y"]);
        // A lone `$` is not a dollar quote (or a positional `$1`): scanning
        // continues past it.
        assert_eq!(keys("revenue $ cost"), vec!["revenue", "cost"]);
    }

    #[test]
    fn dotted_chain_is_not_split_by_whitespace() {
        // `a . b` (spaced) is two chains, `a.b` (contiguous) is one.
        assert_eq!(keys("a . b"), vec!["a", "b"]);
        assert_eq!(keys("a.b"), vec!["a.b"]);
    }

    // ----- referenced_names_qualified / references_ref (FIND) -----

    /// Bare-only FIND helper (no source table) for the tests below.
    fn bare(expr: &str, names: &[&'static str]) -> Vec<&'static str> {
        let cands: Vec<(&str, Option<&str>)> = names.iter().map(|&n| (n, None)).collect();
        referenced_names_qualified(expr, &cands)
    }

    #[test]
    fn find_is_case_and_quote_insensitive() {
        // E-2: a mixed-case / quoted reference still resolves.
        assert_eq!(
            bare("REVENUE - Cost", &["revenue", "cost"]),
            vec!["revenue", "cost"]
        );
        assert_eq!(bare("\"Revenue\"", &["revenue"]), vec!["revenue"]);
    }

    #[test]
    fn find_skips_foreign_qualified_and_string_literals() {
        // E-3: a *foreign* `x.revenue` is NOT a `revenue` reference, and a
        // string literal is never a reference.
        assert!(bare("x.revenue", &["revenue"]).is_empty());
        assert!(bare("'revenue'", &["revenue"]).is_empty());
        assert!(!references_ref("x.revenue + 'revenue'", "revenue", None));
        assert!(references_ref("revenue + x.revenue", "revenue", None));
    }

    #[test]
    fn find_matches_own_source_qualified_but_not_foreign() {
        // A fact/metric `net_price` on table `o` is referenced by `o.net_price`
        // (own table) but NOT by `x.net_price` (foreign) — the distinction the
        // bare-only scan could not make.
        assert!(references_ref("o.net_price + 1", "net_price", Some("o")));
        assert!(!references_ref("x.net_price + 1", "net_price", Some("o")));
        // Bare still matches regardless of source table.
        assert!(references_ref("SUM(net_price)", "net_price", Some("o")));
    }

    #[test]
    fn find_preserves_names_order_and_dedupes() {
        assert_eq!(
            bare("cost + revenue + cost", &["revenue", "cost", "profit"]),
            vec!["revenue", "cost"]
        );
    }

    // ----- inline_references (INLINE) -----

    #[test]
    fn inline_bare_and_qualified_keys() {
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("revenue".to_string(), "(SUM(o.amount))");
        assert_eq!(
            inline_references("revenue - cost", &m),
            "(SUM(o.amount)) - cost"
        );
        // A foreign qualified column is left intact (E-3).
        assert_eq!(inline_references("x.revenue", &m), "x.revenue");
        // Own qualified form matches its own key.
        m.insert("o.net_price".to_string(), "(price)");
        assert_eq!(inline_references("SUM(o.net_price)", &m), "SUM((price))");
    }

    #[test]
    fn inline_leaves_string_literals_intact() {
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("revenue".to_string(), "(SUM(o.amount))");
        // The metric name inside the literal survives; the bare ref is replaced.
        assert_eq!(
            inline_references("revenue || ' revenue '", &m),
            "(SUM(o.amount)) || ' revenue '"
        );
    }

    #[test]
    fn inline_is_case_and_quote_insensitive() {
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("revenue".to_string(), "(R)");
        m.insert("cost".to_string(), "(C)");
        // E-2 + #28: mixed-case and quoted references both inline.
        assert_eq!(inline_references("REVENUE - \"Cost\"", &m), "(R) - (C)");
    }

    #[test]
    fn inline_does_not_rescan_inserted_text() {
        // Replacement text that itself contains a key is not re-substituted.
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("net_price".to_string(), "(o.net_price)");
        assert_eq!(
            inline_references("SUM(net_price)", &m),
            "SUM((o.net_price))"
        );
    }

    // ----- is_bare / scan_function_heads / rewrite_qualifier -----

    #[test]
    fn is_bare_distinguishes_qualified_chains() {
        let bare: Vec<bool> = scan_references("revenue + o.amount + \"Rev\" + a.\"C d\"")
            .iter()
            .map(IdentRef::is_bare)
            .collect();
        // revenue (bare), o.amount (qualified), "Rev" (bare), a."C d" (qualified)
        assert_eq!(bare, vec![true, false, true, false]);
    }

    #[test]
    fn function_heads_are_the_complement_of_references() {
        let expr = "SUM(o.amount) + main.avg(x) - revenue";
        let heads: Vec<String> = scan_function_heads(expr)
            .iter()
            .map(IdentRef::key)
            .collect();
        assert_eq!(heads, vec!["sum", "main.avg"]);
        // The same chains never appear as references…
        let refs: Vec<String> = keys(expr);
        assert_eq!(refs, vec!["o.amount", "x", "revenue"]);
        // …and reference vs head spans are disjoint.
        let head_spans: Vec<(usize, usize)> = scan_function_heads(expr)
            .iter()
            .map(|r| (r.start, r.end))
            .collect();
        for r in scan_references(expr) {
            assert!(!head_spans.contains(&(r.start, r.end)));
        }
    }

    #[test]
    fn function_heads_skip_literal_content() {
        // A `sum(` inside a string / dollar literal is not a call head.
        assert!(scan_function_heads("'sum(x)'").is_empty());
        assert!(scan_function_heads("$$avg(y)$$").is_empty());
    }

    #[test]
    fn rewrite_qualifier_rewrites_only_the_head_part() {
        // Bare-alias qualifier before a dot is rewritten; the column part stays.
        assert_eq!(rewrite_qualifier("a.city", "a", "a__dep"), "a__dep.city");
        // Every own-qualified chain in the expr is rewritten.
        assert_eq!(
            rewrite_qualifier("a.city || ' from ' || a.country", "a", "a__dep"),
            "a__dep.city || ' from ' || a__dep.country"
        );
        // A bare occurrence of the alias is rewritten wholesale.
        assert_eq!(rewrite_qualifier("a", "a", "a__dep"), "a__dep");
    }

    #[test]
    fn rewrite_qualifier_is_case_and_quote_insensitive() {
        assert_eq!(rewrite_qualifier("A.city", "a", "a__dep"), "a__dep.city");
        assert_eq!(
            rewrite_qualifier("\"A\".city", "a", "a__dep"),
            "a__dep.city"
        );
    }

    #[test]
    fn rewrite_qualifier_leaves_literals_functions_and_foreign_tails_intact() {
        // The alias `a` inside a string literal must NOT be rewritten (the E-3
        // hazard that quote-blind replace_word_boundary had).
        assert_eq!(
            rewrite_qualifier("a.city || ' a '", "a", "a__dep"),
            "a__dep.city || ' a '"
        );
        // A function head named like the alias is not a qualifier.
        assert_eq!(
            rewrite_qualifier("a(o.x) + a.city", "a", "a__dep"),
            "a(o.x) + a__dep.city"
        );
        // A FOREIGN qualified tail `x.a` is not the alias `a` as a qualifier.
        assert_eq!(
            rewrite_qualifier("x.a + a.city", "a", "a__dep"),
            "x.a + a__dep.city"
        );
        // No match → unchanged.
        assert_eq!(rewrite_qualifier("b.city", "a", "a__dep"), "b.city");
    }

    // ----- PARSE-12: a chain abutting a `'` introduces a LITERAL -----
    //
    // (code-review 2026-08-08) `e` / `E` / `DATE` / any type name immediately
    // before a quote is the literal's introducer, not a name: the `e` in
    // `e'\''` is an ident byte, so `scan_chain` emitted a one-byte Reference
    // and the string skipper only saw the quote afterwards. DuckDB parses all
    // of these as one literal token — including the qualified and quoted type
    // names (`main.date'…'`, `"date"'…'`), verified against DuckDB — so the
    // whole chain is suppressed, not just a bare `e`.

    #[test]
    fn an_escape_string_introducer_is_not_a_reference() {
        // `count_if(x = e'\'')`
        let expr = "count_if(x = e'\\'')";
        assert_eq!(keys(expr), vec!["x"], "the `e` introduces the literal");
        // FIND: a member named `e` is NOT a dependency of this expression
        // (a phantom one changes join topology since PAR-6).
        assert!(!references_ref(expr, "e", None));
    }

    #[test]
    fn inlining_does_not_corrupt_an_escape_string_literal() {
        // With a declared member named `e`, INLINE spliced the replacement over
        // the introducer: `count_if(x = (o.something)'\'')` — a query-time
        // syntax error produced from a valid definition.
        let expr = "count_if(x = e'\\'')";
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("e".to_string(), "(o.something)");
        assert_eq!(inline_references(expr, &m), expr);
    }

    #[test]
    fn rewriting_a_qualifier_does_not_corrupt_an_escape_string_literal() {
        // The role-playing alias rewriter turned `e'a'` into `e__dep'a'`
        // whenever the rewritten alias happened to be named `e`.
        assert_eq!(
            rewrite_qualifier("e.name || e'a'", "e", "e__dep"),
            "e__dep.name || e'a'"
        );
        // Uppercase introducer, same rule.
        assert_eq!(
            rewrite_qualifier("e.name || E'a'", "e", "e__dep"),
            "e__dep.name || E'a'"
        );
    }

    #[test]
    fn a_typed_literal_prefix_is_not_a_reference() {
        // A fact named `date` corrupted `o.ts > DATE'2020-01-01'`.
        let expr = "o.ts > DATE'2020-01-01'";
        assert_eq!(keys(expr), vec!["o.ts"]);
        assert!(!references_ref(expr, "date", None));
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("date".to_string(), "(o.day)");
        assert_eq!(inline_references(expr, &m), expr);
        // DuckDB accepts a qualified and a quoted type name in this position
        // (`main.date'…'` / `"date"'…'` both parse), so the whole chain is the
        // introducer — not just its last part.
        assert!(scan_references("main.date'2020-01-01'").is_empty());
        assert!(scan_references("\"date\"'2020-01-01'").is_empty());
    }

    #[test]
    fn a_spaced_typed_literal_prefix_is_not_a_reference() {
        // DuckDB reads `DATE '2020-01-01'` — with the space, the usual
        // spelling — as the same typed literal as `DATE'2020-01-01'`, so a
        // member named `date` must not be spliced over its type either.
        let expr = "o.ts > DATE '2020-01-01'";
        assert_eq!(keys(expr), vec!["o.ts"]);
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("date".to_string(), "(o.day)");
        assert_eq!(inline_references(expr, &m), expr);
        assert_eq!(keys("x > TIMESTAMP\n  '2020-01-01 00:00'"), vec!["x"]);
        assert_eq!(keys("ts + INTERVAL '1 day'"), vec!["ts"]);
    }

    #[test]
    fn a_multi_word_typed_literal_type_is_not_a_reference() {
        // Review of #267: every word of a multi-word type is the literal's
        // type, not just the one before the quote.
        assert_eq!(
            keys("ts > TIMESTAMP WITH TIME ZONE '2020-01-01 00:00:00+00'"),
            vec!["ts"]
        );
        assert_eq!(keys("x = DOUBLE PRECISION '1.5'"), vec!["x"]);
        assert_eq!(keys("x = CHARACTER VARYING 'x'"), vec!["x"]);
        assert_eq!(keys("t = TIME WITHOUT TIME ZONE '10:00'"), vec!["t"]);
        assert_eq!(
            keys("ts > TIMESTAMP WITH TIME ZONE'2020-01-01'"),
            vec!["ts"]
        );
        let expr = "ts > TIMESTAMP WITH TIME ZONE '2020-01-01 00:00:00+00'";
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("time".to_string(), "(o.t)");
        assert_eq!(inline_references(expr, &m), expr);
    }

    #[test]
    fn the_unit_of_an_abutting_interval_literal_is_not_a_reference() {
        // Review of #267: `INTERVAL'1' DAY` (no space before the quote) is the
        // same literal as `INTERVAL '1' DAY`.
        assert_eq!(keys("ts + INTERVAL'1' DAY"), vec!["ts"]);
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("day".to_string(), "(o.ts::DATE)");
        assert_eq!(
            inline_references("ts < INTERVAL'1' DAY", &m),
            "ts < INTERVAL'1' DAY"
        );
    }

    #[test]
    fn a_chain_not_abutting_a_quote_still_scans() {
        // Controls for the suppression rule — it is ADJACENCY-based (the same
        // rule `util::opens_escape_string` uses), so an ordinary reference near
        // a literal is untouched.
        assert_eq!(keys("x + e"), vec!["x", "e"]);
        assert_eq!(keys("e || 'a'"), vec!["e"]);
        assert_eq!(keys("\"e\" + 1"), vec!["e", "1"]);
        assert!(references_ref("e + 1", "e", None));
        // The literal's own content is still skipped, and the reference after
        // it still scans.
        assert_eq!(keys("e'a' + cost"), vec!["cost"]);
    }

    #[test]
    fn a_suppressed_introducer_is_not_a_function_head_either() {
        // The two scans partition the chains; a suppressed chain belongs to
        // neither (it is part of a literal token).
        assert!(scan_function_heads("count_if(x = e'\\'')")
            .iter()
            .all(|h| h.key() != "e"));
    }

    // ----- COR-3: a cast's TARGET TYPE is not a reference -----
    //
    // (semolina review 2026-10-16) The type name after `::`, or after `AS`
    // inside `CAST(…)` / `TRY_CAST(…)`, is a type, not a name. Emitting it as a
    // reference let a member named like a type (`blob`, `date`, `time`) be
    // substituted OVER the type: a where_clause `payload = '\x41'::BLOB` on a
    // view with a fact named `blob` became `'\x41'::((blob))` — a query-time
    // syntax error from a valid predicate — and the type was reported as a
    // phantom dependency.

    fn blob_member() -> HashMap<String, &'static str> {
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("blob".to_string(), "(o.payload)");
        m
    }

    #[test]
    fn the_type_after_a_cast_operator_is_not_a_reference() {
        assert_eq!(keys("x::BLOB"), vec!["x"]);
        assert_eq!(keys("x :: blob"), vec!["x"], "whitespace around `::`");
        assert!(!references_ref("'\\x41'::BLOB", "blob", None));
    }

    #[test]
    fn a_quoted_or_qualified_cast_type_is_not_a_reference() {
        // DuckDB accepts both (`'a'::"blob"`, `1::main.integer` — verified).
        assert_eq!(keys("x::\"blob\""), vec!["x"]);
        assert_eq!(keys("x::main.blob"), vec!["x"]);
    }

    #[test]
    fn inlining_does_not_corrupt_a_cast_operator_type() {
        let expr = "blob = '\\x41'::BLOB";
        assert_eq!(
            inline_references(expr, &blob_member()),
            "(o.payload) = '\\x41'::BLOB"
        );
    }

    #[test]
    fn the_type_in_cast_as_is_not_a_reference() {
        assert_eq!(keys("CAST(x AS blob)"), vec!["x"]);
        assert_eq!(keys("cast (x as Blob)"), vec!["x"], "case and spacing");
        assert!(!references_ref("CAST('A' AS blob)", "blob", None));
    }

    #[test]
    fn the_type_in_try_cast_as_is_not_a_reference() {
        assert_eq!(keys("TRY_CAST(x AS blob)"), vec!["x"]);
    }

    #[test]
    fn inlining_does_not_corrupt_a_cast_as_type() {
        let expr = "blob = CAST('A' AS BLOB)";
        assert_eq!(
            inline_references(expr, &blob_member()),
            "(o.payload) = CAST('A' AS BLOB)"
        );
    }

    #[test]
    fn a_multi_word_cast_type_is_not_a_reference() {
        // DuckDB's multi-word type names: a member named `time` or `zone`
        // must not land inside `TIMESTAMP WITH TIME ZONE`.
        assert_eq!(keys("x::TIMESTAMP WITH TIME ZONE"), vec!["x"]);
        assert_eq!(keys("x::TIME WITHOUT TIME ZONE"), vec!["x"]);
        assert_eq!(keys("x::DOUBLE PRECISION"), vec!["x"]);
        assert_eq!(keys("x::CHARACTER VARYING"), vec!["x"]);
        assert_eq!(keys("CAST(x AS TIMESTAMP WITH TIME ZONE)"), vec!["x"]);
    }

    #[test]
    fn a_parameterized_or_array_cast_type_is_not_a_reference() {
        // The type's own parameter list and array suffixes belong to the type:
        // `STRUCT(blob BLOB)` names a field and its type, not two members.
        assert_eq!(keys("x::STRUCT(blob BLOB)"), vec!["x"]);
        assert_eq!(keys("x::DECIMAL(10, 2)"), vec!["x"]);
        assert_eq!(keys("x::VARCHAR[]"), vec!["x"]);
        assert_eq!(keys("x::INT[3] [ ]"), vec!["x"]);
        assert_eq!(keys("x::INT ARRAY"), vec!["x"]);
        assert_eq!(keys("CAST(x AS MAP(VARCHAR, blob))"), vec!["x"]);
        // A parameterized type is not a function call either.
        assert!(scan_function_heads("x::DECIMAL(10, 2)").is_empty());
    }

    #[test]
    fn references_around_a_cast_still_scan() {
        // Controls: only the TYPE is suppressed, never its operand or what
        // follows the cast.
        assert_eq!(keys("blob::VARCHAR"), vec!["blob"]);
        assert_eq!(keys("x::INT + blob"), vec!["x", "blob"]);
        assert_eq!(keys("CAST(blob AS INT)"), vec!["blob"]);
        assert_eq!(keys("CAST(a AS INT) + b"), vec!["a", "b"]);
        assert_eq!(keys("CAST(CAST(a AS INT) AS BIGINT) * b"), vec!["a", "b"]);
        // `TIME` alone is a type; a following reference is not a type keyword.
        assert_eq!(keys("x::TIME + zone"), vec!["x", "zone"]);
        assert_eq!(keys("x::TIMESTAMP AND time"), vec!["x", "and", "time"]);
        // `AS` outside a cast is not a cast's type position.
        assert_eq!(keys("f(a AS b)"), vec!["a", "as", "b"]);
        assert_eq!(keys("(CAST(a AS INT), b AS c)"), vec!["a", "b", "as", "c"]);
        // A slice is a single colon, not a cast.
        assert_eq!(keys("l[a:b]"), vec!["l", "a", "b"]);
    }

    // The same defect at the other keyword positions DuckDB's expression
    // grammar has, where a member named like the keyword (`year`, `day`,
    // `time`) is common: the field of `EXTRACT(field FROM …)`, the unit of an
    // `INTERVAL` literal, and `AT TIME ZONE`.

    #[test]
    fn the_field_of_extract_is_not_a_reference() {
        assert_eq!(keys("EXTRACT(year FROM ts)"), vec!["from", "ts"]);
        assert_eq!(keys("extract ( Year from ts )"), vec!["from", "ts"]);
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("year".to_string(), "(EXTRACT(year FROM o.ts))");
        assert_eq!(
            inline_references("year = EXTRACT(year FROM ts)", &m),
            "(EXTRACT(year FROM o.ts)) = EXTRACT(year FROM ts)"
        );
    }

    #[test]
    fn the_unit_of_an_interval_is_not_a_reference() {
        // A numeric operand scans as a chain, as numbers do everywhere here.
        assert_eq!(keys("ts + INTERVAL 1 DAY"), vec!["ts", "1"]);
        assert_eq!(keys("ts + INTERVAL '1' day"), vec!["ts"]);
        assert_eq!(keys("ts + INTERVAL 1.5 DAYS"), vec!["ts", "1.5"]);
        // A parenthesized operand is an expression: its references still scan.
        assert_eq!(keys("ts + INTERVAL (n) DAY"), vec!["ts", "n"]);
        let mut m: HashMap<String, &str> = HashMap::new();
        m.insert("day".to_string(), "(o.ts::DATE)");
        assert_eq!(
            inline_references("day < INTERVAL 1 DAY", &m),
            "(o.ts::DATE) < INTERVAL 1 DAY"
        );
    }

    #[test]
    fn at_time_zone_is_not_a_reference() {
        assert_eq!(keys("ts AT TIME ZONE 'UTC'"), vec!["ts"]);
        assert_eq!(keys("ts at time zone tz"), vec!["ts", "tz"]);
    }

    #[test]
    fn references_around_keyword_positions_still_scan() {
        // Controls: only the keyword position is suppressed.
        assert_eq!(keys("EXTRACT(year FROM year)"), vec!["from", "year"]);
        assert_eq!(keys("extract_year(day)"), vec!["day"]);
        assert_eq!(keys("INTERVAL 1 DAY + day"), vec!["1", "day"]);
        assert_eq!(keys("interval_col + day"), vec!["interval_col", "day"]);
        assert_eq!(keys("ts + time"), vec!["ts", "time"]);
        assert_eq!(keys("at + time"), vec!["at", "time"]);
    }

    // ----- generative proptests -----

    /// A quoted-identifier part whose inner content is arbitrary (including
    /// characters that would otherwise be delimiters), `""`-escaped.
    fn arb_expr_atom() -> impl Strategy<Value = String> {
        prop_oneof![
            // bare identifiers (incl. non-ASCII)
            "[a-zA-Z_][a-zA-Z0-9_]{0,6}",
            "[a-zàéΩ東][a-zàéΩ東_]{0,4}",
            // qualified references
            "[a-z]{1,4}\\.[a-z]{1,4}",
            // quoted identifiers with hostile inner content
            r#""[a-zA-Z0-9 .,()]{0,6}""#,
            // single-quoted string literals with identifier-looking content
            "'[a-zA-Z0-9 .]{0,8}'",
            // PARSE-12: a chain ABUTTING a quote introduces a literal — escape
            // strings (`e'…'`, `E'…'`, whose backslash rules differ) and typed
            // literals (`DATE'…'`, and the qualified / quoted type names DuckDB
            // also accepts). Generated, not just pinned by fixed examples, so
            // the tiling / disjointness / FIND-INLINE-agree properties below
            // exercise the shape too.
            "[eE]'[a-zA-Z0-9 ]{0,4}'",
            Just("e'a\\'b'".to_string()),
            "[a-z]{1,4}'[a-zA-Z0-9 :.-]{0,8}'",
            Just("main.date'2020-01-01'".to_string()),
            Just("\"date\"'2020-01-01'".to_string()),
            // COR-3: a cast's target type is not a reference — after `::`
            // (bare, quoted, multi-word, parameterized) and after `AS` inside
            // `CAST(…)`. Generated so the tiling / FIND-INLINE-agree properties
            // see the shape, not only the fixed examples.
            "::[a-zA-Z]{1,5}",
            Just(" :: \"blob\"".to_string()),
            Just("::TIMESTAMP WITH TIME ZONE".to_string()),
            Just("::STRUCT(blob BLOB)[]".to_string()),
            "CAST\\(x AS [a-zA-Z]{1,5}\\)",
            // COR-3: the other keyword positions — an EXTRACT field and an
            // INTERVAL unit (offsets recorded ahead of the walk), AT TIME ZONE,
            // and a multi-word typed literal's type. The unit word is random,
            // so it is sometimes a real unit and sometimes an ordinary name.
            "EXTRACT\\([a-z]{1,4} FROM ",
            "INTERVAL [0-9] [a-zA-Z]{1,4}",
            "INTERVAL \\([a-z]{1,3}\\) [a-zA-Z]{1,4}",
            "INTERVAL ?'[0-9]' [a-zA-Z]{1,4}",
            Just(" AT TIME ZONE ".to_string()),
            "[a-zA-Z]{1,5} (WITH|WITHOUT) TIME ZONE ?'[0-9 :-]{0,8}'",
            "[a-zA-Z]{1,5} (PRECISION|VARYING) '[0-9.]{0,4}'",
            // dollar-quoted strings
            Just("$$a.b revenue$$".to_string()),
            // operators / punctuation / whitespace
            prop::sample::select(vec![
                " + ".to_string(),
                " - ".to_string(),
                " * ".to_string(),
                ", ".to_string(),
                "(".to_string(),
                ")".to_string(),
                " ".to_string(),
            ]),
        ]
    }

    fn arb_expr() -> impl Strategy<Value = String> {
        prop::collection::vec(arb_expr_atom(), 0..12).prop_map(|atoms| atoms.concat())
    }

    proptest! {
        /// The reference spans are non-overlapping, in order, in bounds, and on
        /// char boundaries — they tile a subset of the input.
        #[test]
        fn refs_are_ordered_non_overlapping_and_on_char_boundaries(expr in arb_expr()) {
            let mut last_end = 0;
            for r in scan_references(&expr) {
                prop_assert!(r.start >= last_end, "overlap/disorder in {expr:?}");
                prop_assert!(r.end <= expr.len());
                prop_assert!(expr.is_char_boundary(r.start));
                prop_assert!(expr.is_char_boundary(r.end));
                prop_assert_eq!(&expr[r.start..r.end], r.raw);
                last_end = r.end;
            }
        }

        /// Scanning any input is total: it never panics, and `key()` on every
        /// ref is likewise total. (Key *stability* is a property of
        /// `normalize_ident_part`, tested in `ident`, not here — a quoted
        /// whitespace-only identifier is a deliberate non-goal.)
        #[test]
        fn scanning_is_total(expr in arb_expr()) {
            for r in scan_references(&expr) {
                let _ = r.key();
            }
        }

        /// A key that appears ONLY inside a string/dollar literal is never a
        /// found reference and is never substituted.
        #[test]
        fn literal_content_is_never_a_reference(inner in "[a-z]{1,6}") {
            let lit = format!("'{inner}'");
            prop_assert!(!references_ref(&lit, &inner, None));
            let dollar = format!("$${inner}$$");
            prop_assert!(!references_ref(&dollar, &inner, None));
            let mut m: HashMap<String, &str> = HashMap::new();
            m.insert(inner.clone(), "REPL");
            prop_assert_eq!(inline_references(&lit, &m), lit.clone());
            prop_assert_eq!(inline_references(&dollar, &m), dollar);
        }

        /// PARSE-12: an identifier chain IMMEDIATELY followed by `'` is the
        /// introducer of a literal, so it is neither found as a reference nor
        /// substituted — for any prefix, not just the `e` / `DATE` examples.
        #[test]
        fn a_chain_abutting_a_quote_is_never_a_reference(
            prefix in "[a-zA-Z_][a-zA-Z0-9_]{0,4}",
            inner in "[a-zA-Z0-9 ]{0,6}",
        ) {
            // The left-hand reference is a QUOTED name containing a space, so
            // no generated `prefix` can collide with it.
            let expr = format!("\"lhs col\" = {prefix}'{inner}'");
            prop_assert!(!references_ref(&expr, &prefix, None), "expr={expr:?}");
            let mut m: HashMap<String, &str> = HashMap::new();
            let key = crate::ident::normalize_ident_part(&prefix);
            m.insert(key, "<R>");
            prop_assert_eq!(inline_references(&expr, &m), expr.clone());
            // Anti-vacuity: the scan really ran — the ordinary reference before
            // the literal is still found (so "nothing scans" cannot pass this).
            prop_assert!(references_ref(&expr, "lhs col", None), "expr={expr:?}");
        }

        /// COR-3: the target type of a cast — after `::`, or after `AS` in
        /// `CAST` / `TRY_CAST` — is never found as a reference nor
        /// substituted, whatever the type is named.
        #[test]
        fn a_cast_type_is_never_a_reference(
            ty in "[a-zA-Z_][a-zA-Z0-9_]{0,4}",
            func in prop::sample::select(vec!["CAST", "try_cast"]),
        ) {
            let key = crate::ident::normalize_ident_part(&ty);
            let mut m: HashMap<String, &str> = HashMap::new();
            m.insert(key, "<R>");
            for expr in [
                format!("\"lhs col\"::{ty}"),
                format!("\"lhs col\" :: \"{ty}\""),
                format!("{func}(\"lhs col\" AS {ty})"),
            ] {
                prop_assert!(!references_ref(&expr, &ty, None), "expr={expr:?}");
                prop_assert_eq!(inline_references(&expr, &m), expr.clone());
                // Anti-vacuity: the cast's operand is still found.
                prop_assert!(references_ref(&expr, "lhs col", None), "expr={expr:?}");
            }
        }

        /// COR-3: a word in an EXTRACT field, an INTERVAL unit, or any word of
        /// a multi-word typed literal's type is never found as a reference nor
        /// substituted, while the references around it still are.
        #[test]
        fn a_keyword_position_word_is_never_a_reference(
            word in "[a-zA-Z_][a-zA-Z0-9_]{0,4}",
            unit in prop::sample::select(INTERVAL_UNITS.to_vec()),
            tail in prop::sample::select(vec!["WITH TIME ZONE", "WITHOUT TIME ZONE", "PRECISION", "VARYING"]),
            gap in prop::sample::select(vec!["", " ", "\n  "]),
        ) {
            let cases = [
                (format!("EXTRACT({word} FROM \"lhs col\")"), word.clone()),
                (format!("\"lhs col\" + INTERVAL{gap}'1' {unit}"), unit.to_string()),
                (format!("\"lhs col\" + INTERVAL 1 {unit}"), unit.to_string()),
                (format!("\"lhs col\" + INTERVAL (\"lhs col\") {unit}"), unit.to_string()),
                (format!("\"lhs col\" = {word} {tail}{gap}'1'"), word.clone()),
                (
                    format!("\"lhs col\" = {word} {tail}{gap}'1'"),
                    tail.split(' ').next().unwrap_or_default().to_string(),
                ),
            ];
            for (expr, name) in cases {
                let key = crate::ident::normalize_ident_part(&name);
                let mut m: HashMap<String, &str> = HashMap::new();
                m.insert(key, "<R>");
                prop_assert!(!references_ref(&expr, &name, None), "expr={expr:?} name={name:?}");
                prop_assert_eq!(inline_references(&expr, &m), expr.clone());
                // Anti-vacuity: the reference beside the keyword is still found.
                prop_assert!(references_ref(&expr, "lhs col", None), "expr={expr:?}");
            }
        }

        /// FIND and INLINE agree: a name is reported as referenced iff inlining
        /// it changes the expression.
        #[test]
        fn find_and_inline_agree(expr in arb_expr(), name in "[a-z]{1,5}") {
            let found = references_ref(&expr, &name, None);
            let mut m: HashMap<String, &str> = HashMap::new();
            m.insert(name.to_ascii_lowercase(), "<R>");
            let changed = inline_references(&expr, &m) != expr;
            // A bare reference is found iff a (bare-key) inline changes the text.
            prop_assert_eq!(found, changed, "expr={:?} name={:?}", expr, name);
        }

        /// A bare needle never matches the tail of a contiguous qualified
        /// reference (E-3).
        #[test]
        fn bare_needle_never_matches_qualified_tail(
            head in "[a-z]{1,4}",
            tail in "[a-z]{1,5}",
        ) {
            let expr = format!("{head}.{tail}");
            // `tail` is not referenced bare (it is a foreign qualified tail),
            // and it is not referenced when qualified by a source table other
            // than `head`.
            prop_assert!(!references_ref(&expr, &tail, None));
            let mut m: HashMap<String, &str> = HashMap::new();
            m.insert(tail.clone(), "<R>");
            prop_assert_eq!(inline_references(&expr, &m), expr);
        }

        /// References and function-call heads partition all identifier chains:
        /// their spans are disjoint, and no span appears in both scans.
        #[test]
        fn references_and_heads_are_disjoint(expr in arb_expr()) {
            let refs: std::collections::HashSet<(usize, usize)> =
                scan_references(&expr).iter().map(|r| (r.start, r.end)).collect();
            let heads: std::collections::HashSet<(usize, usize)> =
                scan_function_heads(&expr).iter().map(|r| (r.start, r.end)).collect();
            prop_assert!(refs.is_disjoint(&heads), "overlap in {expr:?}");
        }

        /// `scan_function_heads` is total and yields spans that tile a subset of
        /// the input on char boundaries (same invariant as `scan_references`).
        #[test]
        fn function_heads_are_ordered_and_in_bounds(expr in arb_expr()) {
            let mut last_end = 0;
            for r in scan_function_heads(&expr) {
                prop_assert!(r.start >= last_end);
                prop_assert!(r.end <= expr.len());
                prop_assert!(expr.is_char_boundary(r.start));
                prop_assert!(expr.is_char_boundary(r.end));
                prop_assert_eq!(&expr[r.start..r.end], r.raw);
                last_end = r.end;
            }
        }

        /// Rewriting a qualifier that matches no chain head leaves the
        /// expression untouched, and rewriting is always total (never panics).
        #[test]
        fn rewrite_qualifier_is_total_and_identity_on_no_match(
            expr in arb_expr(),
            alias in "[a-z]{1,5}",
        ) {
            let out = rewrite_qualifier(&expr, &alias, "<Q>");
            // If no chain's first part equals `alias`, the output is unchanged.
            let matches = scan_references(&expr)
                .iter()
                .any(|r| crate::ident::normalize_ident_part(&r.raw[..first_part_len(r.raw)])
                    == crate::ident::normalize_ident_part(&alias));
            if !matches {
                prop_assert_eq!(out, expr);
            }
        }

        /// For a contiguous `head.tail`, rewriting the qualifier `head` rewrites
        /// exactly the head part and preserves the tail (E-3: the foreign tail
        /// `tail` is never itself treated as the qualifier).
        #[test]
        fn rewrite_qualifier_rewrites_head_preserves_tail(
            head in "[a-z]{1,4}",
            tail in "[a-z]{1,5}",
        ) {
            let expr = format!("{head}.{tail}");
            prop_assert_eq!(
                rewrite_qualifier(&expr, &head, "<Q>"),
                format!("<Q>.{tail}")
            );
            // Rewriting by the tail name changes nothing (it is not a qualifier)
            // — as long as the tail differs from the head part.
            prop_assume!(head != tail);
            prop_assert_eq!(rewrite_qualifier(&expr, &tail, "<Q>"), expr);
        }
    }
}
