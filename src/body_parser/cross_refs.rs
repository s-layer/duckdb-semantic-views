//! Semantic cross-reference validation, shared by the two paths that can
//! produce a [`crate::model::SemanticViewDefinition`].
//!
//! MODEL-1 (code-review 2026-08-08): these checks used to live inline in
//! [`super::parse_keyword_body`], which meant they ran for DDL and **not** for
//! YAML import — the one surface that reaches the model without going through
//! the clause parsers. A YAML definition naming a dimension that does not
//! exist therefore imported cleanly and then rendered `GET_DDL` that this
//! project's own parser rejects, breaking the dump/restore contract
//! `validate_ddl_representable` exists to hold.
//!
//! They live under `body_parser` because the rule they enforce is the body
//! parser's — "a reference must resolve to a declared member" — and because
//! the DDL caller has the vectors in hand before it has a definition. Both
//! callers get the SAME function so the two paths cannot drift again:
//! [`super::parse_keyword_body`] (wrapping the message in a positionless
//! `ParseError`) and [`crate::model::validate_ddl_representable`].
//!
//! Every message here is byte-identical to the one the inline check produced,
//! so the DDL path's diagnostics — and the tests pinning them — are unchanged.

use super::scan::split_qualified_identifier;
use crate::ident::ident_matches;
use crate::model::{Dimension, Materialization, Metric};
use crate::util::suggest_closest;
use std::fmt::Write;

/// Append `" Did you mean 'x'?"` when a close declared name exists.
fn with_suggestion(mut msg: String, needle: &str, haystack: &[String]) -> String {
    if let Some(closest) = suggest_closest(needle, haystack) {
        let _ = write!(msg, " Did you mean '{closest}'?");
    }
    msg
}

fn dimension_names(dimensions: &[Dimension]) -> Vec<String> {
    dimensions.iter().map(|d| d.name.clone()).collect()
}

fn metric_names(metrics: &[Metric]) -> Vec<String> {
    metrics.iter().map(|m| m.name.clone()).collect()
}

/// Does `reference` name one of `dimensions`, either bare or in the D-08
/// dotted `alias.dim_name` form?
///
/// PARSE-8: `ident_matches` on both halves — the project's one identifier
/// rule — so a quoted reference resolves to its unquoted declaration.
fn dimension_exists(dimensions: &[Dimension], reference: &str) -> bool {
    dimensions.iter().any(|d| {
        if ident_matches(&d.name, reference) {
            return true;
        }
        if let Some((alias_part, name_part)) = split_qualified_identifier(reference) {
            if let Some(ref src) = d.source_table {
                return ident_matches(src, alias_part) && ident_matches(&d.name, name_part);
            }
        }
        false
    })
}

/// Phase 47 / Phase 68 B1 (D-08): every `NON ADDITIVE BY` dimension must name
/// a declared dimension, bare or dotted.
fn validate_non_additive_by(dimensions: &[Dimension], metrics: &[Metric]) -> Result<(), String> {
    for metric in metrics {
        for na in &metric.non_additive_by {
            if !dimension_exists(dimensions, &na.dimension) {
                return Err(with_suggestion(
                    format!(
                        "NON ADDITIVE BY dimension '{}' on metric '{}' does not match any declared dimension.",
                        na.dimension, metric.name
                    ),
                    &na.dimension,
                    &dimension_names(dimensions),
                ));
            }
        }
    }
    Ok(())
}

/// Phase 48: a window metric's EXCLUDING / PARTITION BY / ORDER BY dimensions
/// and its inner metric must all name declared members.
fn validate_window_references(dimensions: &[Dimension], metrics: &[Metric]) -> Result<(), String> {
    let met_names = metric_names(metrics);
    for metric in metrics {
        let Some(ref ws) = metric.window_spec else {
            continue;
        };
        // Phase 7 PR 5: every window slot takes the D-08 dotted form, as
        // Snowflake does (fixture 4, W0); EXCLUDING and PARTITION BY used to
        // take the bare form only.
        for dim in &ws.excluding_dims {
            if !dimension_exists(dimensions, dim) {
                return Err(with_suggestion(
                    format!(
                        "Window metric '{}': EXCLUDING dimension '{}' not found in semantic view dimensions.",
                        metric.name, dim
                    ),
                    dim,
                    &dimension_names(dimensions),
                ));
            }
        }
        for dim in &ws.partition_dims {
            if !dimension_exists(dimensions, dim) {
                return Err(with_suggestion(
                    format!(
                        "Window metric '{}': PARTITION BY dimension '{}' not found in semantic view dimensions.",
                        metric.name, dim
                    ),
                    dim,
                    &dimension_names(dimensions),
                ));
            }
        }
        for ob in &ws.order_by {
            if !dimension_exists(dimensions, &ob.expr) {
                return Err(with_suggestion(
                    format!(
                        "Window metric '{}': ORDER BY dimension '{}' not found in semantic view dimensions.",
                        metric.name, ob.expr
                    ),
                    &ob.expr,
                    &dimension_names(dimensions),
                ));
            }
        }
        if !met_names.iter().any(|n| ident_matches(n, &ws.inner_metric)) {
            return Err(with_suggestion(
                format!(
                    "Window metric '{}': inner metric '{}' not found in semantic view metrics.",
                    metric.name, ws.inner_metric
                ),
                &ws.inner_metric,
                &met_names,
            ));
        }
        // Phase 7 PR 5: a window is computed over an aggregate metric's
        // result, so its inner metric cannot be a window metric itself. The
        // query refuses one too (`ExpandError::WindowOverWindow`).
        if metrics
            .iter()
            .any(|m| m.is_window() && ident_matches(&m.name, &ws.inner_metric))
        {
            return Err(format!(
                "Window metric '{}': inner metric '{}' is itself a window function metric. \
                 A window metric must be computed over an aggregate metric.",
                metric.name, ws.inner_metric
            ));
        }
    }
    Ok(())
}

/// Phase 54: materialization names must be unique (case-folded), and every
/// dimension / metric a materialization lists must be declared.
fn validate_materialization_references(
    dimensions: &[Dimension],
    metrics: &[Metric],
    materializations: &[Materialization],
) -> Result<(), String> {
    let mut seen_names: Vec<String> = Vec::new();
    for mat in materializations {
        let lower = mat.name.to_ascii_lowercase();
        if seen_names.iter().any(|n| n == &lower) {
            return Err(format!("Duplicate materialization name '{}'.", mat.name));
        }
        seen_names.push(lower);
    }
    let met_names = metric_names(metrics);
    for mat in materializations {
        for dim_name in &mat.dimensions {
            if !dimensions.iter().any(|d| ident_matches(&d.name, dim_name)) {
                return Err(with_suggestion(
                    format!(
                        "Materialization '{}': dimension '{}' not found in semantic view dimensions.",
                        mat.name, dim_name
                    ),
                    dim_name,
                    &dimension_names(dimensions),
                ));
            }
        }
        for met_name in &mat.metrics {
            if !metrics.iter().any(|m| ident_matches(&m.name, met_name)) {
                return Err(with_suggestion(
                    format!(
                        "Materialization '{}': metric '{}' not found in semantic view metrics.",
                        mat.name, met_name
                    ),
                    met_name,
                    &met_names,
                ));
            }
        }
    }
    Ok(())
}

/// Run every cross-reference check, in the order the inline DDL-path code ran
/// them (NON ADDITIVE BY, then window references, then materializations), so
/// which error a multiply-broken definition reports does not change.
pub(crate) fn validate_cross_references(
    dimensions: &[Dimension],
    metrics: &[Metric],
    materializations: &[Materialization],
) -> Result<(), String> {
    validate_non_additive_by(dimensions, metrics)?;
    validate_window_references(dimensions, metrics)?;
    validate_materialization_references(dimensions, metrics, materializations)
}
