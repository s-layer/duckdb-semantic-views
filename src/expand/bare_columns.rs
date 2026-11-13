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
//! member: a variable, a fact of the member's own table, a fact of another
//! table unless the own table has a column of that name (Snowflake: a bare
//! column is the own table's), and in a metric another metric of its table
//! outside an aggregate call (`graph::same_table_metrics`). That
//! last rule is the precedence the expansion always had (a fact's name
//! inlines the fact), so a member that relies on it keeps its meaning. A
//! member's own name in its own expression is not that member: an identity
//! fact (`li.qty AS qty`) or a metric named after its column (`o.amount AS
//! SUM(amount)`) reads the column, and so is qualified.
//!
//! A view stored before 0.19.0 has no column lists; its expressions are
//! spliced as written, as they were then.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::expr_tokens::{scan_function_heads, scan_references, subquery_spans, IdentRef};
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

/// The fact and variable names of a view, shared by every member: each
/// fact's name with its table, and each variable's name.
struct Names {
    facts: HashMap<String, Option<String>>,
    variables: HashSet<String>,
    aliases: HashSet<String>,
}

impl Names {
    fn of(def: &SemanticViewDefinition) -> Self {
        Self {
            facts: def
                .facts
                .iter()
                .map(|f| {
                    let table = f.source_table.as_deref().map(normalize_ident_part);
                    (normalize_ident_part(&f.name), table)
                })
                .collect(),
            variables: def
                .variables
                .iter()
                .map(|v| normalize_ident_part(&v.name))
                .collect(),
            aliases: def
                .tables
                .iter()
                .map(|t| normalize_ident_part(&t.alias))
                .collect(),
        }
    }
}

/// What the expansion reads as another member rather than a column in one
/// member's expression:
///
/// - a variable's name;
/// - a fact's name, except the member's own (a fact's own name in its
///   expression is its column, as `graph::facts` reads it) and except a fact
///   of another table whose name is also a column of the member's own table:
///   that is the own table's column, as in Snowflake, and the fact is named
///   qualified (`c.amount`);
/// - for a metric, the spans where it names another metric of its table
///   (`graph::same_table_metric_ref_spans`).
struct Members<'n> {
    names: &'n Names,
    table: String,
    own_columns: HashSet<String>,
    own: Option<String>,
    metric_spans: Vec<(usize, usize)>,
}

impl<'n> Members<'n> {
    /// For `m`, whose table has the columns `own_columns`.
    fn of(
        def: &SemanticViewDefinition,
        names: &'n Names,
        m: &MemberExpr<'_>,
        own_columns: &HashSet<String>,
    ) -> Self {
        Self {
            names,
            table: normalize_ident_part(m.table),
            own_columns: own_columns.clone(),
            own: (m.kind == "fact").then(|| normalize_ident_part(m.name)),
            metric_spans: m
                .slot
                .metric()
                .map(|i| crate::graph::same_table_metric_ref_spans(def, i))
                .unwrap_or_default(),
        }
    }

    fn read_as_member(&self, r: &IdentRef<'_>) -> bool {
        let key = r.key();
        if self.names.variables.contains(&key) || self.metric_spans.contains(&(r.start, r.end)) {
            return true;
        }
        match self.names.facts.get(&key) {
            None => false,
            Some(_) if self.own.as_ref() == Some(&key) => false,
            Some(table) if table.as_ref() == Some(&self.table) => true,
            Some(_) => !self.own_columns.contains(&key),
        }
    }
}

/// The bare names in `text` (comment-free) that could be a column of the
/// member's table: not a keyword where the grammar reads one, not a name of
/// the expression's own, not inside a subquery, and not another member's name.
fn column_candidates<'t>(text: &'t str, members: &Members<'_>) -> Vec<IdentRef<'t>> {
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

/// The dotted names and method-call heads in `text` that read a field or
/// call a method on a column of the member's own table (`s.a`, `m.day`,
/// `txt.upper()`): their first part is an own column and no table alias of
/// the view (an alias first is that table's column, as `DuckDB` reads it), and
/// they are not a lambda's or comprehension's variable or inside a subquery.
/// Only the first part is qualified, so `s.a` becomes `"o".s.a`.
fn dotted_own_columns<'t>(
    text: &'t str,
    own: &HashSet<String>,
    members: &Members<'_>,
) -> Vec<IdentRef<'t>> {
    let lambdas = lambda_scopes(text);
    let spans = subquery_spans(text);
    scan_references(text)
        .into_iter()
        .chain(scan_function_heads(text))
        .filter(|r| {
            let first = r.first_part_key();
            !r.is_bare()
                && !r.raw.starts_with('"')
                && own.contains(&first)
                && !members.names.aliases.contains(&first)
                && !text[..r.start].trim_end().ends_with('.')
                && !spans.iter().any(|&(s, e)| r.start >= s && r.end <= e)
                && !is_local_name(text, r, &lambdas)
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
    members: &Members<'_>,
) -> Option<String> {
    let text = crate::util::blank_sql_comments(expr);
    let mut refs: Vec<_> = column_candidates(&text, members)
        .into_iter()
        .filter(|r| own.contains(&r.key()))
        .chain(dotted_own_columns(&text, own, members))
        .collect();
    refs.sort_by_key(|r| r.start);
    refs.dedup_by_key(|r| r.start);
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
    let names = Names::of(def);
    let mut out: Option<SemanticViewDefinition> = None;
    for m in member_exprs(def) {
        let Some(own) = columns_of(def, m.table) else {
            continue;
        };
        let members = Members::of(def, &names, &m, &own);
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
    let names = Names::of(def);
    for m in member_exprs(def) {
        let Some(own) = columns_of(def, m.table) else {
            continue;
        };
        let text = crate::util::blank_sql_comments(m.expr);
        let members = Members::of(def, &names, &m, &own);
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
        // Written without spaces, a field of an own column is qualified (O4):
        // see `a_field_or_method_of_an_own_column_is_qualified`.
        assert_eq!(qualified(&["comment"], "comment.x"), "\"o\".comment.x");
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

    /// `WITHIN GROUP`'s `within` is a keyword, not a column named `within`
    /// (v0.19.0 review O3).
    #[test]
    fn within_group_is_left_alone() {
        assert_eq!(
            qualified(&["within", "n"], "MODE() WITHIN GROUP (ORDER BY n)"),
            "MODE() WITHIN GROUP (ORDER BY \"o\".n)"
        );
        assert_eq!(qualified(&["within"], "within + 1"), "\"o\".within + 1");
    }

    /// `ESCAPE` after a `LIKE` pattern is a keyword (O3).
    #[test]
    fn like_escape_is_left_alone() {
        assert_eq!(
            qualified(&["escape", "txt"], "txt LIKE '%a' ESCAPE '!'"),
            "\"o\".txt LIKE '%a' ESCAPE '!'"
        );
        assert_eq!(
            qualified(&["escape"], "upper(escape)"),
            "upper(\"o\".escape)"
        );
    }

    /// `IGNORE NULLS` / `RESPECT NULLS` are keywords (O3).
    #[test]
    fn ignore_and_respect_nulls_are_left_alone() {
        assert_eq!(
            qualified(&["ignore", "n"], "FIRST(n IGNORE NULLS)"),
            "FIRST(\"o\".n IGNORE NULLS)"
        );
        assert_eq!(
            qualified(&["respect", "n"], "FIRST(n RESPECT NULLS)"),
            "FIRST(\"o\".n RESPECT NULLS)"
        );
        assert_eq!(qualified(&["ignore"], "ignore * 2"), "\"o\".ignore * 2");
    }

    /// A list comprehension's `FOR <name> IN` variable is local to it, as a
    /// lambda's parameter is (O3).
    #[test]
    fn comprehension_variables_are_left_alone() {
        assert_eq!(
            qualified(&["day", "tags"], "[day FOR day IN tags IF day > 10]"),
            "[day FOR day IN \"o\".tags IF day > 10]"
        );
        assert_eq!(
            qualified(&["day", "tags"], "day + len([x * 2 FOR x IN tags])"),
            "\"o\".day + len([x * 2 FOR x IN \"o\".tags])"
        );
    }

    /// A field or method read off an own column (`s.a`, `txt.upper()`) is
    /// qualified when its first part is no table alias of the view, so a
    /// joined table with a column of that name leaves it unambiguous
    /// (v0.19.0 review O4).
    #[test]
    fn a_field_or_method_of_an_own_column_is_qualified() {
        assert_eq!(qualified(&["s"], "s.a"), "\"o\".s.a");
        assert_eq!(qualified(&["s"], "s.a.b + 1"), "\"o\".s.a.b + 1");
        assert_eq!(qualified(&["m"], "m.day"), "\"o\".m.day");
        assert_eq!(qualified(&["tags"], "tags.len()"), "\"o\".tags.len()");
        assert_eq!(qualified(&["txt"], "txt.upper()"), "\"o\".txt.upper()");
        assert_eq!(
            qualified(&["txt"], "txt.lower().upper()"),
            "\"o\".txt.lower().upper()"
        );
    }

    /// A dotted name whose first part is a table alias of the view is that
    /// table's column, as `DuckDB` reads it, even when the member's table has a
    /// column of that name; one whose first part is no column is left too.
    #[test]
    fn a_dotted_name_led_by_an_alias_or_no_own_column_is_left_alone() {
        for expr in ["li.qty", "o.code", "li.upper()", "x.a", "x.upper()"] {
            assert_eq!(qualified(&["li", "o", "code", "qty"], expr), expr, "{expr}");
        }
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

    /// A fact on another table does not take over a column of the member's
    /// own table (Snowflake: a bare column is the own table's); where the own
    /// table has no column of that name, the bare name is still the fact.
    #[test]
    fn another_tables_fact_does_not_take_over_an_own_column() {
        let def = def_with(&["amount"])
            .with_fact("amount", "qty * 2", "li")
            .with_dimension("o_amount", "amount", Some("o"))
            .with_metric("rev", "SUM(amount)", Some("o"))
            .with_metric("li_amount", "SUM(amount)", Some("li"));
        let out = qualify_bare_columns(&def);
        let exprs: Vec<&str> = out
            .dimensions
            .iter()
            .map(|d| d.expr.as_str())
            .chain(out.metrics.iter().map(|m| m.expr.as_str()))
            .collect();
        assert_eq!(exprs, ["\"o\".amount", "SUM(\"o\".amount)", "SUM(amount)"]);
        assert_eq!(check_bare_columns("v", &def), Ok(()));
    }

    /// A fact on the member's own table keeps its precedence over a column
    /// of that name, as the expansion always read it.
    #[test]
    fn an_own_tables_fact_keeps_its_precedence() {
        let def = def_with(&["amount", "net"])
            .with_fact("net", "amount * 2", "o")
            .with_metric("rev", "SUM(net)", Some("o"));
        let out = qualify_bare_columns(&def);
        assert_eq!(out.metrics[0].expr, "SUM(net)");
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
