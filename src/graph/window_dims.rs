//! Window clause dimension validation (Phase 7 PR 5).
//!
//! A window metric's `PARTITION BY`, `PARTITION BY EXCLUDING` and `ORDER BY`
//! may only name a dimension of a table related to the metric's own: the
//! table itself, a table it reaches along foreign keys, or a table that
//! reaches it. Snowflake refuses one across a many-to-many bridge at CREATE
//! with `010211: A metric cannot refer to another dimension from an unrelated
//! entity` (Phase 7 fixture 3, N0): ordering books by their authors' country
//! names a value a book does not have one of.

use crate::errors::ParseError;
use crate::ident::{first_unquoted_dot, ident_matches};
use crate::model::{Cardinality, Dimension, SemanticViewDefinition};

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

/// Refuse a window metric whose window clause names a dimension on a table
/// that neither reaches the metric's table nor is reached from it.
///
/// A table referencing the metric's own (a fan-out, orders ordered by an
/// item's quantity) is accepted: the query answers it once per group (Phase 7
/// PR 4c), and fixture 10 (F*) asks Snowflake whether it accepts it too.
pub fn validate_window_dimensions(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    if def.joins.is_empty() {
        return Ok(());
    }
    for metric in &def.metrics {
        let (Some(ws), Some(table)) = (&metric.window_spec, metric.source_table.as_deref()) else {
            continue;
        };
        let refs = ws
            .excluding_dims
            .iter()
            .chain(&ws.partition_dims)
            .chain(ws.order_by.iter().map(|ob| &ob.expr));
        for reference in refs {
            let Some(dim) = find_dimension(def, reference) else {
                continue; // `cross_refs` reports an unknown dimension.
            };
            let Some(dim_table) = dim.source_table.as_deref() else {
                continue;
            };
            if !reaches(def, table, dim_table) && !reaches(def, dim_table, table) {
                return Err(ParseError::positionless(format!(
                    "Window metric '{}': its window clause names dimension '{}' of table \
                     '{dim_table}', which is unrelated to the metric's table '{table}': neither \
                     reaches the other along relationships (for example, they are joined \
                     through a many-to-many bridge). A window clause may name dimensions of \
                     '{table}', of a table it references, or of a table referencing it.",
                    metric.name, reference
                )));
            }
        }
    }
    Ok(())
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
        for reference in ["country", "a.country"] {
            let err = validate_window_dimensions(&view(reference))
                .expect_err("a dimension across a bridge is unrelated");
            assert!(
                err.message.contains("unrelated") && err.message.contains("'a'"),
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

    /// Controls: the metric's own table, a table above it and one below it.
    #[test]
    fn a_window_ordered_by_a_related_table_is_accepted() {
        for reference in ["title", "b.title", "publisher", "stars"] {
            assert!(
                validate_window_dimensions(&view(reference)).is_ok(),
                "{reference}"
            );
        }
    }

    /// A step back along a one-to-one relationship relates the two tables.
    #[test]
    fn a_one_to_one_step_back_relates_the_tables() {
        let mut def = view("award");
        def.joins[4].cardinality = Cardinality::OneToOne;
        // awards -> publishers is one-to-one, so books -> publishers -> awards
        // steps forward, then back along it.
        assert!(validate_window_dimensions(&def).is_ok());
    }
}
