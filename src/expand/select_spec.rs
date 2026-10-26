//! Shared building blocks for the SELECT-emitting expansion strategies.
//!
//! §6.2 (code-review 2026-07-11) consolidates the four hand-rolled
//! SELECT-statement emitters (base, semi-additive, window, materialization)
//! onto shared pieces so their common shapes are defined once. This module is
//! grown incrementally, one piece per PR, under the sqllogictest / proptest /
//! differential oracle.
//!
//! First piece: [`SelectItem`], which owns the `[CAST(expr AS type)] AS alias`
//! rendering the emitters previously inlined at ~11 sites (the `if let
//! Some(type_str) = ...output_type { format!("CAST(...)") }` idiom). Callers
//! pass the already-resolved expression (scoped-alias rewrite / fact inlining
//! done) and the already-quoted output alias; the item owns only the optional
//! `output_type` CAST wrap and the trailing `AS alias`.
//!
//! Also here: the shared [`push_from_base`] (base-table FROM clause) and
//! [`push_group_by_ordinals`] (ordinal GROUP BY) emit helpers, each previously
//! copied across the base / CTE strategies.

use crate::model::SemanticViewDefinition;

use super::join_resolver::{push_join_clauses, ResolvedJoin};
use super::resolution::{qualify_and_quote_table_ref, quote_stored_ident};

/// One `SELECT`-list item: an expression, an optional `output_type` CAST wrap,
/// and an output alias.
///
/// `expr` is the fully-resolved SQL expression (any scoped-alias rewrite or
/// fact/derived-metric inlining is the caller's responsibility) and `alias` is
/// the already-quoted output column name (via
/// [`super::resolution::quote_ident`]). [`SelectItem::render`] emits no leading
/// indentation, so callers keep control of clause layout.
pub(super) struct SelectItem {
    expr: String,
    cast: Option<String>,
    alias: String,
}

impl SelectItem {
    /// Build a select item. `cast` is the dimension/metric/fact `output_type`
    /// (rendered as `CAST(expr AS <cast>)` when `Some`).
    pub(super) fn new(expr: String, cast: Option<String>, alias: String) -> Self {
        Self { expr, cast, alias }
    }

    /// Write `CAST(expr AS <cast>)` (or bare `expr`) into `out`. The single
    /// source of the CAST-wrap rendering, shared by [`Self::rendered_expr`] and
    /// [`Self::render`] so they cannot diverge (the E-1 invariant lives here);
    /// writing into the caller's buffer keeps `render` to one allocation and
    /// avoids cloning `expr` in the common no-cast case.
    fn write_expr(&self, out: &mut String) {
        match &self.cast {
            Some(ty) => {
                out.push_str("CAST(");
                out.push_str(&self.expr);
                out.push_str(" AS ");
                out.push_str(ty);
                out.push(')');
            }
            None => out.push_str(&self.expr),
        }
    }

    /// The rendered expression with the optional CAST wrap applied, WITHOUT the
    /// trailing `AS alias`: `CAST(expr AS <cast>)` when a cast is set, else
    /// `expr`. Use where the same expression must be repeated elsewhere in the
    /// query — e.g. a window `PARTITION BY` / `ORDER BY` that must reference a
    /// CTE column by its expression rather than the shadowing select alias
    /// (E-1, code-review 2026-07-11).
    pub(super) fn rendered_expr(&self) -> String {
        let mut out = String::with_capacity(self.expr.len() + 16);
        self.write_expr(&mut out);
        out
    }

    /// Render as `<rendered_expr> AS alias` — i.e. `CAST(expr AS <cast>) AS
    /// alias` when a cast is set, else `expr AS alias`. No leading indent — the
    /// caller prepends clause indentation.
    pub(super) fn render(&self) -> String {
        let mut out = String::with_capacity(self.expr.len() + self.alias.len() + 24);
        self.write_expr(&mut out);
        out.push_str(" AS ");
        out.push_str(&self.alias);
        out
    }
}

/// Append the base-table FROM clause: `<lead>FROM <qualified-table> [AS
/// <alias>]`.
///
/// `lead` is the whitespace before `FROM` — `"\n"` at the top level, `"\n    "`
/// inside a CTE. The base table is qualified + quoted via
/// [`qualify_and_quote_table_ref`]; the first declared table's alias is
/// appended as `AS <quote_ident>` when present. Shared by the base, facts,
/// semi-additive, and window emitters (§6.2). The materialization renderer
/// intentionally does not use this — it selects from the pre-aggregated table
/// with no alias.
pub(super) fn push_from_base(sql: &mut String, def: &SemanticViewDefinition, lead: &str) {
    sql.push_str(lead);
    sql.push_str("FROM ");
    sql.push_str(&qualify_and_quote_table_ref(def.base_table(), def));
    if let Some(base_ref) = def.tables.first() {
        sql.push_str(" AS ");
        sql.push_str(&quote_stored_ident(&base_ref.alias));
    }
}

/// Append a FROM clause anchored at an arbitrary declared table:
/// `<lead>FROM <qualified-table> AS <alias>`.
///
/// The per-grain strategy ([`super::per_grain`]) anchors each grain's CTE at
/// that grain's own table rather than the base table, which is the whole point
/// of computing a metric at its own grain. `alias` is the lowercased declared
/// alias; an alias that matches no declared table falls back to
/// [`push_from_base`] so the emitter can never produce a FROM-less SELECT.
pub(super) fn push_from_anchor(
    sql: &mut String,
    def: &SemanticViewDefinition,
    alias: &str,
    lead: &str,
) {
    let Some(table_ref) = def
        .tables
        .iter()
        .find(|t| t.alias.to_ascii_lowercase() == alias)
    else {
        push_from_base(sql, def, lead);
        return;
    };
    sql.push_str(lead);
    sql.push_str("FROM ");
    sql.push_str(&qualify_and_quote_table_ref(&table_ref.table, def));
    sql.push_str(" AS ");
    sql.push_str(&quote_stored_ident(&table_ref.alias));
}

/// Append an ordinal `GROUP BY` for `n` grouping columns: `<lead>GROUP BY\n`
/// then `<item_indent>1,\n<item_indent>2,…`. No-op when `n == 0`.
///
/// Ordinal grouping (never expressions) is what defends the base + CTE
/// aggregation paths from the E-1 alias-shadowing pitfall — `GROUP BY 1` can't
/// be captured by a same-named physical column the way `GROUP BY "region"`
/// could. Callers own the decision of WHETHER to group (the emptiness guard
/// differs by strategy: the base path needs both dimensions and metrics, the
/// CTE paths need only dimensions). `lead` is the whitespace before `GROUP BY`
/// (`"\n"` flat, `"\n    "` in a CTE); `item_indent` the per-ordinal indent
/// (`"    "` flat, `"        "` in a CTE).
pub(super) fn push_group_by_ordinals(sql: &mut String, n: usize, lead: &str, item_indent: &str) {
    if n == 0 {
        return;
    }
    sql.push_str(lead);
    sql.push_str("GROUP BY\n");
    let items: Vec<String> = (1..=n).map(|i| format!("{item_indent}{i}")).collect();
    sql.push_str(&items.join(",\n"));
}

/// The `GROUP BY` of a top-level [`SelectSpec`].
pub(super) enum GroupBy {
    /// No `GROUP BY` — a dimensions-only `DISTINCT` query, a global aggregate
    /// (metrics, no dimensions), an unaggregated fact query, or a window outer
    /// query (window functions are row-level).
    None,
    /// Ordinal `GROUP BY` over the first `n` select items. The emitters always
    /// place the grouping dimensions first, so `1..=n` names exactly them.
    /// Ordinals — never expressions — are the E-1 alias-shadowing defense; see
    /// [`push_group_by_ordinals`].
    Ordinals(usize),
}

/// The `FROM` source of a top-level [`SelectSpec`].
pub(super) enum FromSource<'a> {
    /// The declared base table plus the resolver-selected `LEFT JOIN`s:
    /// `FROM <qualified-base> [AS <alias>]` followed by each join. Carries
    /// `def` for table qualification (both the base ref via [`push_from_base`]
    /// and each join's physical table via [`push_join_clauses`]).
    BaseTable {
        def: &'a SemanticViewDefinition,
        joins: Vec<ResolvedJoin<'a>>,
    },
    /// A declared table other than the base table, plus the joins reaching the
    /// query's dimensions from it — the per-grain strategy's single-grain shape
    /// (see [`push_from_anchor`]).
    AnchorTable {
        def: &'a SemanticViewDefinition,
        anchor: String,
        joins: Vec<ResolvedJoin<'a>>,
    },
    /// A bare, already-safe relation name — a CTE alias such as `__sv_snapshot`
    /// / `__sv_agg`: emitted as `FROM <name>`, with no qualification, no `AS`
    /// alias, and (structurally) no joins.
    Named(String),
}

/// A whole top-level `SELECT` statement: `SELECT[ DISTINCT]` + select list +
/// `FROM` (+ `LEFT JOIN`s) + optional ordinal `GROUP BY`.
///
/// The culmination of §6.2 move 3 (code-review 2026-07-11): the four
/// hand-rolled top-level emitters — base ([`super::sql_gen::expand`]), facts
/// (`sql_gen`'s `expand_facts`), and the OUTER query of the two CTE strategies
/// ([`super::semi_additive`], [`super::window`]) — construct a `SelectSpec` and
/// call [`Self::render`] instead of assembling the string by hand, so the
/// statement skeleton (and the E-1 ordinal-`GROUP BY` defense) lives in one
/// place.
///
/// The CTE strategies' INNER `SELECT` is deliberately NOT modelled here: its
/// select list interleaves bespoke `RANK() OVER (...)` / decomposed-aggregate
/// columns at a deeper indent, so it keeps hand-emitting while still sharing the
/// lower-level [`SelectItem`], [`push_from_base`], [`push_join_clauses`], and
/// [`push_group_by_ordinals`] pieces (the goal is the shared alias-shadowing
/// defense + one render path for the common shape, not forcing every byte
/// through it).
pub(super) struct SelectSpec<'a> {
    /// Emit `SELECT DISTINCT` rather than `SELECT` (dimensions-only base query).
    pub(super) distinct: bool,
    /// The select-list items in emission order. When [`Self::group_by`] is
    /// [`GroupBy::Ordinals`], the grouping dimensions are the leading items.
    pub(super) items: Vec<SelectItem>,
    /// The `FROM` source (+ joins, for the base-table case).
    pub(super) from: FromSource<'a>,
    /// The pre-aggregation predicate, already resolved to raw SQL, or `None`.
    ///
    /// Rendered between the joins and the `GROUP BY`, which is what makes it
    /// *pre*-aggregation: the rows are filtered on their way into the grouping,
    /// not after it. Only the base-anchored and facts emitters set this; the CTE
    /// strategies must filter inside their CTE (before `RANK`/the window
    /// function), so they reject a predicate rather than render one out here
    /// where it would be applied too late.
    pub(super) where_clause: Option<String>,
    /// The `GROUP BY`, if any.
    pub(super) group_by: GroupBy,
}

impl SelectSpec<'_> {
    /// Render the whole statement — no leading indentation and no trailing
    /// newline. Top-level callers place these at column 0; the CTE strategies
    /// append the outer statement directly after the `)\n` closing the CTE.
    pub(super) fn render(&self) -> String {
        let mut sql = String::with_capacity(256);
        sql.push_str(if self.distinct {
            "SELECT DISTINCT\n"
        } else {
            "SELECT\n"
        });
        let rendered: Vec<String> = self
            .items
            .iter()
            .map(|item| format!("    {}", item.render()))
            .collect();
        sql.push_str(&rendered.join(",\n"));
        match &self.from {
            FromSource::BaseTable { def, joins } => {
                push_from_base(&mut sql, def, "\n");
                push_join_clauses(&mut sql, joins, def, "\nLEFT JOIN ");
            }
            FromSource::AnchorTable { def, anchor, joins } => {
                push_from_anchor(&mut sql, def, anchor, "\n");
                push_join_clauses(&mut sql, joins, def, "\nLEFT JOIN ");
            }
            FromSource::Named(name) => {
                sql.push_str("\nFROM ");
                sql.push_str(name);
            }
        }
        if let Some(predicate) = &self.where_clause {
            sql.push_str("\nWHERE ");
            sql.push_str(predicate);
        }
        match self.group_by {
            GroupBy::None => {}
            GroupBy::Ordinals(n) => push_group_by_ordinals(&mut sql, n, "\n", "    "),
        }
        sql
    }
}

#[cfg(test)]
mod tests {
    use super::{push_group_by_ordinals, FromSource, GroupBy, SelectItem, SelectSpec};
    use crate::expand::test_helpers::minimal_def;

    #[test]
    fn renders_without_cast() {
        let item = SelectItem::new("o.region".to_string(), None, "\"region\"".to_string());
        assert_eq!(item.render(), "o.region AS \"region\"");
    }

    #[test]
    fn renders_with_cast() {
        let item = SelectItem::new(
            "SUM(o.amount)".to_string(),
            Some("DECIMAL(18,2)".to_string()),
            "\"revenue\"".to_string(),
        );
        assert_eq!(
            item.render(),
            "CAST(SUM(o.amount) AS DECIMAL(18,2)) AS \"revenue\""
        );
    }

    #[test]
    fn rendered_expr_omits_alias() {
        // No cast: the bare expression.
        let plain = SelectItem::new("upper(o.region)".to_string(), None, "\"r\"".to_string());
        assert_eq!(plain.rendered_expr(), "upper(o.region)");
        // With cast: the CAST wrap, still no `AS alias`. This is what the E-1
        // window PARTITION/ORDER clauses repeat instead of the select alias.
        let cast = SelectItem::new(
            "o.d".to_string(),
            Some("DATE".to_string()),
            "\"d\"".to_string(),
        );
        assert_eq!(cast.rendered_expr(), "CAST(o.d AS DATE)");
        assert_eq!(cast.render(), "CAST(o.d AS DATE) AS \"d\"");
    }

    /// Byte-identical to the pre-refactor idiom: the caller's
    /// `format!("    {}", item.render())` must equal the old
    /// `format!("    {} AS {}", final_expr, alias)` for both branches.
    #[test]
    fn matches_legacy_indented_format() {
        let alias = "\"m\"".to_string();
        // no cast
        let legacy_expr = "expr".to_string();
        let legacy = format!("    {} AS {}", legacy_expr, alias);
        let item = SelectItem::new("expr".to_string(), None, alias.clone());
        assert_eq!(format!("    {}", item.render()), legacy);
        // with cast (legacy pre-wraps the expr into final_expr)
        let legacy_final = format!("CAST({} AS {})", "expr", "INT");
        let legacy_cast = format!("    {} AS {}", legacy_final, alias);
        let item_cast = SelectItem::new("expr".to_string(), Some("INT".to_string()), alias.clone());
        assert_eq!(format!("    {}", item_cast.render()), legacy_cast);
    }

    #[test]
    fn group_by_ordinals_flat_and_cte_indents() {
        // Flat (top-level) layout: "\nGROUP BY\n" + 4-space ordinals.
        let mut flat = String::new();
        push_group_by_ordinals(&mut flat, 3, "\n", "    ");
        assert_eq!(flat, "\nGROUP BY\n    1,\n    2,\n    3");
        // CTE layout: "\n    GROUP BY\n" + 8-space ordinals.
        let mut cte = String::new();
        push_group_by_ordinals(&mut cte, 2, "\n    ", "        ");
        assert_eq!(cte, "\n    GROUP BY\n        1,\n        2");
    }

    #[test]
    fn group_by_ordinals_zero_is_noop() {
        let mut s = String::from("prefix");
        push_group_by_ordinals(&mut s, 0, "\n", "    ");
        assert_eq!(s, "prefix");
    }

    // push_from_base is covered end-to-end (byte-identical) by the exact-string
    // emission tests in sql_gen / semi_additive / window; a direct unit test
    // would need a full SemanticViewDefinition fixture for little added signal.

    #[test]
    fn render_base_table_with_group_by() {
        // The base metrics-and-dimensions shape: SELECT + items + FROM base +
        // ordinal GROUP BY. `minimal_def("orders", …)` has no db/schema, so the
        // base table qualifies to `"orders" AS "orders"`.
        let def = minimal_def("orders", "region", "region", "cnt", "count(*)");
        let spec = SelectSpec {
            where_clause: None,
            distinct: false,
            items: vec![
                SelectItem::new("region".to_string(), None, "\"region\"".to_string()),
                SelectItem::new("count(*)".to_string(), None, "\"cnt\"".to_string()),
            ],
            from: FromSource::BaseTable {
                def: &def,
                joins: Vec::new(),
            },
            group_by: GroupBy::Ordinals(1),
        };
        assert_eq!(
            spec.render(),
            "SELECT\n    region AS \"region\",\n    count(*) AS \"cnt\"\n\
             FROM \"orders\" AS \"orders\"\nGROUP BY\n    1"
        );
    }

    #[test]
    fn render_base_table_distinct_no_group_by() {
        // Dimensions-only base query: SELECT DISTINCT, no GROUP BY.
        let def = minimal_def("orders", "region", "region", "cnt", "count(*)");
        let spec = SelectSpec {
            where_clause: None,
            distinct: true,
            items: vec![SelectItem::new(
                "region".to_string(),
                None,
                "\"region\"".to_string(),
            )],
            from: FromSource::BaseTable {
                def: &def,
                joins: Vec::new(),
            },
            group_by: GroupBy::None,
        };
        assert_eq!(
            spec.render(),
            "SELECT DISTINCT\n    region AS \"region\"\nFROM \"orders\" AS \"orders\""
        );
    }

    #[test]
    fn render_named_source_with_group_by() {
        // The semi-additive OUTER shape: SELECT over a bare CTE name (no
        // qualification, no alias, no joins) with an ordinal GROUP BY.
        let spec = SelectSpec {
            where_clause: None,
            distinct: false,
            items: vec![
                SelectItem::new("\"region\"".to_string(), None, "\"region\"".to_string()),
                SelectItem::new(
                    "SUM(\"__sv_reg_0\")".to_string(),
                    None,
                    "\"total\"".to_string(),
                ),
            ],
            from: FromSource::Named("__sv_snapshot".to_string()),
            group_by: GroupBy::Ordinals(1),
        };
        assert_eq!(
            spec.render(),
            "SELECT\n    \"region\" AS \"region\",\n    SUM(\"__sv_reg_0\") AS \"total\"\n\
             FROM __sv_snapshot\nGROUP BY\n    1"
        );
    }

    #[test]
    fn render_named_source_no_group_by() {
        // The window OUTER shape: SELECT over a bare CTE name, no GROUP BY.
        let spec = SelectSpec {
            where_clause: None,
            distinct: false,
            items: vec![SelectItem::new(
                "AVG(\"q\") OVER ()".to_string(),
                None,
                "\"m\"".to_string(),
            )],
            from: FromSource::Named("__sv_agg".to_string()),
            group_by: GroupBy::None,
        };
        assert_eq!(
            spec.render(),
            "SELECT\n    AVG(\"q\") OVER () AS \"m\"\nFROM __sv_agg"
        );
    }
}
