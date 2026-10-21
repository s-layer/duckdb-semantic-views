//! The transform for the `SEMANTIC_VIEW(...)` query construct, Snowflake's
//! way of querying a semantic view in `FROM`:
//!
//! ```sql
//! SELECT * FROM SEMANTIC_VIEW(v DIMENSIONS o.region AS r METRICS o.revenue WHERE o.year = 2024) AS s
//! ```
//!
//! It lowers to the `semantic_view()` table function, which stays the one
//! implementation (decision D3): each clause's items go to its list as written,
//! aliases included (`crate::query::items` reads them), `WHERE` goes to
//! `where_clause`, and a clause order other than the default goes to
//! `column_order`, since Snowflake orders the columns by the clauses. `DuckDB`
//! stores a view's body in its lowered form, so a view over the construct
//! stores the `semantic_view()` call, which expands against the semantic view's
//! definition each time it is queried.
//!
//! A prepared statement's parameter in `WHERE` is one of the statement's
//! parameters (TECH-DEBT #105). The predicate goes to `where_clause` as text,
//! so there it becomes `$1`, `$2`, ..., and the parameter itself goes, in that
//! order, into `params := row(...)`, which `semantic_view()` reads the values
//! from. The bridge has `DuckDB` transform each parameter as it would anywhere
//! else in the statement, so it is numbered in the statement's order.

use super::tree::{Node, Tree};
use super::{internal, name_or};
use crate::errors::ParseError;
use crate::sql_lit::SqlLit;

/// The grammar rules, in `DuckDB`'s PEG syntax.
///
/// The body is a flat run of parts, which the transform groups into clauses
/// and items. An item is `<expr> [[AS] <alias>]`, and `DIMENSIONS`, `METRICS`
/// and `FACTS` are not reserved, so in a structured grammar a bare alias would
/// match the next clause's keyword (`DIMENSIONS o.region METRICS ...`), and
/// `DuckDB`'s PEG reader has no negative lookahead to stop it (it ignores `!`).
/// A clause word is tried before an expression, so a member or table named
/// like one must be quoted (`"facts".x`), as in Snowflake.
pub(super) const RULES: &str = r"
SvSemanticView <- 'SEMANTIC_VIEW' '(' SvViewName SvQueryPart* ')' SvQueryTableAlias?
SvQueryPart <- SvQueryWord / SvQueryComma / SvQueryAlias / SvQueryExpr
SvQueryWord <- 'DIMENSIONS' / 'METRICS' / 'FACTS' / 'WHERE'
SvQueryComma <- ','
SvQueryAlias <- 'AS' ColLabelOrString
SvQueryExpr <- Expression
SvQueryTableAlias <- TableAlias
";

/// The statement rules, each a table reference: an alternative of `DuckDB`'s
/// `InnerTableRef`.
pub(super) const TABLE_REFS: &[&str] = &["SvSemanticView"];

/// The clauses, in the default column order.
const CLAUSES: [&str; 3] = ["DIMENSIONS", "METRICS", "FACTS"];

/// One part of the body, classified.
enum Part<'a> {
    Word(String, &'a Node),
    Comma(&'a Node),
    Alias(&'a Node),
    Expr(&'a Node),
}

/// One clause as written: its items' text, or the predicate for `WHERE`.
struct Clause<'a> {
    word: String,
    at: &'a Node,
    items: Vec<String>,
}

/// What the bridge writes the statement's parameters as in the lowered query:
/// it puts each one, transformed by `DuckDB`, in place of these in turn
/// (`sv_grammar_finalize_tableref` in `cpp/src/shim.cpp`).
pub(crate) const PARAMETER_PLACEHOLDER: &str = "$__sv_parameter";

/// Lower a matched `SEMANTIC_VIEW(...)` to a query over `semantic_view()`.
pub(super) fn transform(tree: &Tree) -> Result<String, ParseError> {
    let mut statement = || PARAMETER_PLACEHOLDER.to_string();
    Ok(format!(
        "SELECT * FROM {}",
        lower(tree, &tree.root, &mut statement)?
    ))
}

/// The values one construct's `params` holds: each the way the text around
/// the construct refers to a parameter, in the order the predicate refers to
/// them as `$1`, `$2`, ....
struct Params<'e> {
    row: Vec<String>,
    /// How the text around the construct refers to its next parameter: the
    /// statement's placeholder, or for a construct inside another's predicate,
    /// that one's next `$n`.
    around: &'e mut dyn FnMut() -> String,
}

impl Params<'_> {
    /// Take the next value; the predicate refers to it as the result.
    fn take(&mut self) -> String {
        self.row.push((self.around)());
        format!("${}", self.row.len())
    }
}

/// The `semantic_view()` call, with its table alias, that the construct at
/// `root` lowers to. `around` gives how the text around it refers to each of
/// its parameters in turn.
fn lower(
    tree: &Tree,
    root: &Node,
    around: &mut dyn FnMut() -> String,
) -> Result<String, ParseError> {
    let mut params = Params {
        row: Vec::new(),
        around,
    };
    let name_node = root
        .find("SvViewName")
        .ok_or_else(|| internal(root, "missing view name"))?;
    let name = name_or(tree, name_node, |rest| {
        format!("expected a clause after the view name in SEMANTIC_VIEW(...), got: {rest}")
    })?;

    let parts = root
        .find_all("SvQueryPart")
        .into_iter()
        .map(|p| classify(tree, p))
        .collect::<Result<Vec<_>, _>>()?;
    let clauses = group(tree, &parts, &mut params)?;

    let mut args = vec![format!("'{}'", SqlLit::escape(&name))];
    let mut order = Vec::new();
    for clause in &clauses {
        let items = clause
            .items
            .iter()
            .map(|i| format!("'{}'", SqlLit::escape(i)))
            .collect::<Vec<_>>()
            .join(", ");
        if clause.word == "WHERE" {
            args.push(format!("where_clause := {items}"));
        } else {
            args.push(format!("{} := [{items}]", clause.word.to_ascii_lowercase()));
            order.push(clause.word.as_str());
        }
    }
    let rank = |w: &str| CLAUSES.iter().position(|c| *c == w);
    if order.windows(2).any(|w| rank(w[0]) > rank(w[1])) {
        let names = order
            .iter()
            .map(|w| format!("'{}'", w.to_ascii_lowercase()))
            .collect::<Vec<_>>()
            .join(", ");
        args.push(format!("column_order := [{names}]"));
    }
    if !params.row.is_empty() {
        args.push(format!("params := row({})", params.row.join(", ")));
    }

    let alias = table_alias(root)
        .map(|a| format!(" {}", clean(&tree.text(a))))
        .unwrap_or_default();
    Ok(format!("semantic_view({}){alias}", args.join(", ")))
}

/// What `part` is: the first of the part rules under it, in pre-order. An
/// expression can hold a construct of its own in a subquery, with words,
/// commas and aliases that are not this construct's, so only the outermost
/// rule counts.
fn classify<'a>(tree: &Tree, part: &'a Node) -> Result<Part<'a>, ParseError> {
    let node = first_of(
        part,
        &["SvQueryWord", "SvQueryComma", "SvQueryAlias", "SvQueryExpr"],
    )
    .ok_or_else(|| internal(part, "unknown part"))?;
    if node.rule.eq_ignore_ascii_case("SvQueryWord") {
        let text = tree
            .tokens_in(node)
            .next()
            .map(|t| t.text.to_ascii_uppercase())
            .ok_or_else(|| internal(node, "missing clause word"))?;
        Ok(Part::Word(text, node))
    } else if node.rule.eq_ignore_ascii_case("SvQueryComma") {
        Ok(Part::Comma(node))
    } else if node.rule.eq_ignore_ascii_case("SvQueryAlias") {
        Ok(Part::Alias(node))
    } else {
        Ok(Part::Expr(node))
    }
}

/// The construct's table alias: not one of a construct inside a part.
fn table_alias(node: &Node) -> Option<&Node> {
    if node.rule.eq_ignore_ascii_case("SvQueryTableAlias") {
        return Some(node);
    }
    if node.rule.eq_ignore_ascii_case("SvQueryPart") {
        return None;
    }
    node.children.iter().find_map(table_alias)
}

/// The first node under `node` named one of `rules`, in pre-order.
fn first_of<'a>(node: &'a Node, rules: &[&str]) -> Option<&'a Node> {
    if rules.iter().any(|r| node.rule.eq_ignore_ascii_case(r)) {
        return Some(node);
    }
    node.children.iter().find_map(|c| first_of(c, rules))
}

/// One item as written: its expression and its alias, if any (`true` for a
/// bare alias, written without `AS`).
#[derive(Clone, Copy)]
struct ItemParts<'a> {
    expr: &'a Node,
    alias: Option<(&'a Node, bool)>,
}

/// Group the parts into clauses: a clause word, then its items separated by
/// commas (each an expression with at most one alias, `AS <name>` or a bare
/// name), or for `WHERE` one predicate.
fn group<'a>(
    tree: &Tree,
    parts: &[Part<'a>],
    params: &mut Params<'_>,
) -> Result<Vec<Clause<'a>>, ParseError> {
    let mut clauses: Vec<Clause<'a>> = Vec::new();
    let mut item: Option<ItemParts<'a>> = None;
    let mut after_comma: Option<&Node> = None;

    let finish = |clauses: &mut Vec<Clause<'a>>,
                  item: &mut Option<ItemParts<'a>>,
                  after_comma: Option<&Node>,
                  next: Option<&Node>,
                  params: &mut Params<'_>|
     -> Result<(), ParseError> {
        let Some(clause) = clauses.last_mut() else {
            return Ok(());
        };
        if let Some(parts) = item.take() {
            let text = item_text(tree, &clause.word, parts, params)?;
            clause.items.push(text);
        } else {
            let at = after_comma.or(next).or(Some(clause.at));
            let message = if after_comma.is_some() {
                format!("expected an item after ',' in {}", clause.word)
            } else if clause.word == "WHERE" {
                "WHERE needs a predicate".to_string()
            } else {
                format!("{} needs at least one item", clause.word)
            };
            return Err(error_at(at, message));
        }
        Ok(())
    };

    for part in parts {
        match part {
            Part::Word(word, node) => {
                if !clauses.is_empty() {
                    finish(&mut clauses, &mut item, after_comma, Some(node), params)?;
                    after_comma = None;
                }
                if clauses.iter().any(|c| c.word == *word) {
                    return Err(error_at(
                        Some(node),
                        format!("{word} given twice in SEMANTIC_VIEW(...)"),
                    ));
                }
                clauses.push(Clause {
                    word: word.clone(),
                    at: node,
                    items: Vec::new(),
                });
            }
            Part::Comma(node) => {
                let Some(clause) = clauses.last_mut() else {
                    return Err(expected_clause(tree, node));
                };
                if clause.word == "WHERE" {
                    return Err(error_at(
                        Some(node),
                        "WHERE takes one predicate; combine conditions with AND".to_string(),
                    ));
                }
                let Some(parts) = item.take() else {
                    return Err(error_at(
                        Some(node),
                        format!("expected an item before ',' in {}", clause.word),
                    ));
                };
                let text = item_text(tree, &clause.word, parts, params)?;
                clause.items.push(text);
                after_comma = Some(node);
            }
            Part::Alias(node) | Part::Expr(node) => {
                let Some(clause) = clauses.last() else {
                    return Err(expected_clause(tree, node));
                };
                let is_alias = matches!(part, Part::Alias(_));
                item = Some(extend(tree, &clause.word, item, node, is_alias)?);
                after_comma = None;
            }
        }
    }
    finish(&mut clauses, &mut item, after_comma, None, params)?;
    Ok(clauses)
}

/// `item` with the next expression or alias `node` added: the start of an
/// item, or its alias. Anything more is a missing comma.
fn extend<'a>(
    tree: &Tree,
    word: &str,
    item: Option<ItemParts<'a>>,
    node: &'a Node,
    is_alias: bool,
) -> Result<ItemParts<'a>, ParseError> {
    match item {
        None if is_alias => Err(error_at(
            Some(node),
            format!("expected an item before AS in {word}"),
        )),
        None => Ok(ItemParts {
            expr: node,
            alias: None,
        }),
        // An item is one expression with at most one alias, and a predicate
        // one expression.
        Some(ItemParts { expr, alias: None }) if word != "WHERE" => Ok(ItemParts {
            expr,
            alias: Some((node, !is_alias)),
        }),
        Some(_) => {
            // A comma can't follow the predicate either: WHERE takes one.
            let expected = if word == "WHERE" {
                "a clause or ')' after the WHERE predicate".to_string()
            } else {
                format!("',' between the items of {word}")
            };
            Err(error_at(
                Some(node),
                format!("expected {expected}, got: {}", clean(&tree.text(node))),
            ))
        }
    }
}

/// An item's text for its `semantic_view()` list: the member, then `AS` and
/// the alias as written. A list item names its column only with `AS`
/// (`crate::query::items::parse_item`), so a bare alias gets one. The predicate
/// of `WHERE` goes as written, but for a construct in a subquery of it, which
/// is lowered too: `semantic_view()` parses `where_clause` with `DuckDB`'s own
/// parser, which does not know the construct. A parameter in it becomes a
/// reference into `params`; anywhere else in the construct it is refused.
fn item_text(
    tree: &Tree,
    word: &str,
    item: ItemParts<'_>,
    params: &mut Params<'_>,
) -> Result<String, ParseError> {
    if word == "WHERE" {
        return Ok(clean(&lower_inside(tree, item.expr, params)?));
    }
    let parts = std::iter::once(item.expr).chain(item.alias.map(|(a, _)| a));
    if let Some(at) = parts
        .flat_map(|n| inner_parts(n).into_iter())
        .find(|p| !p.1)
    {
        return Err(error_at(
            Some(at.0),
            "prepared statement parameters are only supported in the WHERE clause of \
             SEMANTIC_VIEW(...)"
                .to_string(),
        ));
    }
    let expr = clean(&tree.text(item.expr));
    let is_member = expr.ends_with('*') || {
        let refs = crate::expr_tokens::scan_references(&expr);
        matches!(refs.as_slice(), [r] if r.start == 0 && r.end == expr.len())
    };
    if !is_member {
        return Err(error_at(
            Some(item.expr),
            format!(
                "'{expr}' is not a member name; expressions in a query item are not \
                 supported yet (TECH-DEBT #76)"
            ),
        ));
    }
    Ok(match item.alias {
        None => expr,
        Some((alias, true)) => format!("{expr} AS {}", clean(&tree.text(alias))),
        Some((alias, false)) => format!("{expr} {}", clean(&tree.text(alias))),
    })
}

/// `node`'s text with each construct inside it lowered, and each parameter
/// replaced by its reference into `params`. A construct's own constructs and
/// parameters are lowered with it, and its parameters are values of `params`
/// too, so they are taken in the order they are written.
fn lower_inside(tree: &Tree, node: &Node, params: &mut Params<'_>) -> Result<String, ParseError> {
    let mut text = tree.text(node);
    let start = node.offset().unwrap_or_default();
    let mut replacements = Vec::new();
    for (inner, is_construct) in inner_parts(node) {
        let range = inner
            .span
            .and_then(|(o, l)| o.checked_sub(start).map(|at| at..at + l))
            .filter(|r| text.get(r.clone()).is_some())
            .ok_or_else(|| internal(inner, "construct outside its expression"))?;
        let lowered = if is_construct {
            lower(tree, inner, &mut || params.take())?
        } else {
            params.take()
        };
        replacements.push((range, lowered));
    }
    for (range, lowered) in replacements.into_iter().rev() {
        text.replace_range(range, &lowered);
    }
    Ok(text)
}

/// The constructs (`true`) and parameters (`false`) under `node`, in the
/// order they are written, not looking inside either.
fn inner_parts(node: &Node) -> Vec<(&Node, bool)> {
    fn collect<'a>(node: &'a Node, out: &mut Vec<(&'a Node, bool)>) {
        if node.rule.eq_ignore_ascii_case("SvSemanticView") {
            out.push((node, true));
        } else if node.rule.eq_ignore_ascii_case("Parameter") {
            out.push((node, false));
        } else {
            for c in &node.children {
                collect(c, out);
            }
        }
    }
    let mut out = Vec::new();
    for c in &node.children {
        collect(c, &mut out);
    }
    out
}

fn clean(text: &str) -> String {
    crate::util::blank_sql_comments(text).trim().to_string()
}

fn expected_clause(tree: &Tree, node: &Node) -> ParseError {
    error_at(
        Some(node),
        format!(
            "expected DIMENSIONS, METRICS, FACTS or WHERE after the view name in \
             SEMANTIC_VIEW(...), got: {}",
            clean(&tree.text(node))
        ),
    )
}

fn error_at(node: Option<&Node>, message: String) -> ParseError {
    ParseError {
        message,
        position: node.and_then(Node::offset),
    }
}

#[cfg(test)]
mod tests {
    use super::transform;
    use crate::grammar::tree::tests::n;
    use crate::grammar::tree::{Kind, Node, Source, Token, Tree};

    const L: Kind = Kind::List;

    /// The tree of `SEMANTIC_VIEW(...)` in `sql`, which spans all of it but
    /// a table alias: `name` is the view name, `parts` the body's parts in
    /// order as `(rule, text)`, each found after the one before, and `alias`
    /// the table alias.
    fn construct(sql: &str, name: &str, parts: &[(&str, &str)], alias: Option<&str>) -> Tree {
        let mut children = vec![n(sql, "SvViewName", name, L, "", vec![])];
        let mut from = sql.find(name).unwrap() + name.len();
        for (rule, text) in parts {
            let offset = from + sql[from..].find(text).unwrap();
            from = offset + text.len();
            let leaf = Node {
                rule: (*rule).to_string(),
                kind: L,
                span: Some((offset, text.len())),
                value: String::new(),
                children: Vec::new(),
            };
            children.push(Node {
                rule: "SvQueryPart".to_string(),
                kind: Kind::Choice,
                span: leaf.span,
                value: String::new(),
                children: vec![leaf],
            });
        }
        if let Some(alias) = alias {
            let offset = sql.rfind(alias).unwrap();
            children.push(Node {
                rule: "SvQueryTableAlias".to_string(),
                kind: L,
                span: Some((offset, alias.len())),
                value: String::new(),
                children: Vec::new(),
            });
        }
        let root = n(sql, "SvSemanticView", sql, L, "", children);
        Tree {
            root,
            tokens: tokens(sql),
            source: Some(Source {
                offset: 0,
                text: sql.to_string(),
            }),
        }
    }

    /// Words split on whitespace, with `.`, `(`, `)` and `,` tokens of their
    /// own.
    fn tokens(sql: &str) -> Vec<Token> {
        let mut out = Vec::new();
        let mut start = None;
        let push = |s: usize, e: usize, out: &mut Vec<Token>| {
            out.push(Token {
                offset: s,
                len: e - s,
                text: sql[s..e].to_string(),
                newline: false,
            });
        };
        for (i, c) in sql.char_indices().chain(std::iter::once((sql.len(), ' '))) {
            let single = matches!(c, '.' | '(' | ')' | ',');
            if c.is_whitespace() || single {
                if let Some(s) = start.take() {
                    push(s, i, &mut out);
                }
                if single {
                    push(i, i + 1, &mut out);
                }
            } else if start.is_none() {
                start = Some(i);
            }
        }
        out
    }

    const W: &str = "SvQueryWord";
    const E: &str = "SvQueryExpr";
    const C: &str = "SvQueryComma";
    const A: &str = "SvQueryAlias";

    #[test]
    fn clauses_lower_to_the_lists_of_semantic_view() {
        let sql = "SEMANTIC_VIEW(s.v DIMENSIONS o.region , o.city METRICS o.revenue FACTS o.qty)";
        let parts = [
            (W, "DIMENSIONS"),
            (E, "o.region"),
            (C, ","),
            (E, "o.city"),
            (W, "METRICS"),
            (E, "o.revenue"),
            (W, "FACTS"),
            (E, "o.qty"),
        ];
        assert_eq!(
            transform(&construct(sql, "s.v", &parts, None)).unwrap(),
            "SELECT * FROM semantic_view('s.v', dimensions := ['o.region', 'o.city'], \
             metrics := ['o.revenue'], facts := ['o.qty'])"
        );
    }

    /// A bare alias gets an `AS`: a list item names its column only with one.
    #[test]
    fn a_bare_alias_gets_an_as() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS o.region AS \"My R\" , o.city c)";
        let parts = [
            (W, "DIMENSIONS"),
            (E, "o.region"),
            (A, "AS \"My R\""),
            (C, ","),
            (E, "o.city"),
            (E, "c"),
        ];
        assert_eq!(
            transform(&construct(sql, "v", &parts, None)).unwrap(),
            "SELECT * FROM semantic_view('v', dimensions := ['o.region AS \"My R\"', 'o.city AS c'])"
        );
    }

    #[test]
    fn a_clause_order_other_than_the_default_sets_the_column_order() {
        let sql = "SEMANTIC_VIEW(v METRICS m DIMENSIONS d)";
        let parts = [(W, "METRICS"), (E, "m"), (W, "DIMENSIONS"), (E, "d")];
        assert_eq!(
            transform(&construct(sql, "v", &parts, None)).unwrap(),
            "SELECT * FROM semantic_view('v', metrics := ['m'], dimensions := ['d'], \
             column_order := ['metrics', 'dimensions'])"
        );
    }

    #[test]
    fn the_default_order_with_a_clause_missing_needs_no_column_order() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS d FACTS f)";
        let parts = [(W, "DIMENSIONS"), (E, "d"), (W, "FACTS"), (E, "f")];
        assert_eq!(
            transform(&construct(sql, "v", &parts, None)).unwrap(),
            "SELECT * FROM semantic_view('v', dimensions := ['d'], facts := ['f'])"
        );
    }

    #[test]
    fn where_lowers_to_where_clause_and_quotes_are_escaped() {
        let sql = "SEMANTIC_VIEW(v METRICS m WHERE o.region = 'EU' /* c */ AND x > 1)";
        let parts = [
            (W, "METRICS"),
            (E, "m"),
            (W, "WHERE"),
            (E, "o.region = 'EU' /* c */ AND x > 1"),
        ];
        assert_eq!(
            transform(&construct(sql, "v", &parts, None)).unwrap(),
            "SELECT * FROM semantic_view('v', metrics := ['m'], \
             where_clause := 'o.region = ''EU''         AND x > 1')"
        );
    }

    #[test]
    fn the_table_alias_follows_the_call() {
        let sql = "SEMANTIC_VIEW(v METRICS m) AS s(total)";
        let parts = [(W, "METRICS"), (E, "m")];
        let t = construct(sql, "v", &parts, Some("AS s(total)"));
        assert_eq!(
            transform(&t).unwrap(),
            "SELECT * FROM semantic_view('v', metrics := ['m']) AS s(total)"
        );
    }

    fn error(sql: &str, parts: &[(&str, &str)]) -> (String, Option<usize>) {
        let e = transform(&construct(sql, "v", parts, None)).unwrap_err();
        (e.message, e.position)
    }

    #[test]
    fn a_clause_given_twice_is_an_error_at_the_second() {
        let sql = "SEMANTIC_VIEW(v METRICS a METRICS b)";
        let (message, at) = error(sql, &[(W, "METRICS"), (E, "a"), (W, "METRICS"), (E, "b")]);
        assert_eq!(message, "METRICS given twice in SEMANTIC_VIEW(...)");
        assert_eq!(at, Some(26));
    }

    #[test]
    fn a_clause_without_items_is_an_error() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS METRICS m)";
        let (message, at) = error(sql, &[(W, "DIMENSIONS"), (W, "METRICS"), (E, "m")]);
        assert_eq!(message, "DIMENSIONS needs at least one item");
        assert_eq!(at, Some(27));
    }

    #[test]
    fn a_trailing_comma_is_an_error() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS d , METRICS m)";
        let (message, _) = error(
            sql,
            &[
                (W, "DIMENSIONS"),
                (E, "d"),
                (C, ","),
                (W, "METRICS"),
                (E, "m"),
            ],
        );
        assert_eq!(message, "expected an item after ',' in DIMENSIONS");
    }

    #[test]
    fn a_leading_comma_is_an_error() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS , d)";
        let (message, _) = error(sql, &[(W, "DIMENSIONS"), (C, ","), (E, "d")]);
        assert_eq!(message, "expected an item before ',' in DIMENSIONS");
    }

    #[test]
    fn a_missing_comma_is_an_error_at_the_third_part() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS a b c)";
        let (message, at) = error(sql, &[(W, "DIMENSIONS"), (E, "a"), (E, "b"), (E, "c")]);
        assert_eq!(
            message,
            "expected ',' between the items of DIMENSIONS, got: c"
        );
        assert_eq!(at, Some(31));
    }

    #[test]
    fn an_expression_item_is_not_supported_yet() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS upper(o.region) AS r)";
        let (message, at) = error(
            sql,
            &[(W, "DIMENSIONS"), (E, "upper(o.region)"), (A, "AS r")],
        );
        assert_eq!(
            message,
            "'upper(o.region)' is not a member name; expressions in a query item are not \
             supported yet (TECH-DEBT #76)"
        );
        assert_eq!(at, Some(27));
    }

    /// An item ending in `*` is a wildcard only as `<table>.*`.
    #[test]
    fn a_call_ending_in_a_star_is_an_expression() {
        let sql = "SEMANTIC_VIEW(v METRICS count(*))";
        let (message, _) = error(sql, &[(W, "METRICS"), (E, "count(*)")]);
        assert!(message.contains("TECH-DEBT #76"), "{message}");
    }

    #[test]
    fn a_wildcard_is_a_member_item() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS o.*)";
        let parts = [(W, "DIMENSIONS"), (E, "o.*")];
        assert_eq!(
            transform(&construct(sql, "v", &parts, None)).unwrap(),
            "SELECT * FROM semantic_view('v', dimensions := ['o.*'])"
        );
    }

    #[test]
    fn an_alias_without_an_item_is_an_error() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS AS r)";
        let (message, _) = error(sql, &[(W, "DIMENSIONS"), (A, "AS r")]);
        assert_eq!(message, "expected an item before AS in DIMENSIONS");
    }

    #[test]
    fn an_item_before_any_clause_is_an_error() {
        let sql = "SEMANTIC_VIEW(v o.region)";
        let (message, _) = error(sql, &[(E, "o.region")]);
        assert_eq!(
            message,
            "expected DIMENSIONS, METRICS, FACTS or WHERE after the view name in \
             SEMANTIC_VIEW(...), got: o.region"
        );
    }

    #[test]
    fn where_takes_one_predicate() {
        let sql = "SEMANTIC_VIEW(v METRICS m WHERE a = 1 , b = 2)";
        let (message, _) = error(
            sql,
            &[
                (W, "METRICS"),
                (E, "m"),
                (W, "WHERE"),
                (E, "a = 1"),
                (C, ","),
                (E, "b = 2"),
            ],
        );
        assert_eq!(
            message,
            "WHERE takes one predicate; combine conditions with AND"
        );

        let sql = "SEMANTIC_VIEW(v METRICS m WHERE a = 1 b)";
        let (message, _) = error(
            sql,
            &[
                (W, "METRICS"),
                (E, "m"),
                (W, "WHERE"),
                (E, "a = 1"),
                (E, "b"),
            ],
        );
        assert_eq!(
            message,
            "expected a clause or ')' after the WHERE predicate, got: b"
        );

        let sql = "SEMANTIC_VIEW(v METRICS m WHERE)";
        let (message, _) = error(sql, &[(W, "METRICS"), (E, "m"), (W, "WHERE")]);
        assert_eq!(message, "WHERE needs a predicate");
    }

    /// The tree of `sql`, an outer construct over `parts` (with the table alias
    /// `alias`) whose part holding `inner` has the inner construct's tree, over
    /// `inner_parts` and `inner_alias`, under it, as a subquery in an
    /// expression has.
    fn nested(
        sql: &str,
        parts: &[(&str, &str)],
        alias: Option<&str>,
        inner: &str,
        inner_parts: &[(&str, &str)],
        inner_alias: Option<&str>,
    ) -> Tree {
        fn shift(node: &mut Node, by: usize) {
            node.span = node.span.map(|(o, l)| (o + by, l));
            node.children.iter_mut().for_each(|c| shift(c, by));
        }
        let mut tree = construct(sql, "v", parts, alias);
        let at = sql.find(inner).unwrap();
        let mut inner_root = construct(inner, "v", inner_parts, inner_alias).root;
        shift(&mut inner_root, at);
        let holder = tree
            .root
            .children
            .iter_mut()
            .find(|c| {
                c.span
                    .is_some_and(|(o, l)| o <= at && at + inner.len() <= o + l)
            })
            .unwrap();
        holder.children[0].children.push(inner_root);
        tree
    }

    const NESTED: &str = "SEMANTIC_VIEW(v METRICS m WHERE d IN (SELECT r FROM \
                          SEMANTIC_VIEW(v DIMENSIONS d AS r METRICS m) AS z WHERE m < 100)) AS s";
    const INNER: &str = "SEMANTIC_VIEW(v DIMENSIONS d AS r METRICS m) AS z";

    fn nested_tree() -> Tree {
        nested(
            NESTED,
            &[
                (W, "METRICS"),
                (E, "m"),
                (W, "WHERE"),
                (E, "d IN (SELECT r FROM SEMANTIC_VIEW(v DIMENSIONS d AS r METRICS m) AS z WHERE m < 100)"),
            ],
            Some("AS s"),
            INNER,
            &[(W, "DIMENSIONS"), (E, "d"), (A, "AS r"), (W, "METRICS"), (E, "m")],
            Some("AS z"),
        )
    }

    /// A construct in a subquery of `WHERE` leaves the predicate a predicate:
    /// its clause words are its own.
    #[test]
    fn a_construct_inside_where_is_part_of_the_predicate() {
        let t = nested_tree();
        assert!(
            transform(&t).is_ok(),
            "{:?}",
            transform(&t).err().map(|e| e.message)
        );
    }

    /// The table alias is the outer construct's, not one inside it.
    #[test]
    fn the_table_alias_is_not_taken_from_a_construct_inside() {
        let lowered = transform(&nested_tree()).unwrap_or_else(|e| e.message);
        assert!(lowered.ends_with(") AS s"), "{lowered}");
    }

    /// A construct inside the predicate is lowered too: `where_clause` is
    /// expanded with `DuckDB`'s own parser, which does not know the construct.
    #[test]
    fn a_construct_inside_where_is_lowered() {
        assert_eq!(
            transform(&nested_tree()).unwrap(),
            "SELECT * FROM semantic_view('v', metrics := ['m'], where_clause := \
             'd IN (SELECT r FROM semantic_view(''v'', dimensions := [''d AS r''], \
             metrics := [''m'']) AS z WHERE m < 100)') AS s"
        );
    }

    /// `tree` with a `Parameter` node for each of `params`, found in `sql` in
    /// turn, under the innermost node it lies in, as `DuckDB`'s parse has it.
    fn with_parameters(mut tree: Tree, sql: &str, params: &[&str]) -> Tree {
        fn place(node: &mut Node, param: Node) {
            let (at, len) = param.span.unwrap();
            let inside = |n: &Node| n.span.is_some_and(|(o, l)| o <= at && at + len <= o + l);
            if let Some(child) = node.children.iter_mut().find(|c| inside(c)) {
                place(child, param);
            } else {
                node.children.push(param);
                node.children.sort_by_key(|c| c.span.map(|(o, _)| o));
            }
        }
        let mut from = 0;
        for p in params {
            let at = from + sql[from..].find(p).unwrap();
            from = at + p.len();
            let param = Node {
                rule: "Parameter".to_string(),
                kind: Kind::Choice,
                span: Some((at, p.len())),
                value: String::new(),
                children: Vec::new(),
            };
            place(&mut tree.root, param);
        }
        tree
    }

    /// A parameter in `WHERE` becomes `$n` in `where_clause`, and the
    /// statement's parameter goes into `params` in the same order, as the
    /// placeholder the bridge replaces (TECH-DEBT #105).
    #[test]
    fn a_parameter_in_where_goes_into_params() {
        let sql = "SEMANTIC_VIEW(v METRICS m WHERE x = ? AND y > $name OR z = $3)";
        let parts = [
            (W, "METRICS"),
            (E, "m"),
            (W, "WHERE"),
            (E, "x = ? AND y > $name OR z = $3"),
        ];
        let tree = with_parameters(
            construct(sql, "v", &parts, None),
            sql,
            &["?", "$name", "$3"],
        );
        let p = super::PARAMETER_PLACEHOLDER;
        assert_eq!(
            transform(&tree).unwrap(),
            format!(
                "SELECT * FROM semantic_view('v', metrics := ['m'], where_clause := \
                 'x = $1 AND y > $2 OR z = $3', params := row({p}, {p}, {p}))"
            )
        );
    }

    /// A parameter in a construct inside the predicate is a value of the outer
    /// construct's `params` too: the inner one's `params` refers to it there.
    #[test]
    fn a_parameter_in_a_construct_inside_where_goes_into_both_params() {
        let sql = "SEMANTIC_VIEW(v METRICS m WHERE x = ? AND d IN (SELECT r FROM \
                   SEMANTIC_VIEW(v DIMENSIONS d METRICS m WHERE y = $p) AS z) AND w = ?) AS s";
        let inner = "SEMANTIC_VIEW(v DIMENSIONS d METRICS m WHERE y = $p) AS z";
        let tree = nested(
            sql,
            &[
                (W, "METRICS"),
                (E, "m"),
                (W, "WHERE"),
                (
                    E,
                    "x = ? AND d IN (SELECT r FROM SEMANTIC_VIEW(v DIMENSIONS d METRICS m \
                     WHERE y = $p) AS z) AND w = ?",
                ),
            ],
            Some("AS s"),
            inner,
            &[
                (W, "DIMENSIONS"),
                (E, "d"),
                (W, "METRICS"),
                (E, "m"),
                (W, "WHERE"),
                (E, "y = $p"),
            ],
            Some("AS z"),
        );
        let tree = with_parameters(tree, sql, &["?", "$p", "?"]);
        let p = super::PARAMETER_PLACEHOLDER;
        assert_eq!(
            transform(&tree).unwrap(),
            format!(
                "SELECT * FROM semantic_view('v', metrics := ['m'], where_clause := \
                 'x = $1 AND d IN (SELECT r FROM semantic_view(''v'', dimensions := [''d''], \
                 metrics := [''m''], where_clause := ''y = $1'', params := row($2)) AS z) \
                 AND w = $3', params := row({p}, {p}, {p})) AS s"
            )
        );
    }

    /// An item names a member, so a parameter there has nothing to bind to.
    #[test]
    fn a_parameter_outside_where_is_refused() {
        let sql = "SEMANTIC_VIEW(v DIMENSIONS ? METRICS m)";
        let parts = [(W, "DIMENSIONS"), (E, "?"), (W, "METRICS"), (E, "m")];
        let tree = with_parameters(construct(sql, "v", &parts, None), sql, &["?"]);
        let e = transform(&tree).unwrap_err();
        assert!(
            e.message.contains("only supported in the WHERE clause"),
            "{}",
            e.message
        );
        assert_eq!(e.position, sql.find('?'));
    }

    #[test]
    fn a_dollar_quoted_string_is_not_a_parameter() {
        let sql = "SEMANTIC_VIEW(v METRICS m WHERE x = $$a$$)";
        let parts = [(W, "METRICS"), (E, "m"), (W, "WHERE"), (E, "x = $$a$$")];
        assert!(transform(&construct(sql, "v", &parts, None)).is_ok());
    }
}
