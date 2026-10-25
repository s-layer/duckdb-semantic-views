//! SQL identifier parsing and normalisation helpers.
//!
//! This leaf module owns the grammar for *dot-qualified, double-quoted* SQL
//! identifiers. It is the inverse of [`crate::expand::resolution::quote_ident`]:
//! where `quote_ident` emits `"foo""bar"` from the bare string `foo"bar`,
//! [`parse_qualified_identifier`] consumes such input and returns the bare
//! parts.
//!
//! ## Grammar
//!
//! ```text
//! identifier      := part ("." part)*
//! part            := bare | quoted
//! bare            := [^."]+
//! quoted          := '"' ( [^"] | '""' )* '"'
//! ```
//!
//! Inside a quoted part, `""` is an escape for a literal `"` (SQL standard).
//! Any other byte — including `.`, whitespace, `;`, `(` — is part of the
//! identifier, not a separator.
//!
//! ## Round-trip property
//!
//! For any sequence of legal identifier parts `v`,
//!
//! ```text
//! parse_qualified_identifier(v.iter().map(quote_ident).join(".")) == Ok(v)
//! ```
//!
//! This invariant is exercised by the proptest in [`tests::proptests`].
//!
//! ## Error model
//!
//! Errors are returned as `String` to match the existing convention in
//! `parse.rs::extract_quoted_string` and `body_parser.rs`. No new error enum
//! is introduced for this leaf helper.

/// Parse a dot-qualified SQL identifier into its *unquoted* parts.
///
/// Honours `"..."` quoting (with `""` escape) and treats `.` inside a quoted
/// region as part of the identifier rather than a part separator.
///
/// Returns `Err` for empty input, unterminated quoted parts, empty parts
/// between dots (`a..b`), leading dots, or trailing garbage after a closing
/// quote.
///
/// Case is preserved for every part; callers that need fold-to-lowercase
/// semantics for bare parts use [`parse_qualified_identifier_with_quoting`]
/// to learn which parts were quoted.
///
/// # Examples
///
/// ```ignore
/// use semantic_views::ident::parse_qualified_identifier;
/// assert_eq!(parse_qualified_identifier("orders_sv").unwrap(), vec!["orders_sv"]);
/// assert_eq!(
///     parse_qualified_identifier("\"db\".\"sch\".\"v\"").unwrap(),
///     vec!["db", "sch", "v"],
/// );
/// assert_eq!(parse_qualified_identifier("\"with\"\"q\"").unwrap(), vec![r#"with"q"#]);
/// ```
pub fn parse_qualified_identifier(input: &str) -> Result<Vec<String>, String> {
    Ok(parse_qualified_identifier_with_quoting(input)?
        .into_iter()
        .map(|(part, _)| part)
        .collect())
}

/// [`parse_qualified_identifier`] variant that also reports, per part,
/// whether it was written `"quoted"` in the input. The quotedness flag is
/// what decides case-folding at [`normalize_view_name`].
pub fn parse_qualified_identifier_with_quoting(input: &str) -> Result<Vec<(String, bool)>, String> {
    if input.is_empty() {
        return Err("empty identifier".to_string());
    }
    let bytes = input.as_bytes();
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut pos: usize = 0;

    loop {
        // ExpectPart: at start, or just after a `.`.
        if pos >= bytes.len() {
            return Err("empty identifier part (trailing '.')".to_string());
        }
        if bytes[pos] == b'.' {
            return Err(if parts.is_empty() {
                "identifier may not start with '.'".to_string()
            } else {
                "empty identifier part between '.' separators".to_string()
            });
        }

        if bytes[pos] == b'"' {
            // Quoted part — scan to matching closing quote, honouring "" escape.
            // Byte-scan for the ASCII quote (a UTF-8 continuation byte can
            // never equal 0x22) but accumulate content by slicing the
            // original `&str` between quote positions: the previous
            // `bytes[pos] as char` push Latin-1-ized every non-ASCII
            // codepoint (`"café"` stored as `cafÃ©` — PA-2, code-review
            // 2026-07-02).
            pos += 1;
            let mut buf = String::new();
            let mut seg_start = pos;
            loop {
                if pos >= bytes.len() {
                    return Err("unterminated quoted identifier".to_string());
                }
                if bytes[pos] == b'"' {
                    buf.push_str(&input[seg_start..pos]);
                    // Look ahead for escape `""`.
                    if pos + 1 < bytes.len() && bytes[pos + 1] == b'"' {
                        buf.push('"');
                        pos += 2;
                        seg_start = pos;
                        continue;
                    }
                    // Closing quote consumed.
                    pos += 1;
                    break;
                }
                pos += 1;
            }
            if buf.is_empty() {
                return Err("empty quoted identifier (\"\")".to_string());
            }
            parts.push((buf, true));
        } else {
            // Bare part — read until '.', '"', or end-of-input.
            let start = pos;
            while pos < bytes.len() && bytes[pos] != b'.' && bytes[pos] != b'"' {
                pos += 1;
            }
            if pos == start {
                return Err("empty bare identifier part".to_string());
            }
            // A bare part cannot abut a `"` (e.g. `foo"bar"`).
            if pos < bytes.len() && bytes[pos] == b'"' {
                return Err(
                    "unexpected '\"' after bare identifier part (mixed bare/quoted)".to_string(),
                );
            }
            // Safe: bare parts contain only single-byte ASCII excluding '.', '"'.
            // We accept any non-`.`/`"` byte verbatim here; non-ASCII bytes are
            // copied through as UTF-8 because we sliced the original &str.
            parts.push((input[start..pos].to_string(), false));
        }

        // AfterPart: must be '.' (more) or end-of-input (done).
        if pos >= bytes.len() {
            return Ok(parts);
        }
        if bytes[pos] == b'.' {
            pos += 1;
            continue;
        }
        return Err(format!(
            "trailing garbage after identifier part at byte offset {pos}"
        ));
    }
}

/// Convenience: return the normalised *last* part of a dot-qualified
/// identifier. This is the lookup key stored in
/// `semantic_layer._definitions(name)`.
///
/// Case normalisation (PA-8, code-review 2026-07-02; case rule revised
/// 2026-07-12): the name is stripped of any surrounding quotes and folded to
/// ASCII lowercase, whether it was written quoted or not. This matches
/// **`DuckDB`'s** identifier semantics — identifiers are case-insensitive
/// *including* double-quoted ones (`"Foo"`, `foo`, and `"FOO"` all denote the
/// same object) — which is the convention the rest of this extension follows
/// (dimension/metric/fact and table-alias matching are all case-insensitive).
/// It applies uniformly because every view-name consumer — DDL capture sites,
/// guard/DML emission, and the `semantic_view()` / `explain_semantic_view()`
/// lookup arguments — resolves names through this function.
///
/// (The earlier revision folded unquoted names but *preserved* quoted ones,
/// i.e. Snowflake's rule where a quoted identifier is case-sensitive. That
/// diverged from `DuckDB` and from the rest of the project, so quoted names
/// now fold too.)
///
/// Migration note: lookups fold the *requested* name to lowercase and match
/// the stored `semantic_layer._definitions(name)` column exactly, so a view is
/// only reachable if its **stored** name is already lowercase. Unquoted DDL
/// always stored a lowercase name (both before and after this change), so those
/// views are unaffected. Only a definition created before v0.11 via a *quoted*
/// mixed-case identifier (e.g. `CREATE SEMANTIC VIEW "Sales"`) kept its original
/// casing in the catalog; under the folding rule no spelling — `sales`,
/// `Sales`, or `"Sales"` — matches the stored `Sales`, so it is no longer
/// reachable. Drop and recreate it, or rename the catalog row to lowercase, to
/// make it reachable again.
///
/// # Examples
///
/// ```ignore
/// use semantic_views::ident::normalize_view_name;
/// assert_eq!(normalize_view_name("orders_sv").unwrap(), "orders_sv");
/// assert_eq!(normalize_view_name("Orders_SV").unwrap(), "orders_sv");
/// assert_eq!(normalize_view_name("\"Orders_SV\"").unwrap(), "orders_sv");
/// assert_eq!(
///     normalize_view_name("\"memory\".\"main\".\"orders_sv\"").unwrap(),
///     "orders_sv",
/// );
/// ```
pub fn normalize_view_name(input: &str) -> Result<String, String> {
    let parts = parse_qualified_identifier_with_quoting(input)?;
    parts
        .into_iter()
        .next_back()
        .map(|(part, _quoted)| part.to_ascii_lowercase())
        .ok_or_else(|| "empty identifier".to_string())
}

/// A semantic-view reference split into its optional qualifier and its name.
///
/// [`normalize_view_name`] answers "what is this view called"; `ViewRef`
/// answers "*which* view is this" — the difference matters once views are
/// scoped to a schema, because `analytics.v` and `staging.v` are two views.
/// Every part is folded to ASCII lowercase under the same `DuckDB`
/// case-insensitivity rule `normalize_view_name` documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRef {
    /// The `db` of `db.schema.name`, when written.
    pub database: Option<String>,
    /// The `schema` of `schema.name` or `db.schema.name`, when written.
    /// `None` means "unqualified" — the caller decides what that resolves to
    /// (`current_schema()` for CREATE, an existing row for DROP/ALTER/read).
    pub schema: Option<String>,
    /// The bare view name — always equal to [`normalize_view_name`] of the
    /// same input (pinned by `parse_view_ref_tests::name_agrees_with_normalize_view_name`).
    pub name: String,
}

/// [`parse_view_ref`] with a total fallback: input that is not a well-formed
/// identifier becomes an unqualified reference carrying the text verbatim.
///
/// The lookup then misses and reports the canonical
/// `semantic view '<name>' does not exist`, which is friendlier than an
/// identifier-grammar error for read paths (`GET_DDL`,
/// `READ_YAML_FROM_SEMANTIC_VIEW`) whose argument is a plain string rather
/// than a parsed SQL identifier.
#[must_use]
pub fn parse_view_ref_lenient(input: &str) -> ViewRef {
    parse_view_ref(input).unwrap_or_else(|_| ViewRef {
        database: None,
        schema: None,
        name: input.to_string(),
    })
}

impl std::fmt::Display for ViewRef {
    /// Render the reference the way it was written — `schema.name` when
    /// qualified, bare otherwise — so an error about a qualified reference
    /// names the schema the caller asked for rather than an unqualified name
    /// that may exist elsewhere.
    ///
    /// Each part is quoted only when a bare emission would not round-trip, so
    /// the rendering re-parses to this same reference. Without that, a part
    /// holding a dot — legal when quoted, as in `ATTACH ':memory:' AS "a.b"` —
    /// prints as `a.b.main.v`, which reads as the four-part reference
    /// [`parse_view_ref`] would have *rejected*. `parse_view_ref` has already
    /// folded every part to lowercase, which is exactly
    /// [`quote_ident_if_needed`]'s bare-safe alphabet, so ordinary names are
    /// untouched.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::expand::quote_ident_if_needed;
        if let Some(db) = &self.database {
            write!(f, "{}.", quote_ident_if_needed(db))?;
        }
        if let Some(schema) = &self.schema {
            write!(f, "{}.", quote_ident_if_needed(schema))?;
        }
        f.write_str(&quote_ident_if_needed(&self.name))
    }
}

/// The keywords `DuckDB` refuses as a bare name, lowercase and sorted. Its
/// grammar takes a name as a `ColId` (an unreserved or column-name keyword, or
/// an identifier that is not a reserved keyword), so these are the reserved
/// keywords and the type and function-name keywords (`left`, `join`, `semi`),
/// less the six of those that are also unreserved or column-name keywords
/// (`columns`, `generated`, `map`, `struct`, `try_cast`, `tuple`). Checked
/// against the engine's `duckdb_keywords()` in `test/sql/grammar_create.test`.
const KEYWORDS_REFUSED_AS_NAMES: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "anti",
    "any",
    "array",
    "as",
    "asc",
    "asof",
    "asymmetric",
    "at",
    "authorization",
    "binary",
    "both",
    "by",
    "case",
    "cast",
    "check",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "default",
    "deferrable",
    "desc",
    "describe",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "glob",
    "group",
    "having",
    "ilike",
    "in",
    "initially",
    "inner",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "lambda",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "overlaps",
    "pivot",
    "pivot_longer",
    "pivot_wider",
    "placing",
    "positional",
    "primary",
    "qualify",
    "references",
    "returning",
    "right",
    "select",
    "semi",
    "show",
    "similar",
    "some",
    "summarize",
    "symmetric",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "unpack",
    "unpivot",
    "using",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
];

/// Whether `DuckDB` refuses `word` as a bare name, so it must be quoted.
#[must_use]
pub fn is_refused_as_name(word: &str) -> bool {
    KEYWORDS_REFUSED_AS_NAMES
        .binary_search(&word.to_ascii_lowercase().as_str())
        .is_ok()
}

/// Parse a (possibly qualified) semantic-view reference.
///
/// The qualifier-preserving sibling of [`normalize_view_name`], which returns
/// only the last part. Accepts one, two, or three parts; four or more is an
/// error rather than a silent truncation, so `a.b.c.d` cannot quietly resolve
/// to `d`.
///
/// # Errors
///
/// Propagates [`parse_qualified_identifier_with_quoting`]'s errors (empty
/// input, unterminated quote, empty part between dots), and rejects references
/// of more than three parts.
///
/// # Examples
///
/// ```ignore
/// use semantic_views::ident::parse_view_ref;
/// let r = parse_view_ref("analytics.Orders_SV").unwrap();
/// assert_eq!(r.schema.as_deref(), Some("analytics"));
/// assert_eq!(r.name, "orders_sv");
/// ```
pub fn parse_view_ref(input: &str) -> Result<ViewRef, String> {
    let mut parts: Vec<String> = parse_qualified_identifier_with_quoting(input)?
        .into_iter()
        .map(|(part, _quoted)| part.to_ascii_lowercase())
        .collect();
    if parts.len() > 3 {
        return Err(format!(
            "too many identifier parts in '{input}' (expected at most \
             database.schema.name)"
        ));
    }
    // Pop from the end so the slots fill right-to-left: name, then schema,
    // then database. `parse_qualified_identifier_with_quoting` never returns
    // an empty Vec on Ok, so the first pop always succeeds.
    let name = parts.pop().ok_or_else(|| "empty identifier".to_string())?;
    let schema = parts.pop();
    let database = parts.pop();
    Ok(ViewRef {
        database,
        schema,
        name,
    })
}

/// Locate the byte offset of the FIRST delimiter that is NOT inside a quoted
/// region. Delimiters are ASCII whitespace, `;`, and (when `allow_paren` is
/// true) `(`.
///
/// Used at the DDL capture sites in `src/parse.rs` to peel a (possibly
/// quoted) identifier off the front of a clause without truncating mid-quote.
///
/// Inside `"..."` (with `""` escape) every byte is part of the identifier:
/// whitespace, `.`, `;`, `(` are all inert.
///
/// If no delimiter is found, returns `input.len()`. An unterminated quote
/// also returns `input.len()` — the caller's parser surfaces the structural
/// error.
///
/// # Examples
///
/// ```ignore
/// use semantic_views::ident::find_identifier_end;
/// assert_eq!(find_identifier_end("orders_sv AS ...", true), 9);
/// assert_eq!(find_identifier_end("\"my table\" AS ...", true), 10);
/// assert_eq!(find_identifier_end("v(foo)", true), 1);
/// assert_eq!(find_identifier_end("v(foo)", false), 6);
/// ```
#[must_use]
pub fn find_identifier_end(input: &str, allow_paren: bool) -> usize {
    let bytes = input.as_bytes();
    let mut pos = 0;
    let mut in_quotes = false;

    while pos < bytes.len() {
        let b = bytes[pos];
        if in_quotes {
            if b == b'"' {
                if pos + 1 < bytes.len() && bytes[pos + 1] == b'"' {
                    pos += 2; // doubled-quote escape — stay in quotes
                    continue;
                }
                in_quotes = false;
                pos += 1;
                continue;
            }
            pos += 1;
            continue;
        }
        // Outside a quoted region.
        if b == b'"' {
            in_quotes = true;
            pos += 1;
            continue;
        }
        let is_ws = (b as char).is_ascii_whitespace();
        let is_semi = b == b';';
        let is_paren = allow_paren && b == b'(';
        if is_ws || is_semi || is_paren {
            return pos;
        }
        // Advance one byte. UTF-8 continuation bytes are non-ASCII so they
        // never match a delimiter — copying them through unchanged is safe.
        pos += 1;
    }
    // No delimiter found (or scan ran off the end inside a quoted region).
    // Saturate at input.len() — the caller's parser surfaces any structural
    // error (e.g. unterminated quote).
    bytes.len()
}

/// Normalize a (possibly dot-qualified, possibly double-quoted) SQL identifier
/// to its case-folding **match key**, following `DuckDB`'s identifier
/// semantics (the same rule [`normalize_view_name`] uses for view names): each
/// part has its surrounding quotes stripped (and `""` unescaped) and is folded
/// to ASCII lowercase, whether it was quoted or not. So `Region`, `region`,
/// `REGION`, `"Region"`, and `"region"` all share the key `region` and match
/// case-insensitively — `DuckDB` treats even double-quoted identifiers as
/// case-insensitive.
///
/// This is the component-name analogue of the view-name rule: dimension /
/// metric / fact references in a query match case-insensitively regardless of
/// quoting, WITHOUT changing how names are stored (the serde wire format is
/// unaffected — normalization happens only at match time, so `DESCRIBE` /
/// `GET_DDL` still show the name as originally written).
///
/// Total by construction: input that is not a well-formed identifier (an
/// unterminated quote, an empty part) falls back to a lowercase fold of the
/// trimmed raw text, so name matching never panics or errors.
#[must_use]
pub fn normalize_ident_part(raw: &str) -> String {
    let trimmed = raw.trim();
    match parse_qualified_identifier_with_quoting(trimmed) {
        Ok(parts) => parts
            .into_iter()
            .map(|(part, _quoted)| part.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("."),
        Err(_) => trimmed.to_ascii_lowercase(),
    }
}

/// The match key of a (possibly dot-qualified, possibly quoted) identifier
/// that KEEPS its part boundaries: each part with its quotes stripped and
/// folded to ASCII lowercase, as [`normalize_ident_part`] does, but kept as a
/// list rather than joined with `.`.
///
/// [`normalize_ident_part`] is lossy for an identifier slot that may be
/// dot-qualified: the two-part `x.y` and the one-part `"x.y"` (a quoted name
/// containing a dot) both normalize to `x.y`, although `DuckDB` resolves them as
/// different identifiers. Relationship names are such a slot
/// (`model::slot_qualified`), so they are keyed with this instead.
///
/// Total, like [`normalize_ident_part`]: input that does not parse as an
/// identifier keys as [`IdentKey::Unparsed`], which never equals a parsed key.
#[must_use]
pub fn ident_key(raw: &str) -> IdentKey {
    let trimmed = raw.trim();
    match parse_qualified_identifier_with_quoting(trimmed) {
        Ok(parts) => IdentKey::Parts(
            parts
                .into_iter()
                .map(|(part, _quoted)| part.to_ascii_lowercase())
                .collect(),
        ),
        Err(_) => IdentKey::Unparsed(trimmed.to_ascii_lowercase()),
    }
}

/// See [`ident_key`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IdentKey {
    /// The normalized parts of a well-formed identifier, in order.
    Parts(Vec<String>),
    /// The lowercased trimmed text of input that is not a well-formed identifier.
    Unparsed(String),
}

/// True when a stored identifier and a requested identifier denote the same
/// object under `DuckDB`'s case-insensitive identifier rule (see
/// [`normalize_ident_part`]): quoting does not affect matching, and case is
/// ignored on both sides.
///
/// Replaces a bare `eq_ignore_ascii_case` on component names, which did not
/// strip quotes, so a `"quoted"` stored name was only reachable by a reference
/// carrying the identical quote characters. Now `"Region"` is matched by
/// `region`, `REGION`, `"region"`, etc., exactly as `DuckDB` matches a
/// double-quoted table name.
#[must_use]
pub fn ident_matches(stored: &str, requested: &str) -> bool {
    // Fast path (the common case): when neither side is double-quoted, the
    // match is a plain ASCII case-insensitive comparison — allocation-free and
    // byte-for-byte the former `eq_ignore_ascii_case` behaviour. Only a quoted
    // reference on either side needs the strip-quotes-and-fold path.
    if !stored.contains('"') && !requested.contains('"') {
        return stored.eq_ignore_ascii_case(requested);
    }
    normalize_ident_part(stored) == normalize_ident_part(requested)
}

/// Byte offset of the first `.` in `s` that lies OUTSIDE a double-quoted
/// region, or `None`. Used to split a qualified reference `alias.name` at its
/// qualifier dot without splitting inside a quoted part — `"a.b"` has no
/// top-level dot, and `o."a.b"` splits only at the dot after `o`. Honours the
/// `""` escape (a doubled quote stays inside the quoted region).
#[must_use]
pub fn first_unquoted_dot(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut in_quotes = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if in_quotes && i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2; // "" escape — stay inside the quoted region
                    continue;
                }
                in_quotes = !in_quotes;
            }
            b'.' if !in_quotes => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Sorted (the lookup is a binary search) and lowercase. The list itself
    /// is checked against the engine's `duckdb_keywords()` in
    /// `test/sql/grammar_create.test`.
    #[test]
    fn keywords_refused_as_names_are_sorted_lowercase() {
        assert_eq!(KEYWORDS_REFUSED_AS_NAMES.len(), 105);
        assert!(KEYWORDS_REFUSED_AS_NAMES.windows(2).all(|w| w[0] < w[1]));
        assert!(KEYWORDS_REFUSED_AS_NAMES
            .iter()
            .all(|w| w.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')));
        for refused in ["SELECT", "pivot_wider", "left", "semi"] {
            assert!(is_refused_as_name(refused), "{refused}");
        }
        for bare in ["region", "if", "int", "between", "columns", "map"] {
            assert!(!is_refused_as_name(bare), "{bare}");
        }
    }

    #[test]
    fn ident_key_keeps_identifier_parts_apart() {
        // One part containing a dot vs. two parts: `normalize_ident_part`
        // joins both to `x.y`; `ident_key` does not.
        assert_eq!(normalize_ident_part("x.y"), normalize_ident_part("\"x.y\""));
        assert_ne!(ident_key("x.y"), ident_key("\"x.y\""));
        // The same two parts in any quoting and case are one key.
        assert_eq!(ident_key("x.y"), ident_key("\"X\".\"y\""));
        assert_eq!(ident_key("\"x.y\""), ident_key("\"X.Y\""));
        assert_eq!(ident_key("Dep"), ident_key("\"dep\""));
        // Malformed input never equals a parsed key.
        assert_eq!(ident_key("\"abc"), IdentKey::Unparsed("\"abc".to_string()));
        assert_ne!(ident_key("\"abc"), ident_key("\"\"\"abc\""));
    }

    mod parse_qualified_identifier_tests {
        use super::*;

        #[test]
        fn bare_simple() {
            assert_eq!(parse_qualified_identifier("foo").unwrap(), vec!["foo"]);
        }

        #[test]
        fn bare_with_underscore_and_digits() {
            assert_eq!(
                parse_qualified_identifier("orders_sv_42").unwrap(),
                vec!["orders_sv_42"],
            );
        }

        #[test]
        fn fully_quoted_single_part() {
            assert_eq!(
                parse_qualified_identifier("\"orders_sv\"").unwrap(),
                vec!["orders_sv"],
            );
        }

        #[test]
        fn fully_quoted_fqn_three_parts() {
            assert_eq!(
                parse_qualified_identifier("\"db\".\"sch\".\"v\"").unwrap(),
                vec!["db", "sch", "v"],
            );
        }

        #[test]
        fn mixed_quoting_db_quoted_only() {
            assert_eq!(
                parse_qualified_identifier("\"db\".sch.v").unwrap(),
                vec!["db", "sch", "v"],
            );
        }

        #[test]
        fn mixed_quoting_middle_quoted_only() {
            assert_eq!(
                parse_qualified_identifier("db.\"sch\".v").unwrap(),
                vec!["db", "sch", "v"],
            );
        }

        #[test]
        fn embedded_double_quote_escape() {
            assert_eq!(
                parse_qualified_identifier("\"with\"\"q\"").unwrap(),
                vec![r#"with"q"#],
            );
        }

        #[test]
        fn dot_inside_quoted_part() {
            assert_eq!(parse_qualified_identifier("\"a.b\"").unwrap(), vec!["a.b"],);
        }

        #[test]
        fn non_ascii_quoted_part_survives_unmangled() {
            // PA-2 regression: the byte-wise loop stored `"café"` as the
            // Latin-1-ized `cafÃ©`.
            assert_eq!(
                parse_qualified_identifier("\"café\"").unwrap(),
                vec!["café"],
            );
            assert_eq!(
                parse_qualified_identifier("\"東京\".\"wéird name\"").unwrap(),
                vec!["東京", "wéird name"],
            );
            assert_eq!(
                parse_qualified_identifier("\"a☕\"\"b\"").unwrap(),
                vec![r#"a☕"b"#],
            );
        }

        #[test]
        fn whitespace_inside_quoted_part() {
            assert_eq!(
                parse_qualified_identifier("\"my table\"").unwrap(),
                vec!["my table"],
            );
        }

        #[test]
        fn semicolon_inside_quoted_part() {
            assert_eq!(parse_qualified_identifier("\"a;b\"").unwrap(), vec!["a;b"],);
        }

        #[test]
        fn error_empty_input() {
            assert!(parse_qualified_identifier("").is_err());
        }

        #[test]
        fn error_unterminated_quote() {
            let e = parse_qualified_identifier("\"foo").unwrap_err();
            assert!(
                e.contains("unterminated"),
                "expected unterminated error, got: {e}",
            );
        }

        #[test]
        fn error_empty_part_between_dots() {
            assert!(parse_qualified_identifier("a..b").is_err());
        }

        #[test]
        fn error_trailing_garbage_after_quote() {
            // `"foo"bar` — a bare run cannot immediately follow a closing quote.
            assert!(parse_qualified_identifier("\"foo\"bar").is_err());
        }

        #[test]
        fn error_leading_dot() {
            assert!(parse_qualified_identifier(".foo").is_err());
        }

        #[test]
        fn error_trailing_dot() {
            assert!(parse_qualified_identifier("foo.").is_err());
        }

        #[test]
        fn error_empty_quoted() {
            // `""` is rejected — a quoted identifier must have at least one
            // character. (Snowflake also rejects this.)
            assert!(parse_qualified_identifier("\"\"").is_err());
        }

        #[test]
        fn error_bare_then_quoted_no_dot() {
            // `foo"bar"` — bare cannot abut quoted without a `.` separator.
            assert!(parse_qualified_identifier("foo\"bar\"").is_err());
        }
    }

    mod normalize_view_name_tests {
        use super::*;

        #[test]
        fn bare_returns_self() {
            assert_eq!(normalize_view_name("orders_sv").unwrap(), "orders_sv");
        }

        // --- PA-8 (code-review 2026-07-02; revised 2026-07-12): fold to
        // lowercase whether quoted or not (DuckDB case-insensitive) ---

        #[test]
        fn unquoted_mixed_case_folds_to_lowercase() {
            assert_eq!(normalize_view_name("Sales").unwrap(), "sales");
            assert_eq!(normalize_view_name("ORDERS_SV").unwrap(), "orders_sv");
            assert_eq!(normalize_view_name("main.Sales").unwrap(), "sales");
        }

        #[test]
        fn quoted_names_fold_like_unquoted() {
            // DuckDB treats double-quoted identifiers as case-insensitive too,
            // so a quoted name folds to lowercase exactly like an unquoted one
            // (revised 2026-07-12, replacing the Snowflake preserve-quoted rule).
            assert_eq!(normalize_view_name("\"Sales\"").unwrap(), "sales");
            assert_eq!(normalize_view_name("\"ORDERS SV\"").unwrap(), "orders sv");
            assert_eq!(
                normalize_view_name("main.\"Sales\"").unwrap(),
                "sales",
                "only the last part is the name; it folds whether quoted or not"
            );
        }

        #[test]
        fn fold_is_ascii_only() {
            // Non-ASCII in a bare part passes through unfolded — ASCII fold
            // matches DuckDB's identifier semantics and avoids locale
            // surprises.
            assert_eq!(normalize_view_name("Ärger").unwrap(), "Ärger");
        }

        #[test]
        fn quoting_flags_reported_per_part() {
            assert_eq!(
                parse_qualified_identifier_with_quoting("db.\"Sch\".V").unwrap(),
                vec![
                    ("db".to_string(), false),
                    ("Sch".to_string(), true),
                    ("V".to_string(), false),
                ],
            );
        }

        #[test]
        fn quoted_fqn_returns_bare_last_part() {
            assert_eq!(
                normalize_view_name("\"memory\".\"main\".\"orders_sv\"").unwrap(),
                "orders_sv",
            );
        }

        #[test]
        fn mixed_quoting_returns_bare_last_part() {
            assert_eq!(
                normalize_view_name("main.\"orders_sv\"").unwrap(),
                "orders_sv",
            );
        }

        #[test]
        fn embedded_quote_survives_to_last_part() {
            assert_eq!(
                normalize_view_name("\"db\".\"with\"\"q\"").unwrap(),
                r#"with"q"#,
            );
        }

        #[test]
        fn error_propagates_from_parser() {
            assert!(normalize_view_name("").is_err());
            assert!(normalize_view_name("\"foo").is_err());
            assert!(normalize_view_name("a..b").is_err());
        }
    }

    mod parse_view_ref_tests {
        use super::*;

        #[test]
        fn bare_name_has_no_qualifier() {
            let r = parse_view_ref("orders_sv").unwrap();
            assert_eq!(r.name, "orders_sv");
            assert_eq!(r.schema, None);
            assert_eq!(r.database, None);
        }

        #[test]
        fn two_parts_are_schema_and_name() {
            let r = parse_view_ref("analytics.orders_sv").unwrap();
            assert_eq!(r.name, "orders_sv");
            assert_eq!(r.schema.as_deref(), Some("analytics"));
            assert_eq!(r.database, None);
        }

        #[test]
        fn three_parts_are_database_schema_and_name() {
            let r = parse_view_ref("memory.analytics.orders_sv").unwrap();
            assert_eq!(r.name, "orders_sv");
            assert_eq!(r.schema.as_deref(), Some("analytics"));
            assert_eq!(r.database.as_deref(), Some("memory"));
        }

        #[test]
        fn every_part_folds_to_lowercase() {
            // Same DuckDB case-insensitivity rule `normalize_view_name` applies
            // to the name — it holds for the qualifier parts too, quoted or not.
            let r = parse_view_ref("Memory.\"Analytics\".ORDERS_SV").unwrap();
            assert_eq!(r.database.as_deref(), Some("memory"));
            assert_eq!(r.schema.as_deref(), Some("analytics"));
            assert_eq!(r.name, "orders_sv");
        }

        #[test]
        fn quoted_dot_is_one_part_not_a_qualifier() {
            // The mirror of the SHOW ... IN SCHEMA "a.b" case: a quoted dot is
            // part of the NAME, so this view is unqualified.
            let r = parse_view_ref("\"a.b\"").unwrap();
            assert_eq!(r.name, "a.b");
            assert_eq!(r.schema, None);
        }

        #[test]
        fn quoted_dotted_schema_qualifies_a_bare_name() {
            let r = parse_view_ref("\"a.b\".v").unwrap();
            assert_eq!(r.schema.as_deref(), Some("a.b"));
            assert_eq!(r.name, "v");
        }

        #[test]
        fn four_parts_are_rejected() {
            let err = parse_view_ref("a.b.c.d").unwrap_err();
            assert!(
                err.contains("too many"),
                "expected a too-many-parts error, got: {err}"
            );
        }

        #[test]
        fn parser_errors_propagate() {
            assert!(parse_view_ref("").is_err());
            assert!(parse_view_ref("\"foo").is_err());
            assert!(parse_view_ref("a..b").is_err());
        }

        #[test]
        fn display_leaves_ordinary_parts_bare() {
            // Positive control for `display_round_trips_a_part_needing_quotes`:
            // the common case must not grow quotes, or every error message
            // naming a plain view would start shouting.
            let r = parse_view_ref("memory.analytics.orders_sv").unwrap();
            assert_eq!(r.to_string(), "memory.analytics.orders_sv");
        }

        #[test]
        fn display_round_trips_a_part_needing_quotes() {
            // A database alias may legally contain a dot when quoted
            // (`ATTACH ':memory:' AS "a.b"`), and this parser accepts it.
            // Rendering the parts raw prints `a.b.main.v` — indistinguishable
            // from the four-part reference `parse_view_ref` would have
            // REJECTED, so an error naming the reference would misreport what
            // was actually parsed. The rendering must re-parse to the same
            // reference.
            let r = parse_view_ref("\"a.b\".main.v").unwrap();
            assert_eq!(r.database.as_deref(), Some("a.b"));
            assert_eq!(r.to_string(), "\"a.b\".main.v");
            assert_eq!(parse_view_ref(&r.to_string()).unwrap(), r);
        }

        #[test]
        fn name_agrees_with_normalize_view_name() {
            // `parse_view_ref` is the qualifier-preserving sibling: for the NAME
            // slot the two must never disagree, or a lookup routed through one
            // would miss a row written through the other.
            for input in [
                "orders_sv",
                "Sales",
                "\"Sales\"",
                "main.Sales",
                "\"memory\".\"main\".\"orders_sv\"",
                "\"a.b\"",
            ] {
                assert_eq!(
                    parse_view_ref(input).unwrap().name,
                    normalize_view_name(input).unwrap(),
                    "name slot diverged for {input:?}"
                );
            }
        }
    }

    mod find_identifier_end_tests {
        use super::*;

        #[test]
        fn bare_until_whitespace() {
            assert_eq!(find_identifier_end("orders_sv AS x", true), 9);
        }

        #[test]
        fn quoted_skips_inner_whitespace() {
            // `"my table"` is 10 bytes; the next byte is whitespace.
            assert_eq!(find_identifier_end("\"my table\" AS x", true), 10);
        }

        #[test]
        fn quoted_skips_inner_dot() {
            // `"a.b".c` is 7 bytes; followed by ` PRIMARY...`.
            assert_eq!(find_identifier_end("\"a.b\".c PRIMARY", true), 7);
        }

        #[test]
        fn paren_terminator_when_allowed() {
            assert_eq!(find_identifier_end("v(foo)", true), 1);
        }

        #[test]
        fn paren_inert_when_not_allowed() {
            // `(` is not a delimiter when allow_paren is false; the scan
            // continues past it and reaches end-of-input.
            assert_eq!(find_identifier_end("v(foo)", false), 6);
        }

        #[test]
        fn semicolon_terminator() {
            assert_eq!(find_identifier_end("orders_sv;", true), 9);
        }

        #[test]
        fn unterminated_quote_returns_input_len() {
            let s = "\"foo bar";
            assert_eq!(find_identifier_end(s, true), s.len());
        }

        #[test]
        fn reaches_end_of_input() {
            assert_eq!(find_identifier_end("orders_sv", true), 9);
        }

        #[test]
        fn doubled_quote_escape_keeps_in_quotes() {
            // `"a""b" rest`: the `""` is an escape, so the scan stays inside
            // the quoted region and terminates at the space after the final
            // `"`. Total length up to space: 7.
            let s = "\"a\"\"b\" rest";
            assert_eq!(find_identifier_end(s, true), 6);
        }

        #[test]
        fn fqn_with_quoted_parts_runs_to_whitespace() {
            // `"db"."sch"."v" AS ...` — total 14 bytes before space.
            let s = "\"db\".\"sch\".\"v\" AS x";
            assert_eq!(find_identifier_end(s, true), 14);
        }
    }

    // -------------------------------------------------------------------
    // Round-trip property tests
    //
    // For any legal identifier-vector v,
    //   parse_qualified_identifier(quote_ident(v[0]) ... + "." + ...) == Ok(v)
    //
    // i.e. parse is a left-inverse of quote_ident-and-join for any sequence
    // of non-empty parts. The alphabet deliberately includes `"`, `.`, and
    // ` ` — those are the bytes whose handling we are exercising.
    // -------------------------------------------------------------------

    mod proptests {
        use super::*;
        use crate::expand::quote_ident;
        use proptest::prelude::*;

        /// Emit `parts` via `quote_ident` and join with `.`. This is the
        /// inverse of `parse_qualified_identifier` for any legal input.
        fn emit_via_quote_ident(parts: &[String]) -> String {
            parts
                .iter()
                .map(|p| quote_ident(p))
                .collect::<Vec<_>>()
                .join(".")
        }

        /// Identifier-part alphabet: printable ASCII (including `"`, `.`,
        /// space) PLUS a unicode arm and keyword arms (TC-3, code-review
        /// 2026-07-02 — the previous ASCII-only alphabet systematically
        /// missed the shapes behind PA-1/PA-2).
        fn arb_part() -> impl Strategy<Value = String> {
            prop_oneof![
                3 => "[\\x20-\\x7E]{1,16}".boxed(),
                2 => "[a-zA-ZéàçßΩλ東京日本語☕ \".]{1,10}".boxed(),
                1 => prop::sample::select(vec![
                    "SELECT".to_string(),
                    "PRIMARY KEY".to_string(),
                    "café".to_string(),
                    "wéird name".to_string(),
                ]).boxed(),
            ]
        }

        proptest! {
            /// parse(emit(v)) == Ok(v) for any 1..=4-part vector of
            /// non-empty parts across the full alphabet.
            #[test]
            fn parse_emit_roundtrip_is_identity(
                parts in prop::collection::vec(arb_part(), 1..=4)
            ) {
                let emitted = emit_via_quote_ident(&parts);
                let parsed = parse_qualified_identifier(&emitted);
                prop_assert_eq!(
                    parsed.as_ref(),
                    Ok(&parts),
                    "round trip failed for parts={:?}, emitted={:?}",
                    parts,
                    emitted,
                );
            }
        }

        proptest! {
            /// normalize_view_name(emit(v)) == Ok(v.last().to_lowercase()).
            /// Quoted identifiers fold to lowercase like unquoted ones under
            /// DuckDB's case-insensitive rule (revised 2026-07-12), so the last
            /// part round-trips as its ASCII-lowercased form.
            #[test]
            fn normalize_returns_last_part(
                parts in prop::collection::vec(arb_part(), 1..=4)
            ) {
                let emitted = emit_via_quote_ident(&parts);
                let normalised = normalize_view_name(&emitted);
                let expected = parts.last().unwrap().to_ascii_lowercase();
                prop_assert_eq!(
                    normalised,
                    Ok(expected),
                    "normalize failed for parts={:?}, emitted={:?}",
                    parts,
                    emitted,
                );
            }
        }

        proptest! {
            /// Bare (unquoted) names fold to ASCII lowercase (PA-8), and
            /// folding then quoting round-trips through normalize.
            #[test]
            fn bare_names_fold_to_lowercase(
                name in "[A-Za-z_][A-Za-z0-9_]{0,16}"
            ) {
                let folded = name.to_ascii_lowercase();
                prop_assert_eq!(
                    normalize_view_name(&name),
                    Ok(folded.clone()),
                );
                // Quoting the folded name preserves it exactly.
                prop_assert_eq!(
                    normalize_view_name(&quote_ident(&folded)),
                    Ok(folded),
                );
            }
        }
    }

    mod normalize_ident_part_tests {
        use super::*;

        #[test]
        fn unquoted_folds_to_lowercase() {
            assert_eq!(normalize_ident_part("Region"), "region");
            assert_eq!(normalize_ident_part("REGION"), "region");
            assert_eq!(normalize_ident_part("region"), "region");
        }

        #[test]
        fn quoted_strips_quotes_and_folds() {
            // DuckDB: quoted identifiers are case-insensitive too, so a quoted
            // part strips its quotes AND folds to lowercase (revised
            // 2026-07-12, replacing the Snowflake preserve-quoted rule).
            assert_eq!(normalize_ident_part("\"Region\""), "region");
            assert_eq!(normalize_ident_part("\"REGION\""), "region");
            // Doubled-quote escape is unescaped, then folded.
            assert_eq!(normalize_ident_part("\"a\"\"B\""), "a\"b");
        }

        #[test]
        fn qualified_normalizes_each_part() {
            assert_eq!(normalize_ident_part("O.Region"), "o.region");
            assert_eq!(normalize_ident_part("o.\"Region\""), "o.region");
        }

        #[test]
        fn whitespace_trimmed() {
            assert_eq!(normalize_ident_part("  Region  "), "region");
        }

        #[test]
        fn malformed_falls_back_to_lowercase_fold() {
            // Unterminated quote — total, never panics.
            assert_eq!(normalize_ident_part("\"oops"), "\"oops");
        }

        #[test]
        fn ident_matches_unquoted_is_case_insensitive() {
            // Identical to the former eq_ignore_ascii_case behaviour.
            assert!(ident_matches("region", "REGION"));
            assert!(ident_matches("Region", "region"));
            assert!(ident_matches("region", "region"));
            assert!(!ident_matches("region", "country"));
        }

        #[test]
        fn ident_matches_quoted_is_case_insensitive() {
            // DuckDB: quoting does not affect matching, and case is ignored on
            // both sides — a quoted name matches any-case quoted or unquoted
            // reference (revised 2026-07-12).
            assert!(ident_matches("\"Region\"", "\"Region\""));
            assert!(ident_matches("\"Region\"", "\"region\""));
            assert!(ident_matches("\"Region\"", "\"REGION\""));
            // Quoted vs unquoted, any case: all match (quotes stripped, folded).
            assert!(ident_matches("Region", "\"Region\""));
            assert!(ident_matches("\"Region\"", "region"));
            assert!(ident_matches("Region", "region"));
            // Distinct names still don't match.
            assert!(!ident_matches("\"Region\"", "\"Country\""));
        }

        #[test]
        fn first_unquoted_dot_ignores_dots_in_quotes() {
            // Top-level dot after the alias.
            assert_eq!(first_unquoted_dot("o.region"), Some(1));
            // Dot only inside a quoted part → no top-level dot.
            assert_eq!(first_unquoted_dot("\"a.b\""), None);
            // Qualifier dot present, plus a dot inside the quoted name part:
            // split at the qualifier dot (offset 1), not the inner one.
            assert_eq!(first_unquoted_dot("o.\"a.b\""), Some(1));
            // Bare name, no dot.
            assert_eq!(first_unquoted_dot("region"), None);
            // Doubled-quote escape keeps us inside the quoted region.
            assert_eq!(first_unquoted_dot("\"a\"\"b.c\""), None);
        }
    }
}
