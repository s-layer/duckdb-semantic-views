//! Semantic-view syntax as productions of `DuckDB`'s own grammar (`DuckDB` 2.0).
//!
//! `DuckDB` 2.0 parses with a PEG grammar that an extension can extend. This
//! module owns our part of it: the rule text ([`changes`]) and the transforms
//! that turn a matched rule into SQL ([`transform`]). The C++ shim only
//! bridges: it registers the rules `DuckDB` gets from [`changes`], and when one
//! of the statement rules matches it serialises the parse subtree (see
//! [`tree`]), calls [`transform`] through `sv_grammar_transform_rust`, and
//! parses the SQL that comes back with the builtin parser.
//!
//! The parser override still runs first and still owns every statement that
//! starts with our syntax. The grammar takes what the override cannot reach,
//! such as SHOW inside a subquery, and everything when overrides are off
//! (`SET allow_parser_override_extension = 'DEFAULT'`), which is how the DDL
//! corpus is run against both while they coexist (`.planning/PEG-PARSER-EPIC.md`,
//! Phase 3).

mod create;
mod show;
pub(crate) mod tree;
mod write;

#[cfg(feature = "extension")]
pub(crate) mod ffi;

use crate::errors::ParseError;
use crate::parse::RewriteAction;
use std::fmt::Write;
use tree::{Kind, Node, Tree};

/// What a statement rule's transform returns, which decides where `DuckDB`
/// may use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// One query (a `QueryNode`): an alternative of `DescribeStatement`, so it
    /// also stands as a subquery.
    Query,
    /// A statement (a `SQLStatement`, several as one `MultiStatement`): an
    /// alternative of `Statement`, so it stands only at the top level.
    Statement,
}

impl Shape {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Statement => "statement",
        }
    }

    /// The `DuckDB` rule a statement rule of this shape is an alternative of.
    fn target(self) -> &'static str {
        match self {
            Self::Query => "DescribeStatement",
            Self::Statement => "Statement",
        }
    }
}

/// The grammar changes to register, one per line:
///
/// - `RULE\t<definition>\t<shape>`: add a rule. `<shape>` is empty for a
///   helper rule, or names the transform's result for a statement rule.
/// - `ANY_TOKEN\t<rule>`: match `<rule>` with `DuckDB`'s matcher for any one
///   token short of the statement's end, instead of its body.
/// - `PREPEND\t<rule>\t<choice>`: make `<choice>` the first alternative of
///   an existing `DuckDB` rule.
pub(crate) fn changes() -> String {
    let mut out = String::new();
    let statements: Vec<(&str, Shape)> = show::QUERY_STATEMENTS
        .iter()
        .map(|rule| (*rule, Shape::Query))
        .chain(
            write::STATEMENTS
                .iter()
                .chain(create::STATEMENTS)
                .map(|rule| (*rule, Shape::Statement)),
        )
        .collect();
    let definitions = show::RULES
        .lines()
        .chain(write::RULES.lines())
        .chain(create::RULES.lines());
    for definition in definitions.map(str::trim).filter(|l| !l.is_empty()) {
        let name = definition.split_whitespace().next().unwrap_or_default();
        let shape = statements
            .iter()
            .find(|(rule, _)| *rule == name)
            .map_or("", |(_, shape)| shape.wire_name());
        let _ = writeln!(out, "RULE\t{definition}\t{shape}");
    }
    for rule in create::ANY_TOKEN {
        let _ = writeln!(out, "ANY_TOKEN\t{rule}");
    }
    for (rule, shape) in &statements {
        let _ = writeln!(out, "PREPEND\t{}\t{rule}", shape.target());
    }
    out
}

/// Plan one matched statement rule: the same [`RewriteAction`] the parser
/// override plans for the statement, which `crate::parse::lower_action` turns
/// into SQL.
#[cfg_attr(not(feature = "extension"), allow(dead_code))]
pub(crate) fn transform(tree: &Tree) -> Result<RewriteAction, ParseError> {
    let rule = tree.root.rule.as_str();
    let is = |rules: &[&str]| rules.iter().any(|r| r.eq_ignore_ascii_case(rule));
    if is(show::QUERY_STATEMENTS) {
        return show::transform(rule, tree).map(RewriteAction::Passthrough);
    }
    if is(write::STATEMENTS) {
        return write::transform(rule, tree);
    }
    if is(create::STATEMENTS) {
        return create::transform(tree);
    }
    Err(ParseError {
        message: format!("semantic_views grammar: no transform for rule '{rule}'"),
        position: tree.root.offset(),
    })
}

// --- helpers the statement modules share ---

/// The decoded contents of the string literal under `node`, which must be
/// written `'...'`, as the parser override requires: `DuckDB`'s
/// `StringLiteral` also matches `$$...$$`, `$tag$...$tag$` and `E'...'`, which
/// get `message`, at the literal.
fn plain_string(tree: &Tree, node: &Node, message: &str) -> Result<String, ParseError> {
    let leaf = node
        .first_leaf(Kind::String)
        .ok_or_else(|| internal(node, "missing string literal"))?;
    let quoted = tree
        .tokens_in(leaf)
        .next()
        .is_some_and(|t| t.text.starts_with('\''));
    if !quoted {
        return Err(ParseError {
            message: message.to_string(),
            position: leaf.offset(),
        });
    }
    Ok(leaf.value.clone())
}

/// A name exactly as written, quotes and qualifiers included: the read table
/// functions fold and resolve it themselves (FF-4).
///
/// The grammar's tokens skip whitespace and comments, so `s . v` matches a
/// dotted name. The parser override ends a name at the first whitespace, as
/// `DuckDB` does, and reports what follows. So a name is one run of text: the
/// error is the offset of the first token that whitespace or a comment
/// separates from the one before it, for the caller to report with
/// [`rest_from`].
fn name_text(tree: &Tree, node: &Node) -> Result<String, usize> {
    let mut out = String::new();
    let mut end = None;
    for t in tree.tokens_in(node) {
        if end.is_some_and(|e| e != t.offset) {
            return Err(t.offset);
        }
        out.push_str(&t.text);
        end = Some(t.offset + t.len);
    }
    Ok(out)
}

/// The statement's text from query offset `offset` to its end, comments
/// blanked and trimmed: what the parser override reports as the unexpected
/// rest of a statement.
fn rest_from(tree: &Tree, offset: usize) -> String {
    let start = tree.root.offset().unwrap_or(0);
    let text = tree.text(&tree.root);
    let rest = text.get(offset.saturating_sub(start)..).unwrap_or_default();
    let rest = crate::util::blank_sql_comments(rest);
    rest.trim().trim_end_matches(';').trim_end().to_string()
}

/// [`name_text`], with the rest of the statement reported by `message` when
/// the name is not one run of text.
fn name_or(
    tree: &Tree,
    node: &Node,
    message: impl FnOnce(&str) -> String,
) -> Result<String, ParseError> {
    name_text(tree, node).map_err(|at| ParseError {
        message: message(&rest_from(tree, at)),
        position: Some(at),
    })
}

fn internal(node: &Node, what: &str) -> ParseError {
    ParseError {
        message: format!("semantic_views grammar: {what} in {}", node.rule),
        position: node.offset(),
    }
}

/// Run `body` under `catch_unwind` and convert a panic into the
/// return-code contract of `sv_grammar_transform_rust` (`ffi.rs`).
///
/// Split out so the panic arm is reachable from a unit test with a real
/// unwinding panic (the production body has no injectable fault), as
/// `crate::parse::ffi`'s `parse_function_panic_guard` is.
///
/// # Safety
///
/// `error_out` must point to `error_out_len` writable bytes; `position_out`
/// must be null or point to a writable `u32`.
#[cfg_attr(not(feature = "extension"), allow(dead_code))]
pub(super) unsafe fn transform_panic_guard<F>(
    error_out: *mut u8,
    error_out_len: usize,
    position_out: *mut u32,
    body: F,
) -> u8
where
    F: FnOnce() -> u8,
{
    if let Ok(rc) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        rc
    } else {
        if !position_out.is_null() {
            *position_out = u32::MAX;
        }
        crate::ffi_util::write_error_to_buffer(
            error_out,
            error_out_len,
            "semantic_views: internal error: panic inside sv_grammar_transform_rust \
             (please report this bug)",
        );
        // rc=1, an ordinary parser error, as the parse stub reports its own
        // panics. rc=2 is thrown as an InternalException, which can leave the
        // database invalidated for every later query.
        1
    }
}

#[cfg(test)]
mod panic_guard_tests {
    use super::transform_panic_guard;

    /// Drive the guard with `body`, returning `(rc, message, position)`.
    fn guarded(body: impl FnOnce() -> u8) -> (u8, String, u32) {
        let mut error_buf = vec![0_u8; 256];
        let mut position: u32 = 7;
        let rc = unsafe {
            transform_panic_guard(
                error_buf.as_mut_ptr(),
                error_buf.len(),
                &raw mut position,
                body,
            )
        };
        let nul = error_buf.iter().position(|&b| b == 0).unwrap_or(0);
        (
            rc,
            String::from_utf8_lossy(&error_buf[..nul]).into_owned(),
            position,
        )
    }

    /// A panic in a transform (a latent body-parser bug a fuzzer could find)
    /// must reach the user as an ordinary parser error (rc=1), not as rc=2,
    /// which the bridge throws as an `InternalException` that can invalidate
    /// the database.
    #[test]
    fn a_panic_is_reported_as_a_parse_error() {
        let (rc, msg, position) = guarded(|| panic!("injected"));
        assert_eq!(rc, 1, "msg={msg}");
        assert!(
            msg.contains("panic inside sv_grammar_transform_rust"),
            "got: {msg}"
        );
        assert_eq!(position, u32::MAX, "an internal failure has no caret");
    }

    /// Control: a body that returns passes its code through.
    #[test]
    fn a_returned_code_passes_through() {
        assert_eq!(guarded(|| 0).0, 0);
        assert_eq!(guarded(|| 2).0, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::changes;

    #[test]
    fn every_statement_rule_is_defined_and_prepended() {
        let text = changes();
        for rule in super::show::QUERY_STATEMENTS {
            assert!(
                text.lines()
                    .any(|l| l.starts_with(&format!("RULE\t{rule} <-")) && l.ends_with("\tquery")),
                "{rule} is not defined as a query rule"
            );
            assert!(text.contains(&format!("PREPEND\tDescribeStatement\t{rule}\n")));
        }
        for rule in super::create::ANY_TOKEN {
            let at = text.find(&format!("ANY_TOKEN\t{rule}\n")).unwrap();
            let defined = text.find(&format!("RULE\t{rule} <-")).unwrap();
            assert!(defined < at, "{rule} must be defined before its override");
        }
        for rule in super::write::STATEMENTS
            .iter()
            .chain(super::create::STATEMENTS)
        {
            assert!(
                text.lines()
                    .any(|l| l.starts_with(&format!("RULE\t{rule} <-"))
                        && l.ends_with("\tstatement")),
                "{rule} is not defined as a statement rule"
            );
            assert!(text.contains(&format!("PREPEND\tStatement\t{rule}\n")));
        }
    }

    #[test]
    fn no_rule_is_defined_twice() {
        let text = changes();
        let mut names: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("RULE\t"))
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
    }

    #[test]
    fn helper_rules_have_no_shape() {
        let text = changes();
        let line = text
            .lines()
            .find(|l| l.starts_with("RULE\tSvViewName <-"))
            .unwrap();
        assert!(line.ends_with('\t'));
    }
}
