//! The parts of `SHOW SEMANTIC ...` that the grammar productions share.
//!
//! The grammar (`crate::grammar::show`) splits a SHOW statement into its
//! `LIKE` / `IN` / `FOR METRIC` / `STARTS WITH` / `LIMIT` clauses and fills a
//! [`ShowClauses`]. This module renders those clauses as the SQL `WHERE` /
//! `LIMIT` suffix of the catalog query ([`build_filter_suffix`]), and words the
//! error for text left over after them ([`show_unexpected_tokens`]).

use super::DdlKind;
use crate::sql_lit::SqlLit;

/// Build optional WHERE and LIMIT suffix for a SHOW rewrite.
///
/// LIKE maps to `name ILIKE '<escaped>'` (case-insensitive).
/// STARTS WITH maps to `starts_with(name, '<escaped>')` (case-sensitive).
/// IN SCHEMA maps to `lower(schema_name) = lower('<escaped>')`.
/// IN DATABASE maps to `lower(database_name) = lower('<escaped>')`.
/// All conditions combined with AND. LIMIT appended last.
///
/// The last two fold case because `DuckDB` resolves identifiers case-insensitively
/// and this project follows that rule everywhere else (`ident::ident_matches`).
/// They are the two slots where it also has to be *load-bearing*: the stored
/// side is stamped from `current_schema()` / `current_database()` at CREATE
/// time, which `DuckDB` returns as the spelling the caller last wrote in `USE`
/// rather than the catalog's, so two views in one schema can be stamped
/// `MySchema` and `myschema` and an exact match could return at most one.
///
/// The fold is left to SQL rather than applied to the requested name in Rust
/// because the stored side is folded by `DuckDB`'s Unicode-aware `lower()`,
/// while `ident::normalize_ident_part` is `to_ascii_lowercase`. Folding here
/// would emit `'myschÉma'` against a stored `'myschéma'` and break a non-ASCII
/// name that matches exactly today. `lower(x) = lower(y)` puts both sides
/// through the same function.
///
/// It stays an equality rather than becoming `ILIKE`: `SqlLit::escape` only
/// doubles single quotes, so a `%` or `_` in a schema name would turn into a
/// wildcard.
///
/// IDENT-6: `STARTS WITH` takes the same treatment for the same reason. It used
/// to emit `name LIKE '<escaped>%'`, which handed the user's prefix to the
/// pattern matcher — `STARTS WITH 'a_b'` also matched `axb`, and
/// `STARTS WITH '100%'` matched every name beginning `100`. Snowflake's
/// `STARTS WITH` is a literal prefix, so the predicate is now `DuckDB`'s
/// `starts_with(name, '<escaped>')` scalar function: no metacharacters, nothing
/// to escape beyond the quote doubling `SqlLit` already does, and no `ESCAPE`
/// clause whose escape character would itself need escaping. `LIKE` keeps its
/// pattern semantics — there the wildcards are the point.
/// `after` is `LIMIT <n> FROM '<name>'`'s cursor: it keeps the rows whose name
/// sorts after it, `name > '<escaped>'`. Like `STARTS WITH`, the comparison is
/// case-sensitive, and the string need not be a whole name, as in Snowflake.
pub(crate) fn build_filter_suffix(
    like_pattern: Option<&str>,
    starts_with: Option<&str>,
    limit: Option<u64>,
    in_schema: Option<&ScopeName>,
    in_database: Option<&ScopeName>,
    after: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(pattern) = like_pattern {
        let escaped = SqlLit::escape(pattern);
        parts.push(format!("name ILIKE '{escaped}'"));
    }
    if let Some(prefix) = starts_with {
        let escaped = SqlLit::escape(prefix);
        parts.push(format!("starts_with(name, '{escaped}')"));
    }
    if let Some(schema) = in_schema {
        parts.push(schema.predicate("schema_name", "current_schema"));
    }
    if let Some(db) = in_database {
        parts.push(db.predicate("database_name", "current_database"));
    }
    if let Some(after) = after {
        let escaped = SqlLit::escape(after);
        parts.push(format!("name > '{escaped}'"));
    }
    let mut suffix = String::new();
    if !parts.is_empty() {
        suffix.push_str(" WHERE ");
        suffix.push_str(&parts.join(" AND "));
    }
    if let Some(n) = limit {
        use std::fmt::Write;
        let _ = write!(suffix, " LIMIT {n}");
    }
    suffix
}

/// What a scope clause names: an explicit object, or the caller's current one.
///
/// Snowflake writes the name as optional — `IN SCHEMA` alone means the current
/// schema, `IN SCHEMA analytics` a specific one — and the two cannot share a
/// representation, because the bare form has to reach SQL as a **call** rather
/// than a literal so it resolves on the caller's connection.
///
/// The bare form is exact rather than approximate, which is worth stating: the
/// stored `schema_name` is itself stamped from `current_schema()` at CREATE
/// time, so `IN SCHEMA` compares that function against itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScopeName {
    /// An explicit name, already unquoted.
    Named(String),
    /// The bare `IN SCHEMA` / `IN DATABASE` form — the caller's current one.
    Current,
}

impl ScopeName {
    /// Render this scope as a `WHERE` predicate over `column`.
    ///
    /// `current_fn` is the SQL function supplying the bare form's value —
    /// `current_schema` or `current_database`. Both sides are folded for the
    /// reasons in [`build_filter_suffix`]; for [`ScopeName::Current`] that is
    /// belt-and-braces, since the two sides are the same function.
    fn predicate(&self, column: &str, current_fn: &str) -> String {
        match self {
            Self::Named(name) => {
                let escaped = SqlLit::escape(name);
                format!("lower({column}) = lower('{escaped}')")
            }
            Self::Current => format!("lower({column}) = lower({current_fn}())"),
        }
    }
}

/// Parsed filter clauses from a SHOW SEMANTIC command.
pub(crate) struct ShowClauses<'a> {
    pub(crate) like_pattern: Option<String>,
    pub(crate) in_view: Option<&'a str>,
    /// Owned, unlike the other name slots: these two are the only ones whose
    /// value reaches SQL as a plain string literal with no downstream
    /// normalization, so the grammar strips their quotes and the result no
    /// longer borrows the query.
    ///
    /// A qualified `IN SCHEMA <db>.<schema>` populates **both**, which is how
    /// the database half of Snowflake's qualified form gets applied.
    ///
    /// `IN ACCOUNT` leaves both `None`: it is Snowflake's "everything I can
    /// see", which here is simply an unfiltered listing.
    pub(crate) in_schema: Option<ScopeName>,
    pub(crate) in_database: Option<ScopeName>,
    pub(crate) for_metric: Option<&'a str>,
    pub(crate) starts_with: Option<String>,
    pub(crate) limit: Option<u64>,
    /// `LIMIT <n> FROM '<name>'`: list only the views named after `<name>`.
    /// `SHOW SEMANTIC VIEWS` only, as in Snowflake.
    pub(crate) limit_from: Option<String>,
}

/// The error for text left over after a SHOW statement's clauses, with the
/// statement's usage. The grammar productions report a name with whitespace
/// inside it the same way.
pub(crate) fn show_unexpected_tokens(kind: DdlKind, rest: &str) -> String {
    let entity = match kind {
        DdlKind::Show | DdlKind::ShowTerse => "VIEWS",
        DdlKind::ShowDimensions => "DIMENSIONS",
        DdlKind::ShowMetrics => "METRICS",
        DdlKind::ShowMaterializations => "MATERIALIZATIONS",
        _ => "FACTS",
    };
    let for_metric = if kind == DdlKind::ShowDimensions {
        " [FOR METRIC metric_name]"
    } else {
        ""
    };
    let limit_from = if matches!(kind, DdlKind::Show | DdlKind::ShowTerse) {
        " [FROM '<name>']"
    } else {
        ""
    };
    format!(
        "Unexpected tokens: '{rest}'. \
         Usage: SHOW SEMANTIC {entity} [LIKE '<pattern>'] [IN {{view_name | ACCOUNT | DATABASE [db] | SCHEMA [[db.]schema]}}]{for_metric} [STARTS WITH '<prefix>'] [LIMIT <n>{limit_from}]"
    )
}

#[cfg(test)]
mod tests {
    use super::{build_filter_suffix, ScopeName};

    #[test]
    fn filter_suffix_escapes_single_quotes() {
        assert_eq!(
            build_filter_suffix(Some("O'Brien"), None, None, None, None, None),
            " WHERE name ILIKE 'O''Brien'"
        );
        assert_eq!(
            build_filter_suffix(None, Some("O'Br"), None, None, None, None),
            " WHERE starts_with(name, 'O''Br')"
        );
        assert_eq!(
            build_filter_suffix(
                None,
                None,
                None,
                Some(&ScopeName::Named("sch'ema".into())),
                Some(&ScopeName::Named("d'b".into())),
                None,
            ),
            " WHERE lower(schema_name) = lower('sch''ema') AND lower(database_name) = lower('d''b')"
        );
    }

    #[test]
    fn limit_from_keeps_the_names_after_the_cursor() {
        assert_eq!(
            build_filter_suffix(None, Some("v"), Some(3), None, None, Some("v_3")),
            " WHERE starts_with(name, 'v') AND name > 'v_3' LIMIT 3"
        );
        assert_eq!(
            build_filter_suffix(None, None, Some(1), None, None, Some("O'B")),
            " WHERE name > 'O''B' LIMIT 1"
        );
    }

    #[test]
    fn filter_suffix_plain_values_and_limit_unchanged() {
        assert_eq!(
            build_filter_suffix(Some("cust%"), None, Some(10), None, None, None),
            " WHERE name ILIKE 'cust%' LIMIT 10"
        );
        assert_eq!(build_filter_suffix(None, None, None, None, None, None), "");
    }

    /// IDENT-6 (code-review 2026-08-08): `STARTS WITH` is a **literal** prefix
    /// in Snowflake, but the predicate used to be `name LIKE '<prefix>%'` with
    /// only `SqlLit`'s quote-doubling applied — so a `_` or `%` inside the
    /// prefix reached `LIKE` as a wildcard. `STARTS WITH 'a_b'` matched `axb`.
    ///
    /// The fix routes the prefix through `starts_with(name, '<literal>')`, the
    /// same "don't hand a user value to a pattern matcher" choice
    /// [`build_filter_suffix`]'s doc records for IN SCHEMA (equality, not
    /// `ILIKE`). Nothing else changes: `starts_with` is case-sensitive, which
    /// is what the clause already promised.
    #[test]
    fn starts_with_prefix_is_literal_not_a_like_pattern() {
        assert_eq!(
            build_filter_suffix(None, Some("a_b"), None, None, None, None),
            " WHERE starts_with(name, 'a_b')"
        );
        assert_eq!(
            build_filter_suffix(None, Some("100%"), None, None, None, None),
            " WHERE starts_with(name, '100%')"
        );
        // Quote escaping still happens exactly once.
        assert_eq!(
            build_filter_suffix(None, Some("O'Br"), None, None, None, None),
            " WHERE starts_with(name, 'O''Br')"
        );
        // LIKE is untouched: it is a pattern slot by contract.
        assert_eq!(
            build_filter_suffix(Some("a_b"), None, None, None, None, None),
            " WHERE name ILIKE 'a_b'"
        );
    }

    /// The behavioural half of IDENT-6: the emitted predicate must actually
    /// discriminate `a_b` from `axb` when `DuckDB` runs it. A unit test on the
    /// predicate string alone cannot tell a correct escape from a
    /// plausible-looking one, so this executes it.
    ///
    /// Gated off the `extension` feature, which swaps the bundled `DuckDB` API
    /// for loadable-extension stubs — `Connection::open_in_memory` panics there
    /// with "DuckDB API not initialized". The default-features run covers it.
    #[cfg(not(feature = "extension"))]
    #[test]
    fn starts_with_discriminates_underscore_from_wildcard_in_duckdb() {
        let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB opens");
        conn.execute_batch(
            "CREATE TABLE t(name VARCHAR); \
             INSERT INTO t VALUES ('a_b'), ('axb'), ('a_bc'), ('100%x'), ('1005x');",
        )
        .expect("fixture rows insert");

        let matches = |prefix: &str| -> Vec<String> {
            let suffix = build_filter_suffix(None, Some(prefix), None, None, None, None);
            let sql = format!("SELECT name FROM t{suffix} ORDER BY name");
            let mut stmt = conn.prepare(&sql).expect("emitted predicate is valid SQL");
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .expect("query runs")
                .collect::<Result<Vec<_>, _>>()
                .expect("rows read");
            rows
        };

        assert_eq!(
            matches("a_b"),
            vec!["a_b".to_string(), "a_bc".to_string()],
            "`_` in a STARTS WITH prefix is a literal underscore, not a single-char wildcard"
        );
        assert_eq!(
            matches("100%"),
            vec!["100%x".to_string()],
            "`%` in a STARTS WITH prefix is a literal percent, not a match-anything wildcard"
        );
    }
}
