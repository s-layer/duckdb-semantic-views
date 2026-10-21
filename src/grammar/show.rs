//! Transforms for the read statements: `SHOW SEMANTIC ...`, `SHOW COLUMNS IN
//! SEMANTIC VIEW` and `DESCRIBE SEMANTIC VIEW`.
//!
//! Each lowers to the same query the parser override emits for it
//! ([`lower_show`], [`lower_name_only_read`]), so the two paths cannot drift
//! while both exist. Each is a query, so it can stand wherever `DuckDB`'s
//! grammar accepts one: a subquery, a CTE, a view body (TECH-DEBT #77).

use super::tree::{Kind, Node, Tree};
use super::{internal, name_or, plain_string};
use crate::errors::ParseError;
use crate::parse::messages::unexpected_after_view_name;
use crate::parse::{
    lower_name_only_read, lower_show, show_unexpected_tokens, DdlKind, ScopeName, ShowClauses,
};

/// The grammar rules for the read statements, in `DuckDB`'s PEG syntax.
///
/// Two shapes here work around what that syntax lacks:
///
/// - The scope name after `IN SCHEMA` / `IN DATABASE` is optional, so the
///   word after it may be the next clause instead. `LIMIT` and `FOR` are
///   reserved words, which a name cannot be, but `STARTS` is not, and the
///   grammar has no negative lookahead (`DuckDB`'s PEG reader ignores `!`). So
///   the bare scope followed by `STARTS WITH` is its own alternative, tried
///   first; the transform finds the `STARTS WITH` clause wherever it sits.
/// - `LIMIT` takes any single value, so a non-integer is our error with our
///   wording rather than a `LIMIT` modifier on the query `DuckDB` would bind.
/// - `ACCOUNT`, `SCHEMA` and `DATABASE` are not reserved, so each can be a
///   schema's name: `IN account.v` names a view, as it does for the override
///   (`in_clause_is_scope` wants a space after the keyword). An ordered choice
///   commits to the first alternative that matches, and the scope keyword alone
///   would match, leaving `.v` unparsed, so a dotted view name is tried first.
pub(super) const RULES: &str = r"
SvShowStatement <- 'SHOW' SvShowWhat SvShowLike? SvShowIn? SvShowForMetric? SvShowStartsWith? SvShowLimit?
SvShowWhat <- SvShowTerse / SvShowViews / SvShowDimensions / SvShowMetrics / SvShowFacts / SvShowMaterializations
SvShowTerse <- 'TERSE' 'SEMANTIC' 'VIEWS'
SvShowViews <- 'SEMANTIC' 'VIEWS'
SvShowDimensions <- 'SEMANTIC' 'DIMENSIONS'
SvShowMetrics <- 'SEMANTIC' 'METRICS'
SvShowFacts <- 'SEMANTIC' 'FACTS'
SvShowMaterializations <- 'SEMANTIC' 'MATERIALIZATIONS'
SvShowLike <- 'LIKE' StringLiteral
SvShowIn <- 'IN' SvShowScope
SvShowScope <- SvShowInDottedView / SvShowInAccount / SvShowInSchema / SvShowInDatabase / SvShowInView
SvShowInDottedView <- SvNamePart '.' SvViewName
SvShowInAccount <- 'ACCOUNT'
SvShowInSchema <- 'SCHEMA' SvShowStartsWith / 'SCHEMA' SvScopeName?
SvShowInDatabase <- 'DATABASE' SvShowStartsWith / 'DATABASE' SvScopeName?
SvScopeName <- SvNamePart ('.' SvNamePart)*
SvShowInView <- SvViewName
SvShowForMetric <- 'FOR' 'METRIC' SvMemberName
SvShowStartsWith <- 'STARTS' 'WITH' StringLiteral
SvShowLimit <- 'LIMIT' SvShowLimitValue
SvShowLimitValue <- NumberLiteral / '-' NumberLiteral / ColId / StringLiteral
SvShowColumns <- 'SHOW' 'COLUMNS' 'IN' 'SEMANTIC' 'VIEW' SvViewName
SvDescribe <- SvDescribeWord 'SEMANTIC' 'VIEW' SvViewName
SvDescribeWord <- 'DESCRIBE' / 'DESC'
SvViewName <- SvNamePart ('.' SvNamePart)*
SvMemberName <- SvNamePart ('.' SvNamePart)*
SvNamePart <- ColId
";

/// The statement rules, each a query alternative of `DescribeStatement`.
pub(super) const QUERY_STATEMENTS: &[&str] = &["SvShowStatement", "SvShowColumns", "SvDescribe"];

/// Lower one matched read statement to SQL.
pub(super) fn transform(rule: &str, tree: &Tree) -> Result<String, ParseError> {
    let root = &tree.root;
    if rule.eq_ignore_ascii_case("SvDescribe") {
        let name = required(tree, root, "SvViewName")?;
        return Ok(lower_name_only_read(DdlKind::Describe, &name));
    }
    if rule.eq_ignore_ascii_case("SvShowColumns") {
        let name = required(tree, root, "SvViewName")?;
        return Ok(lower_name_only_read(DdlKind::ShowColumns, &name));
    }
    transform_show(tree)
}

fn transform_show(tree: &Tree) -> Result<String, ParseError> {
    let root = &tree.root;
    let kind = show_kind(root)?;
    let entity = match kind {
        DdlKind::Show | DdlKind::ShowTerse => "VIEWS",
        DdlKind::ShowDimensions => "DIMENSIONS",
        DdlKind::ShowMetrics => "METRICS",
        DdlKind::ShowMaterializations => "MATERIALIZATIONS",
        _ => "FACTS",
    };

    // The clauses are read in the order the parser override reads them, so a
    // statement with two faults gets the override's message for the first.
    let like_pattern = root
        .find("SvShowLike")
        .map(|like| plain_string(tree, like, QUOTED))
        .transpose()?;

    let (in_view, in_schema, in_database) = match root.find("SvShowScope") {
        Some(scope) => show_scope(tree, scope, kind)?,
        None => (None, None, None),
    };

    let mut for_metric = None;
    if let Some(metric) = root.find("SvShowForMetric") {
        if kind != DdlKind::ShowDimensions {
            return Err(ParseError {
                message: format!(
                    "FOR METRIC is only valid for SHOW SEMANTIC DIMENSIONS, not SHOW SEMANTIC {entity}"
                ),
                position: metric.offset(),
            });
        }
        let name = metric.find("SvMemberName").unwrap_or(metric);
        for_metric = Some(show_name(tree, name, kind)?);
    }

    let starts_with_clauses = root.find_all("SvShowStartsWith");
    if let Some(second) = starts_with_clauses.get(1) {
        return Err(ParseError {
            message: "STARTS WITH given twice".to_string(),
            position: second.offset(),
        });
    }
    let starts_with = starts_with_clauses
        .first()
        .map(|clause| plain_string(tree, clause, QUOTED))
        .transpose()?;

    let mut limit = None;
    if let Some(node) = root.find("SvShowLimit") {
        let token: String = node
            .find("SvShowLimitValue")
            .map(|v| tree.tokens_in(v).map(|t| t.text.as_str()).collect())
            .unwrap_or_default();
        let n: u64 = token.parse().map_err(|_| ParseError {
            message: format!("LIMIT must be a non-negative integer, got: '{token}'"),
            position: node.offset(),
        })?;
        limit = Some(n);
    }

    let clauses = ShowClauses {
        like_pattern,
        in_view: in_view.as_deref(),
        in_schema,
        in_database,
        for_metric: for_metric.as_deref(),
        starts_with,
        limit,
    };
    lower_show(kind, &clauses, root.offset().unwrap_or(0))
}

/// The parser override's message for a pattern that is not a `'...'` literal.
const QUOTED: &str = "Expected single-quoted string";

/// A name in a SHOW clause, which must be one run of text: the override ends
/// it at whitespace and reports the rest of the statement with its usage
/// message.
fn show_name(tree: &Tree, node: &Node, kind: DdlKind) -> Result<String, ParseError> {
    name_or(tree, node, |rest| show_unexpected_tokens(kind, rest))
}

/// What an `IN` clause names: a view, or a schema and / or database scope.
/// `IN ACCOUNT` names nothing: it is an unfiltered listing.
type Scope = (Option<String>, Option<ScopeName>, Option<ScopeName>);

fn show_scope(tree: &Tree, scope: &Node, kind: DdlKind) -> Result<Scope, ParseError> {
    if let Some(view) = scope
        .find("SvShowInDottedView")
        .or_else(|| scope.find("SvShowInView"))
    {
        // VIEWS has no view-name slot: there `IN` is always a scope.
        if matches!(kind, DdlKind::Show | DdlKind::ShowTerse) {
            return Err(ParseError {
                message: "SHOW SEMANTIC VIEWS requires a scope: IN {ACCOUNT | DATABASE [db] | SCHEMA [[db.]schema]}"
                    .to_string(),
                position: view.offset(),
            });
        }
        return Ok((Some(show_name(tree, view, kind)?), None, None));
    }
    if let Some(schema) = scope.find("SvShowInSchema") {
        let Some(name) = schema.find("SvScopeName") else {
            return Ok((None, Some(ScopeName::Current), None));
        };
        let text = show_name(tree, name, kind)?;
        let mut parts = name_parts(name);
        return match parts.len() {
            1 => Ok((None, parts.pop().map(ScopeName::Named), None)),
            // The schema is the last part; the database qualifies it.
            2 => {
                let schema = parts.pop().map(ScopeName::Named);
                Ok((None, schema, parts.pop().map(ScopeName::Named)))
            }
            _ => Err(ParseError {
                message: format!(
                    "Invalid schema name '{text}': expected <schema> or <database>.<schema>"
                ),
                position: name.offset(),
            }),
        };
    }
    if let Some(database) = scope.find("SvShowInDatabase") {
        let Some(name) = database.find("SvScopeName") else {
            return Ok((None, None, Some(ScopeName::Current)));
        };
        let text = show_name(tree, name, kind)?;
        let mut parts = name_parts(name);
        if parts.len() != 1 {
            return Err(ParseError {
                message: format!("Invalid database name '{text}': expected a single <database>"),
                position: name.offset(),
            });
        }
        return Ok((None, None, parts.pop().map(ScopeName::Named)));
    }
    Ok((None, None, None))
}

fn show_kind(root: &Node) -> Result<DdlKind, ParseError> {
    let kinds = [
        ("SvShowTerse", DdlKind::ShowTerse),
        ("SvShowViews", DdlKind::Show),
        ("SvShowDimensions", DdlKind::ShowDimensions),
        ("SvShowMetrics", DdlKind::ShowMetrics),
        ("SvShowFacts", DdlKind::ShowFacts),
        ("SvShowMaterializations", DdlKind::ShowMaterializations),
    ];
    kinds
        .iter()
        .find(|(rule, _)| root.find(rule).is_some())
        .map(|(_, kind)| *kind)
        .ok_or_else(|| internal(root, "SHOW statement without an object kind"))
}

/// The parts of a dotted name, each unquoted.
fn name_parts(node: &Node) -> Vec<String> {
    node.find_all("SvNamePart")
        .into_iter()
        .map(|part| {
            part.first_leaf(Kind::Identifier)
                .or_else(|| part.first_leaf(Kind::Keyword))
                .map(|leaf| leaf.value.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// The view name of `DESCRIBE` / `SHOW COLUMNS`, reported as the override
/// reports a name with whitespace inside it.
fn required(tree: &Tree, root: &Node, rule: &str) -> Result<String, ParseError> {
    let node = root
        .find(rule)
        .ok_or_else(|| internal(root, &format!("missing {rule}")))?;
    name_or(tree, node, unexpected_after_view_name)
}

#[cfg(test)]
mod tests {
    use super::transform;
    use crate::grammar::tree::tests::{n, root, tree};
    use crate::grammar::tree::{Kind, Node};

    fn at(rule: &str, offset: usize, len: usize, kind: Kind, value: &str) -> Node {
        Node {
            rule: rule.to_string(),
            kind,
            span: Some((offset, len)),
            value: value.to_string(),
            children: Vec::new(),
        }
    }

    const L: Kind = Kind::List;

    #[test]
    fn show_dimensions_for_metric_lowers_to_the_metric_function() {
        let sql = "SHOW SEMANTIC DIMENSIONS IN v FOR METRIC m LIMIT 3";
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(
                    sql,
                    "SvShowDimensions",
                    "SEMANTIC DIMENSIONS",
                    L,
                    "",
                    vec![],
                ),
                n(
                    sql,
                    "SvShowScope",
                    "v",
                    L,
                    "",
                    vec![n(sql, "SvShowInView", "v", L, "", vec![])],
                ),
                n(
                    sql,
                    "SvShowForMetric",
                    "FOR METRIC m",
                    L,
                    "",
                    vec![n(sql, "SvMemberName", " m", L, "", vec![])],
                ),
                n(
                    sql,
                    "SvShowLimit",
                    "LIMIT 3",
                    L,
                    "",
                    vec![n(sql, "SvShowLimitValue", "3", Kind::Number, "3", vec![])],
                ),
            ],
        );
        assert_eq!(
            transform("SvShowStatement", &tree(sql, r)).unwrap(),
            "SELECT * FROM show_semantic_dimensions_for_metric('v', 'm') LIMIT 3"
        );
    }

    #[test]
    fn show_views_in_a_view_name_is_an_error() {
        let sql = "SHOW SEMANTIC VIEWS IN v";
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(sql, "SvShowViews", "SEMANTIC VIEWS", L, "", vec![]),
                n(
                    sql,
                    "SvShowScope",
                    " v",
                    L,
                    "",
                    vec![n(sql, "SvShowInView", " v", L, "", vec![])],
                ),
            ],
        );
        let err = transform("SvShowStatement", &tree(sql, r)).unwrap_err();
        assert!(
            err.message
                .starts_with("SHOW SEMANTIC VIEWS requires a scope"),
            "{}",
            err.message
        );
        assert_eq!(err.position, sql.find(" v"));
    }

    #[test]
    fn for_metric_on_another_object_kind_is_an_error() {
        let sql = "SHOW SEMANTIC METRICS IN v FOR METRIC m";
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(sql, "SvShowMetrics", "SEMANTIC METRICS", L, "", vec![]),
                n(sql, "SvShowForMetric", "FOR METRIC m", L, "", vec![]),
            ],
        );
        let err = transform("SvShowStatement", &tree(sql, r)).unwrap_err();
        assert_eq!(
            err.message,
            "FOR METRIC is only valid for SHOW SEMANTIC DIMENSIONS, not SHOW SEMANTIC METRICS"
        );
        assert_eq!(err.position, sql.find("FOR"));
    }

    #[test]
    fn for_metric_without_a_view_is_an_error() {
        let sql = "SHOW SEMANTIC DIMENSIONS FOR METRIC m";
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(
                    sql,
                    "SvShowDimensions",
                    "SEMANTIC DIMENSIONS",
                    L,
                    "",
                    vec![],
                ),
                n(
                    sql,
                    "SvShowForMetric",
                    "FOR METRIC m",
                    L,
                    "",
                    vec![n(sql, "SvMemberName", " m", L, "", vec![])],
                ),
            ],
        );
        let err = transform("SvShowStatement", &tree(sql, r)).unwrap_err();
        assert_eq!(err.message, "FOR METRIC requires IN view_name");
    }

    #[test]
    fn a_metric_name_with_whitespace_inside_gets_the_usage_message() {
        let sql = "SHOW SEMANTIC DIMENSIONS IN v FOR METRIC o . m LIMIT 2";
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(
                    sql,
                    "SvShowDimensions",
                    "SEMANTIC DIMENSIONS",
                    L,
                    "",
                    vec![],
                ),
                n(
                    sql,
                    "SvShowScope",
                    " v ",
                    L,
                    "",
                    vec![n(sql, "SvShowInView", " v ", L, "", vec![])],
                ),
                n(
                    sql,
                    "SvShowForMetric",
                    "FOR METRIC o . m",
                    L,
                    "",
                    vec![n(sql, "SvMemberName", "o . m", L, "", vec![])],
                ),
            ],
        );
        let err = transform("SvShowStatement", &tree(sql, r)).unwrap_err();
        assert_eq!(
            err.message,
            crate::parse::show_unexpected_tokens(
                crate::parse::DdlKind::ShowDimensions,
                ". m LIMIT 2"
            )
        );
        assert_eq!(err.position, Some(sql.find(" . ").unwrap() + 1));
    }

    #[test]
    fn a_pattern_that_is_not_a_plain_literal_is_refused() {
        for literal in ["$$v%$$", "E'v%'"] {
            let sql = format!("SHOW SEMANTIC VIEWS LIKE {literal}");
            let sql = sql.as_str();
            let lit = n(sql, "StringLiteral", literal, Kind::String, "v%", vec![]);
            let like = format!("LIKE {literal}");
            let r = root(
                sql,
                "SvShowStatement",
                vec![
                    n(sql, "SvShowViews", "SEMANTIC VIEWS", L, "", vec![]),
                    n(sql, "SvShowLike", &like, L, "", vec![lit]),
                ],
            );
            let err = transform("SvShowStatement", &tree(sql, r)).unwrap_err();
            assert_eq!(err.message, "Expected single-quoted string", "{literal}");
            assert_eq!(err.position, Some(sql.find(literal).unwrap()), "{literal}");
        }
    }

    #[test]
    fn a_negative_limit_is_an_error_with_the_value_as_written() {
        let sql = "SHOW SEMANTIC VIEWS LIMIT -1";
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(sql, "SvShowViews", "SEMANTIC VIEWS", L, "", vec![]),
                n(
                    sql,
                    "SvShowLimit",
                    "LIMIT -1",
                    L,
                    "",
                    vec![n(sql, "SvShowLimitValue", "-1", L, "", vec![])],
                ),
            ],
        );
        let err = transform("SvShowStatement", &tree(sql, r)).unwrap_err();
        assert_eq!(
            err.message,
            "LIMIT must be a non-negative integer, got: '-1'"
        );
    }

    #[test]
    fn starts_with_twice_is_an_error() {
        let sql = "SHOW SEMANTIC VIEWS IN SCHEMA STARTS WITH 'a' STARTS WITH 'b'";
        let second = sql.rfind("STARTS").unwrap();
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(sql, "SvShowViews", "SEMANTIC VIEWS", L, "", vec![]),
                n(
                    sql,
                    "SvShowScope",
                    "SCHEMA STARTS WITH 'a'",
                    L,
                    "",
                    vec![n(
                        sql,
                        "SvShowInSchema",
                        "SCHEMA STARTS WITH 'a'",
                        L,
                        "",
                        vec![n(
                            sql,
                            "SvShowStartsWith",
                            "STARTS WITH 'a'",
                            L,
                            "",
                            vec![n(sql, "", "'a'", Kind::String, "a", vec![])],
                        )],
                    )],
                ),
                Node {
                    children: vec![at("", second + 12, 3, Kind::String, "b")],
                    ..at("SvShowStartsWith", second, 15, L, "")
                },
            ],
        );
        let err = transform("SvShowStatement", &tree(sql, r)).unwrap_err();
        assert_eq!(err.message, "STARTS WITH given twice");
        assert_eq!(err.position, Some(second));
    }

    #[test]
    fn a_bare_schema_scope_then_starts_with_keeps_both() {
        let sql = "SHOW SEMANTIC VIEWS IN SCHEMA STARTS WITH 'a'";
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(sql, "SvShowViews", "SEMANTIC VIEWS", L, "", vec![]),
                n(
                    sql,
                    "SvShowScope",
                    "SCHEMA STARTS WITH 'a'",
                    L,
                    "",
                    vec![n(
                        sql,
                        "SvShowInSchema",
                        "SCHEMA STARTS WITH 'a'",
                        L,
                        "",
                        vec![n(
                            sql,
                            "SvShowStartsWith",
                            "STARTS WITH 'a'",
                            L,
                            "",
                            vec![n(sql, "", "'a'", Kind::String, "a", vec![])],
                        )],
                    )],
                ),
            ],
        );
        assert_eq!(
            transform("SvShowStatement", &tree(sql, r)).unwrap(),
            "SELECT * FROM list_semantic_views() WHERE starts_with(name, 'a') \
             AND lower(schema_name) = lower(current_schema())"
        );
    }

    #[test]
    fn a_two_part_schema_scope_filters_on_database_and_schema() {
        let sql = "SHOW SEMANTIC VIEWS IN SCHEMA db.s";
        let s_at = sql.rfind('s').unwrap();
        let part = |offset: usize, name: &str| Node {
            children: vec![at("", offset, name.len(), Kind::Identifier, name)],
            ..at("SvNamePart", offset, name.len(), L, "")
        };
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(sql, "SvShowViews", "SEMANTIC VIEWS", L, "", vec![]),
                n(
                    sql,
                    "SvShowScope",
                    "SCHEMA db.s",
                    L,
                    "",
                    vec![n(
                        sql,
                        "SvShowInSchema",
                        "SCHEMA db.s",
                        L,
                        "",
                        vec![n(
                            sql,
                            "SvScopeName",
                            "db.s",
                            L,
                            "",
                            vec![part(sql.find("db").unwrap(), "db"), part(s_at, "s")],
                        )],
                    )],
                ),
            ],
        );
        assert_eq!(
            transform("SvShowStatement", &tree(sql, r)).unwrap(),
            "SELECT * FROM list_semantic_views() WHERE lower(schema_name) = lower('s') \
             AND lower(database_name) = lower('db')"
        );
    }

    #[test]
    fn a_three_part_schema_scope_is_an_error() {
        let sql = "SHOW SEMANTIC VIEWS IN SCHEMA a.b.c";
        let part = |name: &str| Node {
            children: vec![n(
                sql,
                "",
                &format!(".{name}"),
                Kind::Identifier,
                name,
                vec![],
            )],
            ..n(sql, "SvNamePart", &format!(".{name}"), L, "", vec![])
        };
        let first = Node {
            children: vec![n(sql, "", " a", Kind::Identifier, "a", vec![])],
            ..n(sql, "SvNamePart", " a", L, "", vec![])
        };
        let r = root(
            sql,
            "SvShowStatement",
            vec![
                n(sql, "SvShowViews", "SEMANTIC VIEWS", L, "", vec![]),
                n(
                    sql,
                    "SvShowScope",
                    "SCHEMA a.b.c",
                    L,
                    "",
                    vec![n(
                        sql,
                        "SvShowInSchema",
                        "SCHEMA a.b.c",
                        L,
                        "",
                        vec![n(
                            sql,
                            "SvScopeName",
                            "a.b.c",
                            L,
                            "",
                            vec![first, part("b"), part("c")],
                        )],
                    )],
                ),
            ],
        );
        let err = transform("SvShowStatement", &tree(sql, r)).unwrap_err();
        assert_eq!(
            err.message,
            "Invalid schema name 'a.b.c': expected <schema> or <database>.<schema>"
        );
    }

    #[test]
    fn describe_passes_the_name_as_written() {
        let sql = "DESCRIBE SEMANTIC VIEW \"My\".v";
        let r = root(
            sql,
            "SvDescribe",
            vec![n(sql, "SvViewName", "\"My\".v", L, "", vec![])],
        );
        assert_eq!(
            transform("SvDescribe", &tree(sql, r)).unwrap(),
            "SELECT * FROM describe_semantic_view('\"My\".v')"
        );
    }

    #[test]
    fn show_columns_lowers_to_its_function() {
        let sql = "SHOW COLUMNS IN SEMANTIC VIEW v";
        let r = root(
            sql,
            "SvShowColumns",
            vec![n(sql, "SvViewName", " v", L, "", vec![])],
        );
        assert_eq!(
            transform("SvShowColumns", &tree(sql, r)).unwrap(),
            "SELECT * FROM show_columns_in_semantic_view('v')"
        );
    }
}
