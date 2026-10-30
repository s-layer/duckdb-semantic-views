//! CREATE [OR REPLACE | OR ALTER] SEMANTIC VIEW as a grammar production.
//!
//! The rule matches the statement's prefix and the view name, then takes every
//! token up to the end of the statement without interpreting it
//! (`SvCreateToken`, `DuckDB`'s passthrough matcher). The name is a
//! `SvViewName`, a `ColId` as in DROP, ALTER, DESCRIBE and SHOW, so a reserved
//! word must be quoted, as in `DuckDB`'s own DDL. The transform rebuilds the statement's text from its
//! tokens ([`Tree::text`]) and plans it with [`plan_create`], the Rust body
//! parser's entry point. So every CREATE form (`AS` body, `FROM YAML`, `FROM
//! YAML FILE`, `IF NOT EXISTS`, `COMMENT =`) is parsed in one place. An error
//! position in the text is moved by the statement's offset, so it is one in
//! the query.
//!
//! `DuckDB` gives a transform only the tokens, so the stored text of an
//! expression has a space where it had a tab, one line break where it had
//! several, and no comments.

use super::internal;
use super::tree::Tree;
use crate::errors::ParseError;
use crate::parse::{plan_create, RewriteAction};

/// The rules, one per line. `SvViewName` comes from `show::RULES`.
/// `SvCreateToken`'s body is a placeholder: the terminal override in
/// [`ANY_TOKEN`] is what matches.
pub(super) const RULES: &str = r"
SvCreate <- 'CREATE' SvOrReplace? 'SEMANTIC' 'VIEW' SvIfNotExists? SvViewName SvCreateToken*
SvOrReplace <- 'OR' ('REPLACE' / 'ALTER')
SvIfNotExists <- 'IF' 'NOT' 'EXISTS'
SvCreateToken <- Identifier
";

/// The statement rules, each lowered by [`transform`].
pub(super) const STATEMENTS: &[&str] = &["SvCreate"];

/// Rules matched by `DuckDB`'s any-token matcher instead of their body.
pub(super) const ANY_TOKEN: &[&str] = &["SvCreateToken"];

pub(super) fn transform(tree: &Tree) -> Result<RewriteAction, ParseError> {
    let base = tree.root.offset().unwrap_or(0);
    let text = tree.text(&tree.root);
    plan_create(&text)
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
    fn a_create_plans_as_its_text_plans() {
        let sql = "CREATE OR REPLACE SEMANTIC VIEW s.v AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.r AS o.region)";
        let got = transform(&tree(sql, root(sql, "SvCreate", vec![]))).unwrap();
        let want = crate::parse::plan_create(sql).unwrap().unwrap();
        assert_eq!(got, want);
        assert!(matches!(got, RewriteAction::Create { .. }));
    }

    #[test]
    fn create_or_alter_plans_as_its_text_plans() {
        let sql = "CREATE OR ALTER SEMANTIC VIEW v AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.r AS o.region)";
        let got = transform(&tree(sql, root(sql, "SvCreate", vec![]))).unwrap();
        assert_eq!(got, crate::parse::plan_create(sql).unwrap().unwrap());
        assert!(matches!(
            got,
            RewriteAction::Create {
                mode: crate::parse::CreateMode::OrAlter,
                ..
            }
        ));
    }

    /// The body is planned from the rebuilt text: a tab inside an expression
    /// becomes a space and blank lines one line break.
    #[test]
    fn the_body_is_planned_from_the_rebuilt_text() {
        let sql = "SELECT 1; CREATE SEMANTIC VIEW v AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.r AS upper(\to.region ||\n\n  'x'))";
        let stmt = &sql[10..];
        let t = tree(sql, n(sql, "SvCreate", stmt, Kind::List, "", vec![]));
        let got = transform(&t).unwrap();
        let rebuilt = t.text(&t.root);
        assert!(!rebuilt.contains('\t'));
        let want = crate::parse::plan_create(&rebuilt).unwrap().unwrap();
        assert_eq!(got, want);
        assert_ne!(got, crate::parse::plan_create(stmt).unwrap().unwrap());
    }

    /// The statement after another one in the same string: an error position
    /// is the query's, not the statement's.
    #[test]
    fn an_error_position_is_an_offset_in_the_query() {
        let sql = "SELECT 1; CREATE SEMANTIC VIEW v AS TABLES (o AS orders) DIMENSIONS (o.r AS o.region) DIMENSIONS (o.s AS o.s)";
        let stmt = &sql[10..];
        let r = n(sql, "SvCreate", stmt, Kind::List, "", vec![]);
        let err = transform(&tree(sql, r)).unwrap_err();
        let alone = crate::parse::plan_create(stmt).unwrap_err();
        assert_eq!(err.message, alone.message);
        assert_eq!(err.position, alone.position.map(|p| p + 10));
        assert!(err.position.is_some());
    }
}
