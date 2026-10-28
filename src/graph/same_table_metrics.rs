//! Metrics written over other metrics on their own table (Phase 6.3).
//!
//! Snowflake's [validation
//! rules](https://docs.snowflake.com/en/user-guide/views-semantic/validation-rules)
//! let a member expression "refer to base table columns or other expressions
//! on the same logical table", and its multi-path `USING` documentation builds
//! on exactly that: `flights.m_departures USING (flight_departure_airport) AS
//! flights.m_flight_count`, a metric over another metric on its table, computed
//! through the referencing metric's own relationship.
//!
//! A reference to a same-table metric is found the way fact references are
//! (bare, or qualified by the metric's own table), with one more rule: a
//! reference inside an aggregate call (its argument list, or a `FILTER` that
//! belongs to it) is a column, as it always was. An aggregate over an
//! aggregate does not run, so no expression that worked before changes
//! meaning: `SUM(f.seats)` keeps summing the column even beside a metric named
//! `seats`.
//!
//! Expansion inlines each referenced metric's expression in place
//! ([`inline_same_table_metrics`]), once, ahead of everything else, so every
//! later step (fact inlining, the `USING` requalification, the `COUNT(*)`
//! guard, the fan-trap and role-playing checks) sees the aggregate the metric
//! really computes.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::errors::ParseError;
use crate::expr_tokens::{call_end, scan_function_heads, scan_references_outside_subqueries};
use crate::ident::normalize_ident_part;
use crate::model::{Metric, SemanticViewDefinition};

use super::derived_metrics::aggregate_name;

/// One same-table metric reference: the byte span of the reference in the
/// referencing metric's expression, and the index of the metric it names.
struct MetricRef {
    start: usize,
    end: usize,
    target: usize,
}

/// Whether `met` may be written over other metrics on its table: a base
/// metric that is not a window metric. A window metric already names the
/// metric it windows, by its own rules.
fn can_reference(met: &Metric) -> bool {
    met.source_table.is_some() && !met.is_window()
}

/// The same-table metric references in `def.metrics[idx]`'s expression, in
/// source order. Empty for a metric that [`can_reference`] rules out.
fn same_table_refs(def: &SemanticViewDefinition, idx: usize) -> Vec<MetricRef> {
    let met = &def.metrics[idx];
    let Some(table) = met.source_table.as_deref().filter(|_| can_reference(met)) else {
        return Vec::new();
    };
    let table_key = normalize_ident_part(table);
    // Keyed by both spellings a reference may take: `name` and `table.name`.
    let mut keys: HashMap<String, usize> = HashMap::new();
    for (j, other) in def.metrics.iter().enumerate() {
        let same_table = other
            .source_table
            .as_deref()
            .is_some_and(|t| normalize_ident_part(t) == table_key);
        if j != idx && same_table {
            keys.entry(normalize_ident_part(&other.name)).or_insert(j);
            keys.entry(normalize_ident_part(&format!("{table}.{}", other.name)))
                .or_insert(j);
        }
    }
    if keys.is_empty() {
        return Vec::new();
    }
    let expr = met.expr.as_str();
    let aggregate_spans: Vec<(usize, usize)> = scan_function_heads(expr)
        .iter()
        .filter(|head| aggregate_name(&head.last_part_key()).is_some())
        .map(|head| (head.start, call_end(expr, head).unwrap_or(expr.len())))
        .collect();
    scan_references_outside_subqueries(expr)
        .into_iter()
        .filter(|r| {
            !aggregate_spans
                .iter()
                .any(|&(s, e)| r.start >= s && r.end <= e)
        })
        .filter_map(|r| {
            keys.get(&r.key()).map(|&target| MetricRef {
                start: r.start,
                end: r.end,
                target,
            })
        })
        .collect()
}

/// Refuse a metric over a same-table metric that cannot be inlined as an
/// aggregate, and a cycle of such references.
///
/// The referenced metric must be a plain one: not a window metric (it yields
/// a value per row), not semi-additive (its `NON ADDITIVE BY` would be lost),
/// and without `USING` of its own (the path belongs to the metric written
/// over it, which may name a different one).
///
/// # Errors
///
/// A `ParseError` naming both metrics, or the cycle.
pub fn validate_same_table_metric_refs(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    let refs: Vec<Vec<MetricRef>> = (0..def.metrics.len())
        .map(|i| same_table_refs(def, i))
        .collect();
    for (i, met_refs) in refs.iter().enumerate() {
        let met = &def.metrics[i].name;
        for r in met_refs {
            let target = &def.metrics[r.target];
            let why = if target.is_window() {
                "is a window metric"
            } else if !target.non_additive_by.is_empty() {
                "is semi-additive (NON ADDITIVE BY)"
            } else if !target.using_relationships.is_empty() {
                "has USING of its own; give the USING to the metric written over it instead"
            } else {
                continue;
            };
            return Err(ParseError::positionless(format!(
                "metric '{met}' references metric '{}' on its own table, which {why}. \
                 A metric can be written over a plain metric on its own table only.",
                target.name
            )));
        }
    }
    // Depth-first search for a cycle: 0 unvisited, 1 on the stack, 2 done.
    let mut state = vec![0u8; def.metrics.len()];
    for start in 0..def.metrics.len() {
        if let Some(cycle) = find_cycle(start, &refs, &mut state, &mut Vec::new()) {
            let names: Vec<&str> = cycle
                .iter()
                .map(|&i| def.metrics[i].name.as_str())
                .collect();
            return Err(ParseError::positionless(format!(
                "cycle in metrics written over metrics on their own table: [{}]",
                names.join(", ")
            )));
        }
    }
    Ok(())
}

fn find_cycle(
    node: usize,
    refs: &[Vec<MetricRef>],
    state: &mut [u8],
    stack: &mut Vec<usize>,
) -> Option<Vec<usize>> {
    match state[node] {
        2 => return None,
        1 => {
            let from = stack.iter().position(|&n| n == node).unwrap_or(0);
            return Some(stack[from..].to_vec());
        }
        _ => {}
    }
    state[node] = 1;
    stack.push(node);
    for r in &refs[node] {
        if let Some(cycle) = find_cycle(r.target, refs, state, stack) {
            return Some(cycle);
        }
    }
    stack.pop();
    state[node] = 2;
    None
}

/// `def` with every same-table metric reference replaced by the referenced
/// metric's own expression, parenthesized; borrowed unchanged when no metric
/// has one.
///
/// A cycle (refused at CREATE, so only reachable from a catalog row written
/// before that check) leaves the metrics on it as written, and the reference
/// then fails to bind as the column it reads as.
#[must_use]
pub(crate) fn inline_same_table_metrics(
    def: &SemanticViewDefinition,
) -> Cow<'_, SemanticViewDefinition> {
    let refs: Vec<Vec<MetricRef>> = (0..def.metrics.len())
        .map(|i| same_table_refs(def, i))
        .collect();
    if refs.iter().all(Vec::is_empty) {
        return Cow::Borrowed(def);
    }
    let mut resolved: Vec<Option<String>> = vec![None; def.metrics.len()];
    let mut on_stack = vec![false; def.metrics.len()];
    for i in 0..def.metrics.len() {
        resolve(def, &refs, i, &mut resolved, &mut on_stack);
    }
    let mut out = def.clone();
    for (met, expr) in out.metrics.iter_mut().zip(resolved) {
        if let Some(expr) = expr {
            met.expr = expr;
        }
    }
    Cow::Owned(out)
}

/// The expression of `def.metrics[i]` with its references inlined, memoized in
/// `resolved`. `None` while `i` sits on a cycle.
fn resolve(
    def: &SemanticViewDefinition,
    refs: &[Vec<MetricRef>],
    i: usize,
    resolved: &mut [Option<String>],
    on_stack: &mut [bool],
) -> Option<String> {
    if let Some(done) = &resolved[i] {
        return Some(done.clone());
    }
    if on_stack[i] {
        return None;
    }
    on_stack[i] = true;
    let expr = &def.metrics[i].expr;
    let mut out = String::with_capacity(expr.len());
    let mut copied = 0;
    let mut complete = true;
    for r in &refs[i] {
        let Some(inner) = resolve(def, refs, r.target, resolved, on_stack) else {
            complete = false;
            break;
        };
        out.push_str(&expr[copied..r.start]);
        out.push('(');
        out.push_str(&inner);
        out.push(')');
        copied = r.end;
    }
    on_stack[i] = false;
    if !complete {
        return None;
    }
    out.push_str(&expr[copied..]);
    resolved[i] = Some(out.clone());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NonAdditiveDim, TableRef};

    fn metric(name: &str, expr: &str, using: &[&str]) -> Metric {
        Metric {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some("f".to_string()),
            using_relationships: using.iter().map(|r| (*r).to_string()).collect(),
            ..Default::default()
        }
    }

    fn def(metrics: Vec<Metric>) -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "f".to_string(),
                table: "flights".to_string(),
                ..Default::default()
            }],
            metrics,
            ..Default::default()
        }
    }

    fn inlined(def: &SemanticViewDefinition, name: &str) -> String {
        let out = inline_same_table_metrics(def);
        out.metrics
            .iter()
            .find(|m| m.name == name)
            .map(|m| m.expr.clone())
            .unwrap()
    }

    #[test]
    fn qualified_bare_and_quoted_references_are_inlined() {
        let d = def(vec![
            metric("n", "COUNT(*)", &[]),
            metric("q", "f.n", &["r"]),
            metric("b", "n + 1", &[]),
            metric("c", "\"F\".\"N\" * 2", &[]),
        ]);
        assert_eq!(inlined(&d, "q"), "(COUNT(*))");
        assert_eq!(inlined(&d, "b"), "(COUNT(*)) + 1");
        assert_eq!(inlined(&d, "c"), "(COUNT(*)) * 2");
    }

    #[test]
    fn a_reference_inside_an_aggregate_or_its_filter_is_a_column() {
        let d = def(vec![
            metric("seats", "SUM(f.seats)", &[]),
            metric("s", "SUM(f.seats) FILTER (WHERE seats > 0)", &[]),
        ]);
        assert!(matches!(inline_same_table_metrics(&d), Cow::Borrowed(_)));
    }

    #[test]
    fn a_reference_inside_a_subquery_or_a_literal_is_left_alone() {
        let d = def(vec![
            metric("n", "COUNT(*)", &[]),
            metric("q", "COUNT(*) + (SELECT n FROM t) + LENGTH('n')", &[]),
        ]);
        assert!(matches!(inline_same_table_metrics(&d), Cow::Borrowed(_)));
    }

    #[test]
    fn a_reference_to_another_tables_metric_is_not_inlined() {
        let mut other = metric("n", "COUNT(*)", &[]);
        other.source_table = Some("a".to_string());
        let d = def(vec![other, metric("q", "a.n", &[])]);
        assert!(matches!(inline_same_table_metrics(&d), Cow::Borrowed(_)));
    }

    #[test]
    fn a_window_metric_is_neither_inlined_into_nor_inlined() {
        let mut w = metric("w", "SUM(n)", &[]);
        w.window_spec = Some(crate::model::WindowSpec::default());
        let d = def(vec![metric("n", "COUNT(*)", &[]), w]);
        assert_eq!(inlined(&d, "w"), "SUM(n)");
    }

    #[test]
    fn chains_resolve_and_cycles_are_left_as_written() {
        let d = def(vec![
            metric("n", "COUNT(*)", &[]),
            metric("m", "f.n", &[]),
            metric("q", "f.m * 2", &[]),
        ]);
        assert_eq!(inlined(&d, "q"), "((COUNT(*))) * 2");

        let cyc = def(vec![
            metric("a", "f.b + 1", &[]),
            metric("b", "f.a + 1", &[]),
        ]);
        assert_eq!(inlined(&cyc, "a"), "f.b + 1");
    }

    #[test]
    fn validation_accepts_the_documented_pattern() {
        let d = def(vec![
            metric("n", "COUNT(*)", &[]),
            metric("deps", "f.n", &["fdep"]),
        ]);
        assert!(validate_same_table_metric_refs(&d).is_ok());
    }

    #[test]
    fn validation_refuses_a_reference_to_a_metric_with_using() {
        let d = def(vec![
            metric("n", "COUNT(*)", &["farr"]),
            metric("deps", "f.n", &["fdep"]),
        ]);
        let err = validate_same_table_metric_refs(&d).unwrap_err().message;
        assert!(
            err.contains("'deps'") && err.contains("'n'") && err.contains("USING"),
            "{err}"
        );
    }

    #[test]
    fn validation_refuses_a_reference_to_a_semi_additive_metric() {
        let mut n = metric("n", "SUM(bal)", &[]);
        n.non_additive_by = vec![NonAdditiveDim::default()];
        let d = def(vec![n, metric("q", "f.n", &[])]);
        let err = validate_same_table_metric_refs(&d).unwrap_err().message;
        assert!(err.contains("semi-additive"), "{err}");
    }

    #[test]
    fn validation_refuses_a_reference_to_a_window_metric() {
        let mut w = metric("w", "SUM(x)", &[]);
        w.window_spec = Some(crate::model::WindowSpec::default());
        let d = def(vec![w, metric("q", "f.w", &[])]);
        let err = validate_same_table_metric_refs(&d).unwrap_err().message;
        assert!(err.contains("window metric"), "{err}");
    }

    #[test]
    fn validation_refuses_a_cycle() {
        let d = def(vec![
            metric("a", "f.b + 1", &[]),
            metric("b", "f.c + 1", &[]),
            metric("c", "f.a + 1", &[]),
        ]);
        let err = validate_same_table_metric_refs(&d).unwrap_err().message;
        assert!(err.contains("cycle") && err.contains("a, b, c"), "{err}");
    }
}
