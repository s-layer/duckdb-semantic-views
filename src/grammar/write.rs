//! DROP and ALTER SEMANTIC VIEW as grammar productions.
//!
//! Both are statements, not queries: the C++ bridge returns what they lower to
//! (the guarded native DML of `crate::parse::lower_action`, several statements)
//! as one `MultiStatement` in place of the matched `Statement`.
//!
//! The rules accept only well-formed statements. A malformed one (`ALTER
//! SEMANTIC VIEW v TRUNCATE`, a missing name) does not match, so `DuckDB`'s
//! parser fails and the parse stub reports the extension's own message for it,
//! as it does for a statement the parser override declines. The deletion PR,
//! which removes the stub, has to move those messages into the grammar.

use super::tree::{Node, Tree};
use super::{internal, name_or, plain_string};
use crate::errors::ParseError;
use crate::ident::{parse_view_ref, ViewRef};
use crate::parse::RewriteAction;

/// The rules, one per line. `SvViewName` comes from `show::RULES`.
pub(super) const RULES: &str = r"
SvDrop <- 'DROP' 'SEMANTIC' 'VIEW' SvIfExists? SvViewName
SvIfExists <- 'IF' 'EXISTS'
SvAlter <- 'ALTER' 'SEMANTIC' 'VIEW' SvIfExists? SvViewName SvAlterAction
SvAlterAction <- SvAlterRename / SvAlterSetComment / SvAlterUnsetComment
SvAlterRename <- 'RENAME' 'TO' SvNewName
SvNewName <- SvNamePart ('.' SvNamePart)*
SvAlterSetComment <- 'SET' 'COMMENT' '=' StringLiteral
SvAlterUnsetComment <- 'UNSET' 'COMMENT'
";

/// The statement rules, each lowered by [`transform`].
pub(super) const STATEMENTS: &[&str] = &["SvDrop", "SvAlter"];

pub(super) fn transform(rule: &str, tree: &Tree) -> Result<RewriteAction, ParseError> {
    let root = &tree.root;
    let if_exists = root.find("SvIfExists").is_some();
    let is_drop = rule.eq_ignore_ascii_case("SvDrop");
    // The override reads ALTER's operation from where the source name ends,
    // so a name with a gap reads as an unknown operation.
    let name = view_ref(tree, root, "SvViewName", "Invalid view name", |rest| {
        if is_drop {
            format!("Unexpected tokens after view name: '{rest}'")
        } else {
            UNSUPPORTED_ALTER.to_string()
        }
    })?;
    if is_drop {
        return Ok(RewriteAction::Drop { name, if_exists });
    }
    let action = root
        .find("SvAlterAction")
        .ok_or_else(|| internal(root, "missing SvAlterAction"))?;
    if action.find("SvAlterRename").is_some() {
        let new_name = view_ref(
            tree,
            action,
            "SvNewName",
            "Invalid new view name in RENAME TO",
            |rest| format!("Unexpected tokens after new view name in RENAME TO: '{rest}'"),
        )?;
        return Ok(RewriteAction::AlterRename {
            name,
            new_name,
            if_exists,
        });
    }
    if let Some(set) = action.find("SvAlterSetComment") {
        return Ok(RewriteAction::AlterSetComment {
            name,
            comment: plain_string(
                tree,
                set,
                "Expected single-quoted string after SET COMMENT =",
            )?,
            if_exists,
        });
    }
    if action.find("SvAlterUnsetComment").is_some() {
        return Ok(RewriteAction::AlterUnsetComment { name, if_exists });
    }
    Err(internal(action, "unknown ALTER operation"))
}

/// The parser override's message for an ALTER whose operation it cannot read.
const UNSUPPORTED_ALTER: &str =
    "Unsupported ALTER operation. Supported: RENAME TO, SET COMMENT, UNSET COMMENT.";

/// The view named by the `rule` node under `parent`, folded as the parser
/// override folds it (`parse_view_ref`). `gap` words the error for a name
/// with whitespace inside it, given the rest of the statement.
fn view_ref(
    tree: &Tree,
    parent: &Node,
    rule: &str,
    what: &str,
    gap: impl FnOnce(&str) -> String,
) -> Result<ViewRef, ParseError> {
    let node = parent
        .find(rule)
        .ok_or_else(|| internal(parent, &format!("missing {rule}")))?;
    parse_view_ref(&name_or(tree, node, gap)?).map_err(|e| ParseError {
        message: format!("{what}: {e}"),
        position: node.offset(),
    })
}

#[cfg(test)]
mod tests {
    use super::transform;
    use crate::grammar::tree::tests::{n, root, tree};
    use crate::grammar::tree::{Kind, Node};
    use crate::ident::ViewRef;
    use crate::parse::RewriteAction;

    const L: Kind = Kind::List;

    fn name(sql: &str, rule: &str, text: &str) -> Node {
        n(sql, rule, text, L, "", vec![])
    }

    fn view(schema: Option<&str>, name: &str) -> ViewRef {
        ViewRef {
            database: None,
            schema: schema.map(str::to_string),
            name: name.to_string(),
        }
    }

    #[test]
    fn drop_folds_the_name_and_keeps_its_schema() {
        let sql = "DROP SEMANTIC VIEW Analytics.Orders";
        let r = root(
            sql,
            "SvDrop",
            vec![name(sql, "SvViewName", "Analytics.Orders")],
        );
        assert_eq!(
            transform("SvDrop", &tree(sql, r)).unwrap(),
            RewriteAction::Drop {
                name: view(Some("analytics"), "orders"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn drop_if_exists_sets_the_flag() {
        let sql = "DROP SEMANTIC VIEW IF EXISTS v";
        let r = root(
            sql,
            "SvDrop",
            vec![
                name(sql, "SvIfExists", "IF EXISTS"),
                n(sql, "SvViewName", " v", L, "", vec![]),
            ],
        );
        let got = transform("SvDrop", &tree(sql, r)).unwrap();
        assert_eq!(
            got,
            RewriteAction::Drop {
                name: view(None, "v"),
                if_exists: true,
            }
        );
    }

    #[test]
    fn a_four_part_name_is_rejected_at_the_name() {
        let sql = "DROP SEMANTIC VIEW a.b.c.d";
        let r = root(sql, "SvDrop", vec![name(sql, "SvViewName", "a.b.c.d")]);
        let err = transform("SvDrop", &tree(sql, r)).unwrap_err();
        assert!(err
            .message
            .starts_with("Invalid view name: too many identifier parts"));
        assert_eq!(err.position, Some(sql.find("a.b").unwrap()));
    }

    #[test]
    fn alter_rename_reads_both_names() {
        let sql = "ALTER SEMANTIC VIEW IF EXISTS s.old RENAME TO New";
        let r = root(
            sql,
            "SvAlter",
            vec![
                name(sql, "SvIfExists", "IF EXISTS"),
                name(sql, "SvViewName", "s.old"),
                n(
                    sql,
                    "SvAlterAction",
                    "RENAME TO New",
                    L,
                    "",
                    vec![n(
                        sql,
                        "SvAlterRename",
                        "RENAME TO New",
                        L,
                        "",
                        vec![name(sql, "SvNewName", "New")],
                    )],
                ),
            ],
        );
        assert_eq!(
            transform("SvAlter", &tree(sql, r)).unwrap(),
            RewriteAction::AlterRename {
                name: view(Some("s"), "old"),
                new_name: view(None, "new"),
                if_exists: true,
            }
        );
    }

    #[test]
    fn alter_set_comment_takes_the_decoded_string() {
        let sql = "ALTER SEMANTIC VIEW v SET COMMENT = 'it''s'";
        let r = root(
            sql,
            "SvAlter",
            vec![
                n(sql, "SvViewName", " v ", L, "", vec![]),
                n(
                    sql,
                    "SvAlterAction",
                    "SET COMMENT = 'it''s'",
                    L,
                    "",
                    vec![n(
                        sql,
                        "SvAlterSetComment",
                        "SET COMMENT = 'it''s'",
                        L,
                        "",
                        vec![n(
                            sql,
                            "StringLiteral",
                            "'it''s'",
                            Kind::String,
                            "it's",
                            vec![],
                        )],
                    )],
                ),
            ],
        );
        assert_eq!(
            transform("SvAlter", &tree(sql, r)).unwrap(),
            RewriteAction::AlterSetComment {
                name: view(None, "v"),
                comment: "it's".to_string(),
                if_exists: false,
            }
        );
    }

    /// The parser override ends a name at whitespace and reports the rest;
    /// the grammar's tokens skip it, so the transform checks for the gap.
    #[test]
    fn a_name_with_whitespace_inside_is_reported_with_the_rest() {
        let sql = "DROP SEMANTIC VIEW s . v";
        let r = root(sql, "SvDrop", vec![name(sql, "SvViewName", "s . v")]);
        let err = transform("SvDrop", &tree(sql, r)).unwrap_err();
        assert_eq!(err.message, "Unexpected tokens after view name: '. v'");
        assert_eq!(err.position, Some(sql.find(" .").unwrap() + 1));
    }

    #[test]
    fn a_comment_that_is_not_a_plain_literal_is_refused() {
        for literal in ["$$x$$", "E'x'", "$t$x$t$"] {
            let sql = format!("ALTER SEMANTIC VIEW v SET COMMENT = {literal}");
            let sql = sql.as_str();
            let set = format!("SET COMMENT = {literal}");
            let lit = n(sql, "StringLiteral", literal, Kind::String, "x", vec![]);
            let set_node = n(sql, "SvAlterSetComment", &set, L, "", vec![lit]);
            let action = n(sql, "SvAlterAction", &set, L, "", vec![set_node]);
            let r = root(
                sql,
                "SvAlter",
                vec![n(sql, "SvViewName", " v ", L, "", vec![]), action],
            );
            let err = transform("SvAlter", &tree(sql, r)).unwrap_err();
            assert_eq!(
                err.message, "Expected single-quoted string after SET COMMENT =",
                "{literal}"
            );
            assert_eq!(err.position, Some(sql.find(literal).unwrap()), "{literal}");
        }
    }

    #[test]
    fn alter_unset_comment() {
        let sql = "ALTER SEMANTIC VIEW v UNSET COMMENT";
        let r = root(
            sql,
            "SvAlter",
            vec![
                n(sql, "SvViewName", " v ", L, "", vec![]),
                n(
                    sql,
                    "SvAlterAction",
                    "UNSET COMMENT",
                    L,
                    "",
                    vec![name(sql, "SvAlterUnsetComment", "UNSET COMMENT")],
                ),
            ],
        );
        assert_eq!(
            transform("SvAlter", &tree(sql, r)).unwrap(),
            RewriteAction::AlterUnsetComment {
                name: view(None, "v"),
                if_exists: false,
            }
        );
    }
}
