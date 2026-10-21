//! CREATE [OR REPLACE] SEMANTIC VIEW as a grammar production.
//!
//! The rule matches the statement's prefix and then takes every token up to
//! the end of the statement without interpreting it (`SvCreateToken`, `DuckDB`'s
//! passthrough matcher). The transform takes the statement's text
//! ([`Tree::text`]) and plans it with the parser override's own
//! [`plan_rewrite`]. So every CREATE form (`AS` body, `FROM YAML`, `FROM YAML
//! FILE`, `IF NOT EXISTS`, `COMMENT =`) is accepted and refused exactly as the
//! override does, with the same messages. An error position in the text is
//! moved by the statement's offset, so it is one in the query.
//!
//! With the parser override off, the bridge has no source text and the text is
//! rebuilt from the tokens, so a tab inside an expression becomes a space and a
//! run of blank lines one line break (TECH-DEBT #100).

use super::internal;
use super::tree::Tree;
use crate::errors::ParseError;
use crate::parse::{plan_rewrite, RewriteAction};

/// The rules, one per line. `SvCreateToken`'s body is a placeholder: the
/// terminal override in [`ANY_TOKEN`] is what matches.
pub(super) const RULES: &str = r"
SvCreate <- 'CREATE' SvOrReplace? 'SEMANTIC' 'VIEW' SvCreateToken*
SvOrReplace <- 'OR' 'REPLACE'
SvCreateToken <- Identifier
";

/// The statement rules, each lowered by [`transform`].
pub(super) const STATEMENTS: &[&str] = &["SvCreate"];

/// Rules matched by `DuckDB`'s any-token matcher instead of their body.
pub(super) const ANY_TOKEN: &[&str] = &["SvCreateToken"];

pub(super) fn transform(tree: &Tree) -> Result<RewriteAction, ParseError> {
    let base = tree.root.offset().unwrap_or(0);
    let text = tree.text(&tree.root);
    plan_rewrite(&text)
        .map_err(|e| ParseError {
            position: e.position.map(|p| p + base),
            ..e
        })?
        .ok_or_else(|| internal(&tree.root, "statement is not a CREATE"))
}

#[cfg(test)]
mod tests {
    use super::transform;
    use crate::grammar::tree::tests::{n, root, tree};
    use crate::grammar::tree::Kind;
    use crate::parse::RewriteAction;

    #[test]
    fn a_create_plans_as_the_parser_override_plans_it() {
        let sql = "CREATE OR REPLACE SEMANTIC VIEW s.v AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.r AS o.region)";
        let got = transform(&tree(sql, root(sql, "SvCreate", vec![]))).unwrap();
        let want = crate::parse::plan_rewrite(sql).unwrap().unwrap();
        assert_eq!(got, want);
        assert!(matches!(got, RewriteAction::Create { .. }));
    }

    /// With the query's text, an expression keeps its tabs and blank lines,
    /// as it does through the parser override.
    #[test]
    fn with_the_source_the_body_is_planned_as_written() {
        let sql = "SELECT 1; CREATE SEMANTIC VIEW v AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.r AS upper(\to.region ||\n\n  'x'))";
        let stmt = &sql[10..];
        let mut t = tree(sql, n(sql, "SvCreate", stmt, Kind::List, "", vec![]));
        t.source = Some(crate::grammar::tree::Source {
            offset: 10,
            text: stmt.to_string(),
        });
        let got = transform(&t).unwrap();
        let want = crate::parse::plan_rewrite(stmt).unwrap().unwrap();
        assert_eq!(got, want);
    }

    /// The statement after another one in the same string: an error position
    /// is the query's, not the statement's.
    #[test]
    fn an_error_position_is_an_offset_in_the_query() {
        let sql = "SELECT 1; CREATE SEMANTIC VIEW v AS TABLES (o AS orders) DIMENSIONS (o.r AS o.region) DIMENSIONS (o.s AS o.s)";
        let stmt = &sql[10..];
        let r = n(sql, "SvCreate", stmt, Kind::List, "", vec![]);
        let err = transform(&tree(sql, r)).unwrap_err();
        let alone = crate::parse::plan_rewrite(stmt).unwrap_err();
        assert_eq!(err.message, alone.message);
        assert_eq!(err.position, alone.position.map(|p| p + 10));
        assert!(err.position.is_some());
    }
}
