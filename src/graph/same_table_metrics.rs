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
//! `seats`. The same holds inside an ordered-set aggregate's `WITHIN GROUP
//! (ORDER BY ...)`, and for a lambda's parameter. An aggregate is one `DuckDB`
//! ships (the list CREATE checks dimensions against); a name inside a call to
//! an aggregate from an extension or a macro is matched like any other.
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
use crate::query::expr_items::{is_lambda_param, lambda_scopes};

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
    let lambdas = lambda_scopes(expr);
    scan_references_outside_subqueries(expr)
        .into_iter()
        .filter(|r| {
            !aggregate_spans
                .iter()
                .any(|&(s, e)| r.start >= s && r.end <= e)
        })
        .filter(|r| !is_lambda_param(r, &lambdas))
        .filter_map(|r| {
            keys.get(&r.key()).map(|&target| MetricRef {
                start: r.start,
                end: r.end,
                target,
            })
        })
        .collect()
}

/// How deep a chain of metrics written over metrics may go, as for derived
/// metrics (`expand::facts`' `MAX_DERIVATION_DEPTH`).
const MAX_REFERENCE_DEPTH: usize = 64;

/// The longest expression inlining may build. A metric that references
/// another twice doubles its text at each step of a chain.
const MAX_INLINED_LEN: usize = 1 << 20;

/// Whether `target` can be inlined as the aggregate it computes: not a window
/// metric (it yields a value per row), not semi-additive (its `NON ADDITIVE
/// BY` would be lost), and without `USING` of its own (the path belongs to the
/// metric written over it, which may name a different one).
fn refusal(target: &Metric) -> Option<&'static str> {
    if target.is_window() {
        Some("is a window metric")
    } else if !target.non_additive_by.is_empty() {
        Some("is semi-additive (NON ADDITIVE BY)")
    } else if !target.using_relationships.is_empty() {
        Some("has USING of its own; give the USING to the metric written over it instead")
    } else {
        None
    }
}

/// Why inlining stopped.
enum Stop {
    /// `def.metrics[i]` is on a cycle.
    Cycle(usize),
    TooDeep,
    TooLong,
}

/// Refuse a metric over a same-table metric that cannot be inlined as an
/// aggregate ([`refusal`]), a cycle of such references, and a chain deeper
/// than [`MAX_REFERENCE_DEPTH`] or longer, inlined, than [`MAX_INLINED_LEN`].
///
/// # Errors
///
/// A `ParseError` naming both metrics, the cycle or the limit.
pub fn validate_same_table_metric_refs(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    let refs: Vec<Vec<MetricRef>> = (0..def.metrics.len())
        .map(|i| same_table_refs(def, i))
        .collect();
    for (i, met_refs) in refs.iter().enumerate() {
        for r in met_refs {
            let target = &def.metrics[r.target];
            if let Some(why) = refusal(target) {
                return Err(ParseError::positionless(format!(
                    "metric '{}' references metric '{}' on its own table, which {why}. \
                     A metric can be written over a plain metric on its own table only.",
                    def.metrics[i].name, target.name
                )));
            }
        }
    }
    let mut state = State::new(def.metrics.len());
    for i in 0..def.metrics.len() {
        if let Err(stop) = resolve(def, &refs, i, &mut state, 0) {
            let message = match stop {
                Stop::Cycle(at) => {
                    let from = state.stack.iter().position(|&n| n == at).unwrap_or(0);
                    let names: Vec<&str> = state.stack[from..]
                        .iter()
                        .map(|&n| def.metrics[n].name.as_str())
                        .collect();
                    format!(
                        "cycle in metrics written over metrics on their own table: [{}]",
                        names.join(", ")
                    )
                }
                Stop::TooDeep => format!(
                    "metric '{}' is written over a chain of metrics more than \
                     {MAX_REFERENCE_DEPTH} deep",
                    def.metrics[i].name
                ),
                Stop::TooLong => format!(
                    "metric '{}' is written over metrics whose expressions, inlined, \
                     exceed {MAX_INLINED_LEN} bytes",
                    def.metrics[i].name
                ),
            };
            return Err(ParseError::positionless(message));
        }
    }
    Ok(())
}

/// `def` with every same-table metric reference replaced by the referenced
/// metric's own expression (parenthesized, unless it is the whole expression);
/// borrowed unchanged when no metric has one.
///
/// A catalog row written before CREATE checked these references can hold one
/// the check refuses ([`refusal`]), a cycle, or a chain past the limits. Such
/// a reference is left as written, as it was read before, and fails to bind as
/// the column it reads as rather than losing the referenced metric's
/// `NON ADDITIVE BY` or path.
#[must_use]
pub(crate) fn inline_same_table_metrics(
    def: &SemanticViewDefinition,
) -> Cow<'_, SemanticViewDefinition> {
    let refs: Vec<Vec<MetricRef>> = (0..def.metrics.len())
        .map(|i| {
            same_table_refs(def, i)
                .into_iter()
                .filter(|r| refusal(&def.metrics[r.target]).is_none())
                .collect()
        })
        .collect();
    if refs.iter().all(Vec::is_empty) {
        return Cow::Borrowed(def);
    }
    let mut state = State::new(def.metrics.len());
    for i in 0..def.metrics.len() {
        // A stop leaves the metric (and whatever it was resolving) as written.
        let _ = resolve(def, &refs, i, &mut state, 0);
        state.stack.clear();
        state.on_stack.iter_mut().for_each(|b| *b = false);
    }
    let mut out = def.clone();
    for (met, expr) in out.metrics.iter_mut().zip(state.resolved) {
        if let Some((expr, _)) = expr {
            met.expr = expr;
        }
    }
    Cow::Owned(out)
}

/// The resolver's working state: each metric's inlined expression once known,
/// and the chain being resolved.
struct State {
    /// Each metric's inlined expression and the height of the chain below it.
    resolved: Vec<Option<(String, usize)>>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
}

impl State {
    fn new(n: usize) -> Self {
        Self {
            resolved: vec![None; n],
            on_stack: vec![false; n],
            stack: Vec::new(),
        }
    }
}

/// The expression of `def.metrics[i]` with its references inlined, and the
/// height of the chain of references below it, memoized in `state.resolved`.
/// `depth` bounds the recursion itself. On a stop, `state.stack` holds the
/// chain that reached it.
fn resolve(
    def: &SemanticViewDefinition,
    refs: &[Vec<MetricRef>],
    i: usize,
    state: &mut State,
    depth: usize,
) -> Result<(String, usize), Stop> {
    if let Some(done) = &state.resolved[i] {
        return Ok(done.clone());
    }
    if state.on_stack[i] {
        return Err(Stop::Cycle(i));
    }
    if depth > MAX_REFERENCE_DEPTH {
        return Err(Stop::TooDeep);
    }
    state.on_stack[i] = true;
    state.stack.push(i);
    let expr = &def.metrics[i].expr;
    let whole =
        |r: &MetricRef| expr[..r.start].trim().is_empty() && expr[r.end..].trim().is_empty();
    let mut out = String::with_capacity(expr.len());
    let mut copied = 0;
    let mut height = 0;
    for r in &refs[i] {
        let (inner, below) = resolve(def, refs, r.target, state, depth + 1)?;
        height = height.max(below + 1);
        if height > MAX_REFERENCE_DEPTH {
            return Err(Stop::TooDeep);
        }
        out.push_str(&expr[copied..r.start]);
        // A reference that is the whole expression needs no parentheses, and
        // takes none, so a semi-additive metric written over a plain one reads
        // as the single aggregate call its snapshot expansion parses.
        if whole(r) {
            out.push_str(&inner);
        } else {
            out.push('(');
            out.push_str(&inner);
            out.push(')');
        }
        copied = r.end;
        if out.len() > MAX_INLINED_LEN {
            return Err(Stop::TooLong);
        }
    }
    out.push_str(&expr[copied..]);
    if out.len() > MAX_INLINED_LEN {
        return Err(Stop::TooLong);
    }
    state.on_stack[i] = false;
    state.stack.pop();
    state.resolved[i] = Some((out.clone(), height));
    Ok((out, height))
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
        assert_eq!(inlined(&d, "q"), "COUNT(*)");
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
    fn a_reference_in_within_group_or_a_lambda_is_not_a_metric() {
        let d = def(vec![
            metric("fare", "SUM(f.fare)", &[]),
            metric("a", "COUNT(*)", &[]),
            metric(
                "median_fare",
                "PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY f.fare)",
                &[],
            ),
            metric(
                "folded",
                "list_reduce(array_agg(f.v), lambda a, b: a + b)",
                &[],
            ),
        ]);
        assert!(matches!(inline_same_table_metrics(&d), Cow::Borrowed(_)));
    }

    #[test]
    fn a_whole_expression_reference_takes_no_parentheses() {
        let mut latest = metric("latest", "f.total", &[]);
        latest.non_additive_by = vec![NonAdditiveDim::default()];
        let d = def(vec![metric("total", "SUM(f.bal)", &[]), latest]);
        assert_eq!(inlined(&d, "latest"), "SUM(f.bal)");
        assert!(validate_same_table_metric_refs(&d).is_ok());
    }

    #[test]
    fn a_stored_reference_the_check_refuses_is_left_as_written() {
        let mut bal = metric("bal", "SUM(f.x)", &[]);
        bal.non_additive_by = vec![NonAdditiveDim::default()];
        let d = def(vec![
            bal,
            metric("routed", "COUNT(*)", &["r"]),
            metric("twice", "f.bal * 2", &[]),
            metric("via", "f.routed + 1", &[]),
        ]);
        assert!(matches!(inline_same_table_metrics(&d), Cow::Borrowed(_)));
    }

    #[test]
    fn validation_refuses_a_chain_past_the_depth_limit() {
        let mut metrics = vec![metric("m0", "COUNT(*)", &[])];
        for k in 1..=MAX_REFERENCE_DEPTH + 2 {
            metrics.push(metric(&format!("m{k}"), &format!("f.m{} + 1", k - 1), &[]));
        }
        let d = def(metrics);
        let err = validate_same_table_metric_refs(&d).unwrap_err().message;
        assert!(err.contains("deep"), "{err}");
        // Expansion leaves it as written rather than overflowing.
        let _ = inline_same_table_metrics(&d);
    }

    #[test]
    fn validation_refuses_a_chain_that_inlines_too_long() {
        let mut metrics = vec![metric("m0", "COUNT(*)", &[])];
        for k in 1..=24 {
            let prev = format!("f.m{}", k - 1);
            metrics.push(metric(&format!("m{k}"), &format!("{prev} + {prev}"), &[]));
        }
        let d = def(metrics);
        let err = validate_same_table_metric_refs(&d).unwrap_err().message;
        assert!(err.contains("exceed"), "{err}");
        let out = inline_same_table_metrics(&d);
        assert_eq!(out.metrics[24].expr, "f.m23 + f.m23");
    }

    #[test]
    fn a_reference_inside_a_subquery_or_a_literal_is_left_alone() {
        let d = def(vec![
            metric("n", "COUNT(*)", &[]),
            metric("q", "COUNT(*) + (SELECT n FROM t) + LENGTH('n')", &[]),
        ]);
        assert!(matches!(inline_same_table_metrics(&d), Cow::Borrowed(_)));
    }

    // fuzz_sql_expand (PR #323 CI): an unterminated quoted identifier is not
    // a reference, even when it reads as the name of a metric called `"`.
    // Inlining it there turned `x + "` into `x + ()`.
    #[test]
    fn an_unterminated_quoted_identifier_is_not_a_reference() {
        let d = def(vec![
            metric("\"", "COUNT(*)", &[]),
            metric("q", "COUNT(*) + \"", &[]),
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
        assert_eq!(inlined(&d, "q"), "(COUNT(*)) * 2");

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
