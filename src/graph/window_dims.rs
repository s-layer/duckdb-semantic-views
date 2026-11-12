//! Window metric validation at CREATE (Phase 7 PRs 5 and 6b).
//!
//! A window metric's `PARTITION BY`, `PARTITION BY EXCLUDING` and `ORDER BY`
//! name dimensions as Snowflake allows them (fixtures 3, 10, 10b and 10c):
//!
//! - a bare name only of the window metric's own table (`000904: invalid
//!   identifier` for another table's, V3);
//! - a qualified name of the table its inner metric aggregates, whose rows the
//!   window runs over, or of a table that table reaches by one chain of
//!   foreign keys (a parent, `r.region`, V1, V2, V4); not of a table that
//!   references it, a finer grain (010262, 010259), nor of one reached two
//!   ways, such as a role-played parent (010227, V8), nor of one it is not
//!   related to at all, such as across a many-to-many bridge (010211, N0).
//!
//! It also refuses, as Snowflake does, a window over a metric with `USING`
//! (010905: only a derived metric may refer to one), a frame clause with no
//! `ORDER BY` (002305), and an order-sensitive function (`LAG`, `LEAD`,
//! `FIRST_VALUE`, rank functions, ...) with no `ORDER BY` (002061). Each of
//! these gives a value that depends on the order the grouped rows happen to
//! come in.
//!
//! These run only at CREATE (`ddl::define`): a view stored before them keeps
//! loading and answering as it did.

use crate::errors::ParseError;
use crate::ident::{first_unquoted_dot, ident_matches};
use crate::model::{Cardinality, Dimension, Metric, SemanticViewDefinition, WindowSpec};

/// The declared dimension a window clause reference names, bare or in the
/// dotted `alias.name` form.
fn find_dimension<'a>(def: &'a SemanticViewDefinition, reference: &str) -> Option<&'a Dimension> {
    if let Some(dot) = first_unquoted_dot(reference) {
        let (alias, name) = (&reference[..dot], &reference[dot + 1..]);
        def.dimensions.iter().find(|d| {
            ident_matches(&d.name, name)
                && d.source_table
                    .as_deref()
                    .is_some_and(|t| ident_matches(t, alias))
        })
    } else {
        def.dimensions
            .iter()
            .find(|d| ident_matches(&d.name, reference))
    }
}

/// Window functions whose value depends on the order of the rows in their
/// window, which Snowflake refuses with no `ORDER BY` (002061).
const ORDER_SENSITIVE: [&str; 11] = [
    "LAG",
    "LEAD",
    "FIRST_VALUE",
    "LAST_VALUE",
    "NTH_VALUE",
    "ROW_NUMBER",
    "RANK",
    "DENSE_RANK",
    "PERCENT_RANK",
    "CUME_DIST",
    "NTILE",
];

/// The number of ways from `from` to `to` along foreign keys, stepping back
/// along a one-to-one relationship too, each relationship a way of its own
/// (so a table referenced twice is reached two ways), each table at most once
/// on a way. Counting stops at two.
fn chain_count(def: &SemanticViewDefinition, from: &str, to: &str) -> usize {
    fn walk(def: &SemanticViewDefinition, at: &str, to: &str, seen: &mut Vec<String>) -> usize {
        if at.eq_ignore_ascii_case(to) {
            return 1;
        }
        let mut n = 0;
        for j in &def.joins {
            let (src, dst) = (
                j.from_alias.to_ascii_lowercase(),
                j.table.to_ascii_lowercase(),
            );
            let next = if src == at {
                dst
            } else if dst == at && j.cardinality == Cardinality::OneToOne {
                src
            } else {
                continue;
            };
            if seen.contains(&next) {
                continue;
            }
            seen.push(next.clone());
            n += walk(def, &next, to, seen);
            seen.pop();
            if n >= 2 {
                return n;
            }
        }
        n
    }
    let from = from.to_ascii_lowercase();
    walk(def, &from, to, &mut vec![from.clone()])
}

/// Whether `to` is reached from `from` along foreign keys, stepping back along
/// a one-to-one relationship too (it does not fan out).
fn reaches(def: &SemanticViewDefinition, from: &str, to: &str) -> bool {
    let mut seen = vec![from.to_ascii_lowercase()];
    let mut i = 0;
    while i < seen.len() {
        let at = seen[i].clone();
        if at.eq_ignore_ascii_case(to) {
            return true;
        }
        for j in &def.joins {
            let (src, dst) = (
                j.from_alias.to_ascii_lowercase(),
                j.table.to_ascii_lowercase(),
            );
            let next = if src == at {
                Some(dst)
            } else if dst == at && j.cardinality == Cardinality::OneToOne {
                Some(src)
            } else {
                None
            };
            if let Some(next) = next {
                if !seen.contains(&next) {
                    seen.push(next);
                }
            }
        }
        i += 1;
    }
    false
}

/// The tables whose rows a window metric's inner metric aggregates: its own,
/// or a derived one's base metrics' tables. Empty for an unknown inner metric,
/// which `cross_refs` reports.
fn inner_tables<'a>(
    def: &'a SemanticViewDefinition,
    inner: &str,
) -> (Option<&'a Metric>, Vec<String>) {
    let Some(m) = def.metrics.iter().find(|m| ident_matches(&m.name, inner)) else {
        return (None, Vec::new());
    };
    let tables = match &m.source_table {
        Some(t) => vec![t.clone()],
        None => crate::expand::collect_derived_metric_source_tables(m, &def.metrics),
    };
    (Some(m), tables)
}

/// Refuse a window metric Snowflake refuses at CREATE (see the module docs),
/// and one whose own `USING` cannot apply to its inner metric.
///
/// A window's `USING` names relationships from the window metric's table and
/// applies to its inner metric, so that metric must be declared on the same
/// table (`ExpandError::WindowUsingOffItsTable` refuses it at query time).
pub fn validate_window_dimensions(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    for metric in &def.metrics {
        let Some(ws) = &metric.window_spec else {
            continue;
        };
        validate_window_order(metric, ws)?;
        let (inner, tables) = inner_tables(def, &ws.inner_metric);
        if let Some(inner) = inner {
            if !inner.using_relationships.is_empty() {
                return Err(ParseError::positionless(format!(
                    "Window metric '{}' is computed over metric '{}', which has a USING \
                     clause. As in Snowflake, only a derived metric may refer to a metric with \
                     USING; write the window over a metric without one.",
                    metric.name, inner.name
                )));
            }
            let same_table = matches!(
                (&metric.source_table, &inner.source_table),
                (Some(w), Some(t)) if ident_matches(w, t)
            );
            if !metric.using_relationships.is_empty() && !same_table {
                return Err(ParseError::positionless(format!(
                    "Window metric '{}' has a USING clause, but its inner metric '{}' is not on \
                     its table. A window metric's USING applies to its inner metric, so the \
                     inner metric must be declared on the window metric's table.",
                    metric.name, inner.name
                )));
            }
        }
        let refs = ws
            .excluding_dims
            .iter()
            .map(|r| (r, true))
            .chain(ws.partition_dims.iter().map(|r| (r, false)))
            .chain(ws.order_by.iter().map(|ob| (&ob.expr, false)));
        for (reference, excluding) in refs {
            let Some(dim) = find_dimension(def, reference) else {
                continue; // `cross_refs` reports an unknown dimension.
            };
            let Some(dim_table) = dim.source_table.as_deref() else {
                continue;
            };
            if let Some(own) = metric.source_table.as_deref() {
                if first_unquoted_dot(reference).is_none() && !ident_matches(own, dim_table) {
                    return Err(ParseError::positionless(format!(
                        "Window metric '{}': its window clause names dimension '{reference}' \
                         bare, but it is on table '{dim_table}', not on the metric's table \
                         '{own}'. As in Snowflake, a bare name in a window clause names a \
                         dimension of the metric's own table; write '{dim_table}.{reference}' \
                         to name another table's.",
                        metric.name
                    )));
                }
            }
            for table in &tables {
                check_window_reach(def, metric, ws, reference, excluding, dim_table, table)?;
            }
        }
    }
    Ok(())
}

/// A frame clause, or an order-sensitive function, with no `ORDER BY`
/// (Snowflake's 002305 and 002061).
fn validate_window_order(metric: &Metric, ws: &WindowSpec) -> Result<(), ParseError> {
    if !ws.order_by.is_empty() {
        return Ok(());
    }
    if ws.frame_clause.is_some() {
        return Err(ParseError::positionless(format!(
            "Window metric '{}': a window frame requires an ORDER BY clause, as in \
             Snowflake. Without one, which rows fall in the frame depends on the order the \
             grouped rows come in.",
            metric.name
        )));
    }
    let function = ws.window_function.trim();
    if let Some(f) = ORDER_SENSITIVE
        .iter()
        .find(|f| function.eq_ignore_ascii_case(f))
    {
        return Err(ParseError::positionless(format!(
            "Window metric '{}': window function {f} requires ORDER BY in its window \
             specification, as in Snowflake. Without one, its value depends on the order the \
             grouped rows come in.",
            metric.name
        )));
    }
    Ok(())
}

/// A window clause dimension on `dim_table`, for a window over the rows of
/// `table`: the table itself, or a table it reaches by one chain of foreign
/// keys.
fn check_window_reach(
    def: &SemanticViewDefinition,
    metric: &Metric,
    ws: &WindowSpec,
    reference: &str,
    excluding: bool,
    dim_table: &str,
    table: &str,
) -> Result<(), ParseError> {
    if ident_matches(dim_table, table) {
        return Ok(());
    }
    match chain_count(def, table, dim_table) {
        1 => Ok(()),
        0 if reaches(def, dim_table, table) => Err(ParseError::positionless(format!(
            "Window metric '{}': its window clause names dimension '{reference}' of table \
             '{dim_table}', which references the metric's table '{table}', whose rows its inner \
             metric '{}' aggregates, so it is at a finer grain. As in Snowflake, a window's \
             {clause} may name dimensions of '{table}' or of a table it references.",
            metric.name,
            ws.inner_metric,
            clause = if excluding {
                "PARTITION BY EXCLUDING"
            } else {
                "PARTITION BY and ORDER BY"
            },
        ))),
        0 => Err(ParseError::positionless(format!(
            "Window metric '{}': its window clause names dimension '{reference}' of table \
             '{dim_table}', which is unrelated to the metric's table '{table}', whose rows its \
             inner metric '{}' aggregates: neither reaches the other along relationships (for \
             example, they are joined through a many-to-many bridge). A window clause may name \
             dimensions of '{table}' or of a table it references.",
            metric.name, ws.inner_metric
        ))),
        _ => Err(ParseError::positionless(format!(
            "Window metric '{}': its window clause names dimension '{reference}' of table \
             '{dim_table}', which the metric's table '{table}' reaches by more than one path \
             (a table it references twice, or two chains of relationships). As in Snowflake, a \
             window clause cannot name a dimension reached more than one way.",
            metric.name
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_window_dimensions;
    use crate::model::{
        Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef, WindowOrderBy,
        WindowSpec,
    };

    fn table(alias: &str) -> TableRef {
        TableRef {
            alias: alias.to_string(),
            table: alias.to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }
    }

    fn rel(from: &str, to: &str, card: Cardinality) -> Join {
        Join {
            table: to.to_string(),
            from_alias: from.to_string(),
            fk_columns: vec![format!("{to}_id")],
            ref_columns: vec!["id".to_string()],
            name: Some(format!("{from}_{to}")),
            cardinality: card,
            ..Default::default()
        }
    }

    fn dim(name: &str, table: &str) -> Dimension {
        Dimension {
            name: name.to_string(),
            expr: format!("{table}.{name}"),
            source_table: Some(table.to_string()),
            ..Default::default()
        }
    }

    /// Books, authors through the `ba` bridge, the books' publishers above
    /// them, reviews below them, and the publishers' awards beside the books.
    /// `w` is a window on books ordered by `order_by`.
    fn view(order_by: &str) -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: ["b", "a", "ba", "p", "r", "aw"].map(table).to_vec(),
            joins: vec![
                rel("ba", "a", Cardinality::ManyToOne),
                rel("ba", "b", Cardinality::ManyToOne),
                rel("b", "p", Cardinality::ManyToOne),
                rel("r", "b", Cardinality::ManyToOne),
                rel("aw", "p", Cardinality::ManyToOne),
            ],
            dimensions: vec![
                dim("title", "b"),
                dim("country", "a"),
                dim("publisher", "p"),
                dim("stars", "r"),
                dim("award", "aw"),
            ],
            metrics: vec![
                Metric {
                    name: "pages".to_string(),
                    expr: "SUM(b.pages)".to_string(),
                    source_table: Some("b".to_string()),
                    ..Default::default()
                },
                Metric {
                    name: "w".to_string(),
                    expr: "SUM(pages) OVER (...)".to_string(),
                    source_table: Some("b".to_string()),
                    window_spec: Some(WindowSpec {
                        window_function: "SUM".to_string(),
                        inner_metric: "pages".to_string(),
                        order_by: vec![WindowOrderBy {
                            expr: order_by.to_string(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    /// N0: ordered by the authors' country, across the bridge.
    #[test]
    fn a_window_ordered_across_a_bridge_is_refused() {
        for (reference, text) in [("country", "bare"), ("a.country", "unrelated")] {
            let err = validate_window_dimensions(&view(reference))
                .expect_err("a dimension across a bridge is unrelated");
            assert!(
                err.message.contains(text) && err.message.contains("'a'"),
                "{reference}: {}",
                err.message
            );
        }
    }

    /// ... and in `PARTITION BY` and `PARTITION BY EXCLUDING`.
    #[test]
    fn a_window_partitioned_across_a_bridge_is_refused() {
        let mut def = view("title");
        let ws = def.metrics[1].window_spec.as_mut().expect("window");
        ws.partition_dims = vec!["country".to_string()];
        assert!(validate_window_dimensions(&def).is_err());
        let ws = def.metrics[1].window_spec.as_mut().expect("window");
        ws.partition_dims.clear();
        ws.excluding_dims = vec!["country".to_string()];
        assert!(validate_window_dimensions(&def).is_err());
    }

    /// A table beside the metric's, under the same parent, is unrelated too.
    #[test]
    fn a_window_ordered_by_a_sibling_table_is_refused() {
        assert!(validate_window_dimensions(&view("award")).is_err());
    }

    /// The metric's own table, bare or qualified, and a table above it,
    /// qualified, as Snowflake takes them (fixture 10c, V1).
    #[test]
    fn a_window_ordered_by_its_own_or_a_parent_table_is_accepted() {
        for reference in ["title", "b.title", "p.publisher"] {
            assert!(
                validate_window_dimensions(&view(reference)).is_ok(),
                "{reference}"
            );
        }
    }

    /// A parent's dimension named bare is refused, as Snowflake refuses it
    /// (fixture 10c, V3: 000904).
    #[test]
    fn a_bare_name_of_another_tables_dimension_is_refused() {
        let err = validate_window_dimensions(&view("publisher")).expect_err("bare parent");
        assert!(
            err.message.contains("bare") && err.message.contains("'p.publisher'"),
            "{}",
            err.message
        );
    }

    /// A table that references the metric's (reviews, a finer grain) is
    /// refused, qualified or bare, as Snowflake refuses it (fixture 10b,
    /// ordf: 010262; ordfx: 010259).
    #[test]
    fn a_window_ordered_by_a_finer_table_is_refused() {
        let err = validate_window_dimensions(&view("r.stars")).expect_err("finer table");
        assert!(err.message.contains("finer grain"), "{}", err.message);
        assert!(err.message.contains("PARTITION BY and ORDER BY"));
        assert!(validate_window_dimensions(&view("stars")).is_err());
        let mut def = view("title");
        let ws = def.metrics[1].window_spec.as_mut().expect("window");
        ws.excluding_dims = vec!["r.stars".to_string()];
        let err = validate_window_dimensions(&def).expect_err("finer table excluded");
        assert!(
            err.message.contains("PARTITION BY EXCLUDING"),
            "{}",
            err.message
        );
    }

    /// A parent reached two ways is refused: one the metric's table
    /// references twice (a role-played parent, fixture 10c V8: 010227), or
    /// one reached along two chains.
    #[test]
    fn a_parent_reached_two_ways_is_refused() {
        let mut def = view("p.publisher");
        let mut second = def.joins[2].clone();
        second.name = Some("b_p2".to_string());
        second.fk_columns = vec!["p2_id".to_string()];
        def.joins.push(second);
        let err = validate_window_dimensions(&def).expect_err("role-played parent");
        assert!(
            err.message.contains("more than one path"),
            "{}",
            err.message
        );
        // Books -> publishers and books -> imprints -> publishers.
        let mut def = view("p.publisher");
        def.tables.push(table("im"));
        def.joins.push(rel("b", "im", Cardinality::ManyToOne));
        def.joins.push(rel("im", "p", Cardinality::ManyToOne));
        assert!(validate_window_dimensions(&def).is_err());
    }

    /// A window over a metric with `USING` is refused: Snowflake lets only a
    /// derived metric refer to one (fixture 10, air: 010905).
    #[test]
    fn a_window_over_a_using_metric_is_refused() {
        let mut def = view("title");
        def.metrics[0].using_relationships = vec!["b_p".to_string()];
        let err = validate_window_dimensions(&def).expect_err("window over USING");
        assert!(
            err.message.contains("only a derived metric"),
            "{}",
            err.message
        );
    }

    /// A frame, or an order-sensitive function, with no `ORDER BY` is refused
    /// (fixture 10, X1: 002305; X2, X4, X5 and 10b X3: 002061). A frameless
    /// sum over the whole result is not (X6), nor `LAG` with an `ORDER BY`
    /// (10b, X7).
    #[test]
    fn an_order_sensitive_window_needs_an_order_by() {
        let unordered = |function: &str, frame: Option<&str>| {
            let mut def = view("title");
            let ws = def.metrics[1].window_spec.as_mut().expect("window");
            ws.window_function = function.to_string();
            ws.order_by.clear();
            ws.partition_dims = vec!["title".to_string()];
            ws.frame_clause = frame.map(str::to_string);
            def
        };
        let err = validate_window_dimensions(&unordered(
            "SUM",
            Some("ROWS BETWEEN 1 PRECEDING AND CURRENT ROW"),
        ))
        .expect_err("frame without ORDER BY");
        assert!(
            err.message.contains("frame requires an ORDER BY"),
            "{}",
            err.message
        );
        for function in super::ORDER_SENSITIVE.iter().chain(&["lag", "First_Value"]) {
            let err = validate_window_dimensions(&unordered(function, None))
                .expect_err("order-sensitive without ORDER BY");
            assert!(
                err.message.contains("requires ORDER BY"),
                "{function}: {}",
                err.message
            );
        }
        for function in ["SUM", "AVG", "COUNT", "MIN", "MAX"] {
            assert!(
                validate_window_dimensions(&unordered(function, None)).is_ok(),
                "{function}"
            );
        }
        let mut def = view("title");
        def.metrics[1]
            .window_spec
            .as_mut()
            .expect("window")
            .window_function = "LAG".to_string();
        assert!(validate_window_dimensions(&def).is_ok());
    }

    /// A step back along a one-to-one relationship relates the two tables.
    #[test]
    fn a_one_to_one_step_back_relates_the_tables() {
        let mut def = view("aw.award");
        def.joins[4].cardinality = Cardinality::OneToOne;
        // awards -> publishers is one-to-one, so books -> publishers -> awards
        // steps forward, then back along it.
        assert!(validate_window_dimensions(&def).is_ok());
    }

    /// Reviewbot on #348: relatedness is measured from the inner metric's
    /// table, whose rows the window runs over. A window declared on the
    /// authors over the books' pages, ordered by the authors' country, orders
    /// books across the bridge, and is refused.
    #[test]
    fn a_window_is_related_through_its_inner_metrics_table() {
        let mut def = view("title");
        def.metrics[1].source_table = Some("a".to_string());
        def.metrics[1]
            .window_spec
            .as_mut()
            .expect("window")
            .order_by[0]
            .expr = "country".to_string();
        let err = validate_window_dimensions(&def).expect_err("books ordered by country");
        assert!(err.message.contains("'b'"), "{}", err.message);
        // ... and a window on the books over an author count, ordered by
        // country, runs over the authors' rows, and is accepted.
        let mut def = view("a.country");
        def.metrics.push(Metric {
            name: "authors".to_string(),
            expr: "COUNT(a.id)".to_string(),
            source_table: Some("a".to_string()),
            ..Default::default()
        });
        def.metrics[1]
            .window_spec
            .as_mut()
            .expect("window")
            .inner_metric = "authors".to_string();
        assert!(validate_window_dimensions(&def).is_ok());
    }

    /// ... and a derived inner metric is measured from every base table: a
    /// window on the authors over the books' doubled pages is refused too.
    #[test]
    fn a_window_over_a_derived_metric_is_related_through_its_bases() {
        let mut def = view("country");
        def.metrics.push(Metric {
            name: "double_pages".to_string(),
            expr: "pages * 2".to_string(),
            ..Default::default()
        });
        def.metrics[1].source_table = Some("a".to_string());
        def.metrics[1]
            .window_spec
            .as_mut()
            .expect("window")
            .inner_metric = "double_pages".to_string();
        assert!(validate_window_dimensions(&def).is_err());
    }

    /// Reviewbot on #348: a window's own `USING` applies to its inner metric,
    /// so a window with one over a metric on another table, or a derived one,
    /// is refused.
    #[test]
    fn a_windows_using_over_an_inner_metric_off_its_table_is_refused() {
        for (table, expr) in [(Some("p"), "COUNT(p.id)"), (None, "pages * 2")] {
            let mut def = view("title");
            def.metrics.push(Metric {
                name: "inner_m".to_string(),
                expr: expr.to_string(),
                source_table: table.map(str::to_string),
                ..Default::default()
            });
            def.metrics[1]
                .window_spec
                .as_mut()
                .expect("window")
                .inner_metric = "inner_m".to_string();
            def.metrics[1].using_relationships = vec!["b_p".to_string()];
            let err = validate_window_dimensions(&def).expect_err("USING off its table");
            assert!(err.message.contains("USING"), "{table:?}: {}", err.message);
        }
        // Control: the inner metric on the window's own table.
        let mut def = view("title");
        def.metrics[1].using_relationships = vec!["b_p".to_string()];
        assert!(validate_window_dimensions(&def).is_ok());
    }
}
