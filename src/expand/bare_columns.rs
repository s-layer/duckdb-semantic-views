//! Bare columns in member expressions (TECH-DEBT #83).
//!
//! Snowflake scopes a member expression to its own logical table: a bare
//! column is that table's column, and "expressions cannot refer to base table
//! columns from other tables". The expansion splices a member's expression
//! into whatever `FROM` the query needs, so a bare column used to be bound by
//! `DuckDB` against every table joined there: an ambiguity error when two
//! joined tables have the column, and a column of another table when only
//! that one has it.
//!
//! CREATE records the columns of each table
//! ([`SemanticViewDefinition::table_columns`], from the caller's own context,
//! `ddl::read_ffi::check_create_members`). With them:
//!
//! - [`qualify_bare_columns`] writes each bare column of a member's own table
//!   as `<alias>.<column>` before the expansion reads the definition, so the
//!   column binds to that table whatever else the query joins;
//! - [`check_bare_columns`] refuses at CREATE a bare name that is a column of
//!   another table and not of the member's own, as Snowflake does.
//!
//! A name is left as written where it is not a reference to a column of the
//! member's table: a keyword where the grammar reads it as one (`NULLS
//! LAST`), a lambda's parameter, a collation, a name inside a subquery (the
//! subquery's own), a function's named argument or a field of a value (which
//! the scanner does not report), and a name the expansion reads as another
//! member: a fact or a variable anywhere, and in a metric another metric of
//! its table outside an aggregate call (`graph::same_table_metrics`). That
//! last rule is the precedence the expansion always had (a fact's name
//! inlines the fact), so a member that relies on it keeps its meaning. A
//! member's own name in its own expression is not that member: an identity
//! fact (`li.qty AS qty`) or a metric named after its column (`o.amount AS
//! SUM(amount)`) reads the column, and so is qualified.
//!
//! A view stored before 0.19.0 has no column lists; its expressions are
//! spliced as written, as they were then.

use std::borrow::Cow;
use std::collections::HashSet;

use crate::expr_tokens::{scan_references, subquery_spans, IdentRef};
use crate::ident::normalize_ident_part;
use crate::model::SemanticViewDefinition;
use crate::query::expr_items::{is_local_name, is_not_a_column, lambda_scopes};

/// One member expression the rule applies to: its kind and name for a
/// message, its table's alias as stored, and its expression.
struct MemberExpr<'a> {
    kind: &'static str,
    name: &'a str,
    table: &'a str,
    expr: &'a str,
    /// Where it sits in the definition.
    slot: Slot,
}

/// A member expression's place: its list and index there.
#[derive(Clone, Copy)]
enum Slot {
    Fact(usize),
    Dimension(usize),
    Metric(usize),
}

impl Slot {
    fn metric(self) -> Option<usize> {
        match self {
            Slot::Metric(i) => Some(i),
            Slot::Fact(_) | Slot::Dimension(_) => None,
        }
    }
}

/// The member expressions with a table of their own, in declaration order:
/// every fact and dimension, and every metric except a window metric, which
/// is emitted from its window specification and whose references are
/// members. A member with no `source_table` (a derived metric) has no table
/// to scope to.
fn member_exprs(def: &SemanticViewDefinition) -> impl Iterator<Item = MemberExpr<'_>> {
    let facts = def.facts.iter().enumerate().filter_map(|(i, f)| {
        Some(MemberExpr {
            kind: "fact",
            name: &f.name,
            table: f.source_table.as_deref()?,
            expr: &f.expr,
            slot: Slot::Fact(i),
        })
    });
    let dims = def.dimensions.iter().enumerate().filter_map(|(i, d)| {
        Some(MemberExpr {
            kind: "dimension",
            name: &d.name,
            table: d.source_table.as_deref()?,
            expr: &d.expr,
            slot: Slot::Dimension(i),
        })
    });
    let metrics = def
        .metrics
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.is_window())
        .filter_map(|(i, m)| {
            Some(MemberExpr {
                kind: "metric",
                name: &m.name,
                table: m.source_table.as_deref()?,
                expr: &m.expr,
                slot: Slot::Metric(i),
            })
        });
    facts.chain(dims).chain(metrics)
}

/// The keys of a table's recorded columns, matched as `DuckDB` matches a
/// column name: case-insensitively.
fn column_keys(columns: &[String]) -> HashSet<String> {
    columns
        .iter()
        .map(|c| normalize_ident_part(&crate::expand::quote_ident(c)))
        .collect()
}

/// The recorded columns of the table whose alias is `alias`, as keys.
fn columns_of(def: &SemanticViewDefinition, alias: &str) -> Option<HashSet<String>> {
    let want = normalize_ident_part(alias);
    def.table_columns
        .iter()
        .find(|(a, _)| normalize_ident_part(a) == want)
        .map(|(_, cols)| column_keys(cols))
}

/// What the expansion reads as another member rather than a column in one
/// member's expression: the name of every fact and variable except the
/// member's own (a fact's own name in its expression is its column, as
/// `graph::facts` reads it), and for a metric the spans where it names
/// another metric of its table (`graph::same_table_metric_ref_spans`).
struct Members {
    names: HashSet<String>,
    own: Option<String>,
    metric_spans: Vec<(usize, usize)>,
}

impl Members {
    /// The fact and variable names of `def`, shared by every member.
    fn names_of(def: &SemanticViewDefinition) -> HashSet<String> {
        def.facts
            .iter()
            .map(|f| normalize_ident_part(&f.name))
            .chain(def.variables.iter().map(|v| normalize_ident_part(&v.name)))
            .collect()
    }

    /// For a member of `kind` named `name`; `metric` is a metric's index.
    fn of(
        def: &SemanticViewDefinition,
        names: HashSet<String>,
        kind: &str,
        name: &str,
        metric: Option<usize>,
    ) -> Self {
        Self {
            names,
            own: (kind == "fact").then(|| normalize_ident_part(name)),
            metric_spans: metric
                .map(|i| crate::graph::same_table_metric_ref_spans(def, i))
                .unwrap_or_default(),
        }
    }

    fn read_as_member(&self, r: &IdentRef<'_>) -> bool {
        let key = r.key();
        (self.names.contains(&key) && self.own.as_ref() != Some(&key))
            || self.metric_spans.contains(&(r.start, r.end))
    }
}

/// The bare names in `text` (comment-free) that could be a column of the
/// member's table: not a keyword where the grammar reads one, not a name of
/// the expression's own, not inside a subquery, and not another member's name.
fn column_candidates<'t>(text: &'t str, members: &Members) -> Vec<IdentRef<'t>> {
    let lambdas = lambda_scopes(text);
    let spans = subquery_spans(text);
    scan_references(text)
        .into_iter()
        .filter(|r| {
            r.is_bare()
                && !beside_a_dot(text, r)
                && !spans.iter().any(|&(s, e)| r.start >= s && r.end <= e)
                && !is_local_name(text, r, &lambdas)
                && !is_not_a_column(text, r)
                && !members.read_as_member(r)
        })
        .collect()
}

/// Whether `r` is a part of a name written with spaces round its dots
/// (`o . comment`, which `DuckDB` reads as `o.comment`): the scanner reports
/// such parts one by one, and neither is a bare column.
fn beside_a_dot(text: &str, r: &IdentRef<'_>) -> bool {
    text[..r.start].trim_end().ends_with('.') || text[r.end..].trim_start().starts_with('.')
}

/// `expr` with each bare column of `own` written as `qualifier.<column>`, or
/// `None` when it has none.
fn qualify_expr(
    expr: &str,
    qualifier: &str,
    own: &HashSet<String>,
    members: &Members,
) -> Option<String> {
    let text = crate::util::blank_sql_comments(expr);
    let refs: Vec<_> = column_candidates(&text, members)
        .into_iter()
        .filter(|r| own.contains(&r.key()))
        .collect();
    if refs.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len() + refs.len() * (qualifier.len() + 1));
    let mut copied = 0;
    for r in refs {
        out.push_str(&text[copied..r.start]);
        out.push_str(qualifier);
        out.push('.');
        out.push_str(r.raw);
        copied = r.end;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

/// `def` with every bare column of a member's own table qualified with that
/// table's alias (see the module docs); `def` itself when it records no
/// columns or nothing needs it. Qualified text is not rescanned, and running
/// it on its own output changes nothing.
#[must_use]
pub(crate) fn qualify_bare_columns(
    def: &SemanticViewDefinition,
) -> Cow<'_, SemanticViewDefinition> {
    if def.table_columns.is_empty() {
        return Cow::Borrowed(def);
    }
    let names = Members::names_of(def);
    let mut out: Option<SemanticViewDefinition> = None;
    for m in member_exprs(def) {
        let Some(own) = columns_of(def, m.table) else {
            continue;
        };
        let members = Members::of(def, names.clone(), m.kind, m.name, m.slot.metric());
        let Some(new) = qualify_expr(
            m.expr,
            &crate::expand::quote_stored_ident(m.table),
            &own,
            &members,
        ) else {
            continue;
        };
        let out = out.get_or_insert_with(|| def.clone());
        match m.slot {
            Slot::Fact(i) => out.facts[i].expr = new,
            Slot::Dimension(i) => out.dimensions[i].expr = new,
            Slot::Metric(i) => out.metrics[i].expr = new,
        }
    }
    out.map_or(Cow::Borrowed(def), Cow::Owned)
}

/// Refuse a member whose expression names, bare, a column of another table
/// and not one of its own table (Snowflake: "expressions cannot refer to base
/// table columns from other tables"). Run at CREATE, once the column lists
/// are recorded; nothing to check without them.
///
/// # Errors
///
/// A message naming the member, the column and its table, worded as the
/// qualified-reference check's (`graph::validate_member_references`).
#[cfg_attr(not(feature = "extension"), allow(dead_code))]
pub(crate) fn check_bare_columns(
    view_name: &str,
    def: &SemanticViewDefinition,
) -> Result<(), String> {
    if def.table_columns.is_empty() {
        return Ok(());
    }
    let tables: Vec<(&str, HashSet<String>)> = def
        .tables
        .iter()
        .filter_map(|t| Some((t.alias.as_str(), columns_of(def, &t.alias)?)))
        .collect();
    let names = Members::names_of(def);
    for m in member_exprs(def) {
        let Some(own) = columns_of(def, m.table) else {
            continue;
        };
        let text = crate::util::blank_sql_comments(m.expr);
        let members = Members::of(def, names.clone(), m.kind, m.name, m.slot.metric());
        for r in column_candidates(&text, &members) {
            let key = r.key();
            if own.contains(&key) {
                continue;
            }
            let own_key = normalize_ident_part(m.table);
            let Some((other, _)) = tables.iter().find(|(alias, cols)| {
                normalize_ident_part(alias) != own_key && cols.contains(&key)
            }) else {
                continue;
            };
            return Err(format!(
                "semantic view '{view_name}': {kind} '{name}' references '{raw}', a column of \
                 table '{other}', but a {kind} expression may only reference columns of its \
                 own table ('{own_table}'). To use a value from another table, define a FACT on \
                 that table and reference the fact by name (e.g. '{other}.<fact_name>').",
                kind = m.kind,
                name = m.name,
                raw = r.raw,
                own_table = m.table,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expand::test_helpers::TestFixtureExt;

    /// Table `o` with the columns given, plus table `li` with `o_id` and
    /// `qty`, so a name of `li` alone is another table's column.
    fn def_with(columns: &[&str]) -> SemanticViewDefinition {
        SemanticViewDefinition {
            table_columns: [
                (
                    "o".to_string(),
                    columns.iter().map(ToString::to_string).collect(),
                ),
                (
                    "li".to_string(),
                    vec!["o_id".to_string(), "qty".to_string()],
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }
        .with_table("o", "orders", &["o_id"])
        .with_table("li", "items", &["li_id"])
    }

    /// The expression of dimension `d` on `o`, `expr`, after the pass.
    fn qualified(columns: &[&str], expr: &str) -> String {
        let def = def_with(columns).with_dimension("d", expr, Some("o"));
        qualify_bare_columns(&def).dimensions[0].expr.clone()
    }

    #[test]
    fn a_bare_own_column_is_qualified() {
        assert_eq!(qualified(&["code"], "upper(code)"), "upper(\"o\".code)");
        assert_eq!(
            qualified(&["Code"], "\"CODE\" || code"),
            "\"o\".\"CODE\" || \"o\".code"
        );
    }

    #[test]
    fn qualified_names_literals_and_other_names_are_left_alone() {
        for expr in [
            "o.code",
            "li.qty",
            "'code'",
            "upper(name)",
            "code(1)",
            "struct_pack(code := 1).code",
        ] {
            assert_eq!(qualified(&["code"], expr), expr, "{expr}");
        }
    }

    /// COR-3's keyword positions and the words the grammar reads as keywords
    /// keep their meaning when the table has a column of that name.
    #[test]
    fn keyword_positions_are_left_alone() {
        let cols = [
            "date", "day", "year", "last", "first", "nulls", "between", "case",
        ];
        for (expr, want) in [
            ("CAST(x AS date)", "CAST(x AS date)"),
            ("x::date", "x::date"),
            ("x + INTERVAL 1 day", "x + INTERVAL 1 day"),
            ("EXTRACT(year FROM x)", "EXTRACT(year FROM x)"),
            (
                "string_agg(first, ',' ORDER BY last NULLS LAST)",
                "string_agg(\"o\".first, ',' ORDER BY \"o\".last NULLS LAST)",
            ),
            ("CASE WHEN x THEN 1 END", "CASE WHEN x THEN 1 END"),
            ("x BETWEEN 1 AND 2", "x BETWEEN 1 AND 2"),
        ] {
            assert_eq!(qualified(&cols, expr), want, "{expr}");
        }
    }

    /// `DuckDB` reads `o . comment` as `o.comment`, a qualified name.
    #[test]
    fn a_name_with_spaces_round_its_dots_is_left_alone() {
        for expr in ["o . comment = 'x'", "o .comment", "o. comment", "s . a . b"] {
            assert_eq!(
                qualified(&["o", "comment", "s", "a", "b"], expr),
                expr,
                "{expr}"
            );
        }
        assert_eq!(qualified(&["comment"], "comment.x"), "comment.x");
    }

    #[test]
    fn lambda_parameters_collations_and_subqueries_are_left_alone() {
        assert_eq!(
            qualified(&["x", "tags"], "list_transform(tags, lambda x: x + 1)"),
            "list_transform(\"o\".tags, lambda x: x + 1)"
        );
        assert_eq!(
            qualified(&["nocase", "n"], "n COLLATE nocase"),
            "\"o\".n COLLATE nocase"
        );
        assert_eq!(
            qualified(&["code"], "code IN (SELECT code FROM other)"),
            "\"o\".code IN (SELECT code FROM other)"
        );
    }

    /// A fact's or variable's name is that member, as the expansion always
    /// read it; in a metric a metric's name is too.
    #[test]
    fn member_names_take_precedence() {
        let def = def_with(&["amount", "tax", "v"])
            .with_fact("tax", "amount * 0.2", "o")
            .with_metric("total", "SUM(amount)", Some("o"))
            .with_metric("m2", "SUM(tax) + total + v", Some("o"));
        let mut def = def;
        def.variables.push(crate::model::Variable {
            name: "v".to_string(),
            ..Default::default()
        });
        let out = qualify_bare_columns(&def);
        assert_eq!(out.facts[0].expr, "\"o\".amount * 0.2");
        assert_eq!(out.metrics[0].expr, "SUM(\"o\".amount)");
        assert_eq!(out.metrics[1].expr, "SUM(tax) + total + v");
    }

    /// The expansion reads a bare name in a metric as a metric only when it
    /// names another metric on the same table, outside an aggregate call
    /// (`graph::same_table_metrics`); anywhere else it is a column.
    #[test]
    fn a_metric_name_is_a_metric_only_where_the_expansion_reads_one() {
        let def = def_with(&["amount", "total", "seats"])
            .with_metric("amount", "SUM(amount)", Some("o"))
            .with_metric("total", "SUM(total)", Some("o"))
            .with_metric("ratio", "total / SUM(total)", Some("o"))
            .with_metric("seats", "SUM(qty)", Some("li"))
            .with_metric("o_seats", "SUM(seats)", Some("o"));
        let out = qualify_bare_columns(&def);
        let exprs: Vec<&str> = out.metrics.iter().map(|m| m.expr.as_str()).collect();
        assert_eq!(
            exprs,
            [
                "SUM(\"o\".amount)",
                "SUM(\"o\".total)",
                "total / SUM(\"o\".total)",
                "SUM(\"li\".qty)",
                "SUM(\"o\".seats)",
            ]
        );
    }

    /// A fact's own name in its own expression is the column (an identity
    /// fact, `li.qty AS qty`), as the fact DAG reads it; elsewhere the name is
    /// the fact.
    #[test]
    fn an_identity_fact_reads_its_own_column() {
        let def = def_with(&["qty"])
            .with_fact("qty", "qty", "li")
            .with_metric("total_qty", "SUM(qty)", Some("li"));
        let out = qualify_bare_columns(&def);
        assert_eq!(out.facts[0].expr, "\"li\".qty");
        assert_eq!(out.metrics[0].expr, "SUM(qty)");
    }

    /// The refusal reads member names by the same rules.
    #[test]
    fn a_self_named_metric_over_another_tables_column_is_refused() {
        let def = def_with(&["amount"]).with_metric("amount", "SUM(amount)", Some("li"));
        let err = check_bare_columns("v", &def).expect_err("amount is o's");
        assert!(err.contains("metric 'amount' references 'amount'"), "{err}");
    }

    #[test]
    fn an_identity_fact_over_another_tables_column_is_refused() {
        let def = def_with(&["code"]).with_fact("code", "code", "li");
        let err = check_bare_columns("v", &def).expect_err("code is o's");
        assert!(err.contains("fact 'code' references 'code'"), "{err}");
    }

    #[test]
    fn comments_are_blanked_where_a_name_is_qualified() {
        assert_eq!(
            qualified(&["code"], "code -- the code"),
            format!("\"o\".code{}", " ".repeat(12))
        );
    }

    #[test]
    fn idempotent_and_borrowed_when_nothing_changes() {
        let def = def_with(&["code"]).with_dimension("d", "upper(code)", Some("o"));
        let once = qualify_bare_columns(&def).into_owned();
        assert!(matches!(qualify_bare_columns(&once), Cow::Borrowed(_)));
        let mut stored = def.clone();
        stored.table_columns.clear();
        assert!(matches!(qualify_bare_columns(&stored), Cow::Borrowed(_)));
    }

    /// A window metric is emitted from its window specification, and a member
    /// with no table has none to scope to.
    #[test]
    fn window_and_sourceless_members_are_left_alone() {
        let def = def_with(&["code"])
            .with_metric("derived", "code + 1", None)
            .with_window_metric(
                "w",
                crate::model::WindowSpec {
                    window_function: "SUM".to_string(),
                    inner_metric: "code".to_string(),
                    ..Default::default()
                },
            );
        let out = qualify_bare_columns(&def);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn another_tables_column_is_refused_and_an_unknown_name_is_not() {
        let def = def_with(&["code"]).with_dimension("d", "code || qty", Some("o"));
        let err = check_bare_columns("v", &def).expect_err("qty is li's");
        assert!(
            err.starts_with(
                "semantic view 'v': dimension 'd' references 'qty', a column of table 'li'"
            ),
            "{err}"
        );
        // Shared with the own table: the own table's.
        let def = def_with(&["o_id"]).with_dimension("d", "o_id", Some("o"));
        assert_eq!(check_bare_columns("v", &def), Ok(()));
        // No table's column (a keyword DuckDB knows, a misspelling): left to
        // the member's binding.
        let def = def_with(&["code"]).with_dimension("d", "current_date + nope", Some("o"));
        assert_eq!(check_bare_columns("v", &def), Ok(()));
        // A fact's name is the fact.
        let def = def_with(&["code"])
            .with_fact("qty", "1", "o")
            .with_dimension("d", "qty", Some("o"));
        assert_eq!(check_bare_columns("v", &def), Ok(()));
    }
}
