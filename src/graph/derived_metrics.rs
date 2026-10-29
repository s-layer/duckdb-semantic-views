//! Derived metric validation (Phase 30).
//!
//! Validates derived metric uniqueness, aggregate prohibition, reference validity,
//! and cycle detection in the derived metric dependency graph.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;

use crate::errors::ParseError;
use crate::model::SemanticViewDefinition;
use crate::util::suggest_closest;

use super::facts::find_fact_references;

/// Known SQL aggregate function names (lowercase).
const AGGREGATE_FUNCTIONS: &[&str] = &[
    "sum",
    "count",
    "avg",
    "min",
    "max",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "variance",
    "var_pop",
    "var_samp",
    "string_agg",
    "listagg",
    "group_concat",
    "array_agg",
    "any_value",
    "approx_count_distinct",
    "median",
    "mode",
    "percentile_cont",
    "percentile_disc",
    "corr",
    "covar_pop",
    "covar_samp",
    "regr_avgx",
    "regr_avgy",
    "regr_count",
    "regr_intercept",
    "regr_r2",
    "regr_slope",
    "regr_sxx",
    "regr_sxy",
    "regr_syy",
    "bit_and",
    "bit_or",
    "bit_xor",
    "bool_and",
    "bool_or",
    // DuckDB's own aggregates (`duckdb_functions()` lists them; the
    // `every_duckdb_aggregate_is_known` test in `member_exprs` keeps this
    // list complete, and `expand::facts`' phantom-row guard uses it too).
    "approx_quantile",
    "approx_top_k",
    "arbitrary",
    "arg_max",
    "arg_max_null",
    "arg_max_nulls_last",
    "arg_min",
    "arg_min_null",
    "arg_min_nulls_last",
    "argmax",
    "argmin",
    "bitstring_agg",
    "count_if",
    "count_star",
    "countif",
    "entropy",
    "favg",
    "first",
    "fsum",
    "histogram",
    "histogram_exact",
    "kahan_sum",
    "kurtosis",
    "kurtosis_pop",
    "last",
    "list",
    "mad",
    "max_by",
    "mean",
    "min_by",
    "product",
    "quantile",
    "quantile_cont",
    "quantile_disc",
    "reservoir_quantile",
    "sem",
    "skewness",
    "sum_no_overflow",
    "sumkahan",
];

/// Check if an expression contains an aggregate function call.
///
/// Uses the shared reference tokenizer ([`crate::expr_tokens::scan_function_heads`])
/// to find every function-call head, then matches the head's *last* identifier
/// part (the called name — bare `sum(` or schema-qualified `main.sum(`) against
/// the known-aggregate set. The last part is taken quote-aware
/// ([`IdentRef::last_part_key`](crate::expr_tokens::IdentRef::last_part_key)),
/// so a function whose name is a *quoted* identifier containing a dot
/// (`"main.sum"(x)`) is one part `main.sum` and is not mistaken for the
/// aggregate `sum`. Because it rides the tokenizer, it also inherits correct
/// literal handling — a `sum(` inside a `'…'` / `$tag$…$` string is not a call —
/// and the single identifier-byte rule (E-5: `Ωsum(` is the one identifier
/// `Ωsum`, not the aggregate `sum`).
///
/// Returns the first aggregate function found in source order (lowercase), or
/// `None`.
#[must_use]
pub fn contains_aggregate_function(expr: &str) -> Option<&'static str> {
    crate::expr_tokens::scan_function_heads(expr)
        .into_iter()
        .find_map(|head| aggregate_name(&head.last_part_key()))
}

/// `name` (a lowercase function-name key) if it is a known aggregate.
pub(crate) fn aggregate_name(name: &str) -> Option<&'static str> {
    AGGREGATE_FUNCTIONS.iter().copied().find(|&f| f == name)
}

/// Validate derived metrics in a semantic view definition.
///
/// Checks:
/// 1. No duplicate metric names across base and derived (case-insensitive).
/// 2. Derived metrics must not contain aggregate function calls.
/// 3. All metric names referenced in derived metric expressions must exist.
/// 4. The derived metric dependency graph has no cycles (Kahn's algorithm).
///
/// Returns `Ok(())` if valid, `Err` with descriptive message otherwise.
#[allow(clippy::too_many_lines)]
pub fn validate_derived_metrics(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    let derived: Vec<&crate::model::Metric> = def
        .metrics
        .iter()
        .filter(|m| m.source_table.is_none())
        .collect();

    if derived.is_empty() {
        return Ok(());
    }

    // 1. Check metric name uniqueness (case-insensitive)
    check_metric_name_uniqueness(def).map_err(ParseError::positionless)?;

    // 2. Check for aggregate functions in derived metrics
    check_no_aggregates_in_derived(&derived).map_err(ParseError::positionless)?;

    // 3. Check for unknown metric references in derived expressions
    let all_metric_names: Vec<&str> = def.metrics.iter().map(|m| m.name.as_str()).collect();
    let all_metric_names_display: Vec<String> = all_metric_names
        .iter()
        .copied()
        .map(ToString::to_string)
        .collect();
    check_derived_metric_references(
        &derived,
        &def.variables,
        &all_metric_names,
        &all_metric_names_display,
    )
    .map_err(ParseError::positionless)?;

    // 4. Check for cycles in derived metric dependency graph
    let derived_name_strs: Vec<&str> = derived.iter().map(|m| m.name.as_str()).collect();
    check_derived_metric_cycles(&derived, &derived_name_strs).map_err(ParseError::positionless)
}

/// Check that no two metrics (base or derived) share the same name (case-insensitive).
fn check_metric_name_uniqueness(def: &SemanticViewDefinition) -> Result<(), String> {
    let mut seen_names: HashSet<String> = HashSet::new();
    for met in &def.metrics {
        let lower = met.name.to_ascii_lowercase();
        if !seen_names.insert(lower) {
            return Err(format!("duplicate metric name '{}'", met.name));
        }
    }
    Ok(())
}

/// Check that derived metrics do not contain aggregate function calls.
fn check_no_aggregates_in_derived(derived: &[&crate::model::Metric]) -> Result<(), String> {
    for met in derived {
        if let Some(func) = contains_aggregate_function(&met.expr) {
            return Err(format!(
                "derived metric '{}' must not contain aggregate function '{}'. \
                 Derived metrics compose other metrics; use a regular metric for aggregation.",
                met.name, func
            ));
        }
    }
    Ok(())
}

/// Check that all identifiers in derived metric expressions are known metric names.
///
/// Scans each derived expression with the shared reference tokenizer and treats
/// only **bare** reference chains as candidate metric names (a qualified chain
/// like `o.amount` is a raw column, not a metric). Matching goes through
/// [`crate::ident::normalize_ident_part`] on both sides, so a quoted or
/// mixed-case reference (`"Total Revenue"`, `REVENUE`) resolves exactly as the
/// expansion-time inliner does — closing the validator/inliner disagreement
/// that the former quote-blind byte scan had for quoted identifiers containing
/// spaces or dots (E-2 / E-3).
fn check_derived_metric_references(
    derived: &[&crate::model::Metric],
    variables: &[crate::model::Variable],
    all_metric_names: &[&str],
    all_metric_names_display: &[String],
) -> Result<(), String> {
    let mut known: HashSet<String> = all_metric_names
        .iter()
        .map(|n| crate::ident::normalize_ident_part(n))
        .collect();
    // A view variable is read by bare name too (Phase 6.5).
    known.extend(
        variables
            .iter()
            .map(|v| crate::ident::normalize_ident_part(&v.name)),
    );
    for met in derived {
        for r in crate::expr_tokens::scan_references(&met.expr) {
            if !r.is_bare() {
                continue; // qualified chain: a raw column, not a metric reference
            }
            let key = r.key();
            if known.contains(&key)
                || is_sql_keyword_or_builtin(&key)
                || key.chars().next().is_none_or(|c| c.is_ascii_digit())
            {
                continue;
            }
            let suggestion = suggest_closest(&key, all_metric_names_display);
            let mut msg = format!(
                "unknown metric '{}' referenced in derived metric '{}'",
                r.raw, met.name
            );
            if let Some(s) = suggestion {
                let _ = write!(msg, "; did you mean '{s}'?");
            }
            let _ = write!(
                msg,
                ". Available metrics: [{}]",
                all_metric_names.join(", ")
            );
            return Err(msg);
        }
    }
    Ok(())
}

/// Build derived-to-derived DAG and check for cycles via Kahn's algorithm.
fn check_derived_metric_cycles(
    derived: &[&crate::model::Metric],
    derived_name_strs: &[&str],
) -> Result<(), String> {
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    for &name in derived_name_strs {
        in_degree.entry(name).or_insert(0);
    }

    for met in derived {
        let refs = find_fact_references(&met.expr, derived_name_strs);
        for &referenced in &refs {
            edges.entry(met.name.as_str()).or_default().push(referenced);
            *in_degree.entry(referenced).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&str> = VecDeque::new();
    for (&name, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(name);
        }
    }

    let mut visited_count = 0;
    while let Some(node) = queue.pop_front() {
        visited_count += 1;
        if let Some(neighbors) = edges.get(node) {
            for &next in neighbors {
                if let Some(deg) = in_degree.get_mut(next) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(next);
                    }
                }
            }
        }
    }

    if visited_count != derived_name_strs.len() {
        let remaining: Vec<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg > 0)
            .map(|(&name, _)| name)
            .collect();

        if let Some(&start) = remaining.first() {
            let mut path = vec![start];
            let mut current = start;
            let mut seen: HashSet<&str> = HashSet::new();
            seen.insert(current);

            while let Some(neighbors) = edges.get(current) {
                if let Some(&next) = neighbors.iter().find(|n| remaining.contains(n)) {
                    if seen.contains(next) {
                        path.push(next);
                        return Err(format!(
                            "cycle detected in derived metrics: {}",
                            path.join(" -> ")
                        ));
                    }
                    seen.insert(next);
                    path.push(next);
                    current = next;
                } else {
                    break;
                }
            }
        }

        return Err("cycle detected in derived metrics".to_string());
    }

    Ok(())
}

/// Check if a token is a SQL keyword or common builtin (not a metric reference).
fn is_sql_keyword_or_builtin(token: &str) -> bool {
    const SQL_KEYWORDS: &[&str] = &[
        "and",
        "or",
        "not",
        "is",
        "null",
        "true",
        "false",
        "case",
        "when",
        "then",
        "else",
        "end",
        "in",
        "between",
        "like",
        "ilike",
        "as",
        "cast",
        "distinct",
        "asc",
        "desc",
        "by",
        "having",
        "where",
        "from",
        "select",
        "group",
        "order",
        "limit",
        "offset",
        "union",
        "intersect",
        "except",
        "all",
        "any",
        "exists",
        "over",
        "partition",
        "rows",
        "range",
        "unbounded",
        "preceding",
        "following",
        "current",
        "row",
        "filter",
        "within",
        "nulls",
        "first",
        "last",
        "if",
        "coalesce",
        "nullif",
        "ifnull",
    ];
    SQL_KEYWORDS.contains(&token)
}

#[cfg(test)]
mod tests {
    use crate::graph::{contains_aggregate_function, validate_derived_metrics};

    use super::super::test_helpers::*;

    // -----------------------------------------------------------------------
    // Phase 30: contains_aggregate_function tests
    // -----------------------------------------------------------------------

    #[test]
    fn contains_aggregate_sum() {
        let result = contains_aggregate_function("SUM(revenue)");
        assert_eq!(result, Some("sum"));
    }

    #[test]
    fn contains_aggregate_count_distinct() {
        let result = contains_aggregate_function("COUNT(DISTINCT order_id)");
        assert_eq!(result, Some("count"));
    }

    #[test]
    fn contains_aggregate_avg() {
        let result = contains_aggregate_function("AVG(price)");
        assert_eq!(result, Some("avg"));
    }

    #[test]
    fn contains_aggregate_none_arithmetic() {
        let result = contains_aggregate_function("revenue - cost");
        assert_eq!(result, None);
    }

    #[test]
    fn contains_aggregate_none_multiply() {
        let result = contains_aggregate_function("revenue * 100");
        assert_eq!(result, None);
    }

    #[test]
    fn contains_aggregate_none_summary_no_paren() {
        // "SUMMARY" should not match -- not followed by open-paren
        let result = contains_aggregate_function("SUMMARY");
        assert_eq!(result, None);
    }

    #[test]
    fn contains_aggregate_none_string_literal() {
        // Inside single-quoted string literal -- best effort skip
        let result = contains_aggregate_function("'SUM of values'");
        assert_eq!(result, None);
    }

    #[test]
    fn contains_aggregate_none_inside_escaped_string_literal() {
        // The tokenizer honours the `''` escape, so a `sum(` buried in a
        // single-quoted literal (even one containing an escaped quote) is not a
        // call — the former naive `'`-toggle scan could mis-detect it.
        assert_eq!(contains_aggregate_function("'it''s a sum(x)'"), None);
        // Dollar-quoted literal likewise.
        assert_eq!(contains_aggregate_function("$$ sum(x) $$"), None);
    }

    #[test]
    fn contains_aggregate_schema_qualified() {
        // A schema-qualified aggregate call matches on its last part.
        assert_eq!(contains_aggregate_function("main.sum(x)"), Some("sum"));
        // A non-aggregate qualified call does not.
        assert_eq!(contains_aggregate_function("main.scale(x)"), None);
    }

    #[test]
    fn contains_aggregate_quoted_dotted_function_name_is_not_split() {
        // A function whose name is a QUOTED identifier containing a dot is a
        // single part (`main.sum`), not the schema-qualified aggregate `sum` —
        // the called name is split quote-aware, so this is not a false positive.
        assert_eq!(contains_aggregate_function("\"main.sum\"(x)"), None);
        // A quoted bare aggregate name still matches (quotes don't hide it).
        assert_eq!(contains_aggregate_function("\"sum\"(x)"), Some("sum"));
    }

    #[test]
    fn contains_aggregate_none_after_unicode_prefix() {
        // E-5 (code-review 2026-07-11): the aggregate-name scan shares the
        // crate's single boundary definition, so `sum` abutting a non-ASCII
        // byte is part of one identifier (a function named `Ωsum`), not the
        // aggregate `sum`. The previous local boundary predicate treated
        // >= 0x80 bytes as boundaries and falsely reported an aggregate here.
        assert_eq!(contains_aggregate_function("Ωsum(x)"), None);
    }

    // -----------------------------------------------------------------------
    // Phase 30: validate_derived_metrics tests
    // -----------------------------------------------------------------------

    #[test]
    fn validate_derived_metrics_cycle_detected() {
        // a -> b -> a (cycle)
        let def = make_def_with_derived_metrics(vec![], vec![("a", "b + 1"), ("b", "a + 1")]);
        let err = validate_derived_metrics(&def).unwrap_err().message;
        assert!(
            err.contains("cycle detected in derived metrics"),
            "Expected cycle error, got: {err}"
        );
    }

    #[test]
    fn validate_derived_metrics_unknown_reference() {
        // "profit AS revenue - nonexistent" where "nonexistent" is not a metric name
        let def = make_def_with_derived_metrics(
            vec![("revenue", "SUM(o.amount)", "o")],
            vec![("profit", "revenue - nonexistent")],
        );
        let err = validate_derived_metrics(&def).unwrap_err().message;
        assert!(
            err.contains("unknown metric") && err.contains("nonexistent"),
            "Expected unknown metric error, got: {err}"
        );
    }

    #[test]
    fn validate_derived_metrics_aggregate_rejected() {
        // Derived metric must not contain aggregate function
        let def = make_def_with_derived_metrics(
            vec![("revenue", "SUM(o.amount)", "o")],
            vec![("bad", "SUM(revenue)")],
        );
        let err = validate_derived_metrics(&def).unwrap_err().message;
        assert!(
            err.contains("aggregate function") && err.contains("bad"),
            "Expected aggregate error, got: {err}"
        );
    }

    #[test]
    fn validate_derived_metrics_valid_simple() {
        // "profit AS revenue - cost" where both are base metrics
        let def = make_def_with_derived_metrics(
            vec![
                ("revenue", "SUM(o.amount)", "o"),
                ("cost", "SUM(o.cost)", "o"),
            ],
            vec![("profit", "revenue - cost")],
        );
        assert!(
            validate_derived_metrics(&def).is_ok(),
            "Valid derived metric should be accepted"
        );
    }

    #[test]
    fn validate_derived_metrics_valid_stacking() {
        // "margin AS profit / revenue" where profit is derived and revenue is base
        let def = make_def_with_derived_metrics(
            vec![
                ("revenue", "SUM(o.amount)", "o"),
                ("cost", "SUM(o.cost)", "o"),
            ],
            vec![("profit", "revenue - cost"), ("margin", "profit / revenue")],
        );
        assert!(
            validate_derived_metrics(&def).is_ok(),
            "Stacking derived metrics should be accepted"
        );
    }

    #[test]
    fn validate_derived_metrics_valid_negation() {
        // "neg_revenue AS -revenue"
        let def = make_def_with_derived_metrics(
            vec![("revenue", "SUM(o.amount)", "o")],
            vec![("neg_revenue", "-revenue")],
        );
        assert!(
            validate_derived_metrics(&def).is_ok(),
            "Single-metric reference should be accepted"
        );
    }

    #[test]
    fn validate_derived_metrics_did_you_mean() {
        // Close misspelling: "revnue" instead of "revenue"
        let def = make_def_with_derived_metrics(
            vec![("revenue", "SUM(o.amount)", "o")],
            vec![("profit", "revnue - cost")],
        );
        let err = validate_derived_metrics(&def).unwrap_err().message;
        assert!(
            err.contains("did you mean"),
            "Expected 'did you mean?' suggestion, got: {err}"
        );
    }

    #[test]
    fn validate_derived_metrics_no_derived_returns_ok() {
        // No derived metrics -> should return Ok immediately
        let def = make_def_with_derived_metrics(vec![("revenue", "SUM(o.amount)", "o")], vec![]);
        assert!(
            validate_derived_metrics(&def).is_ok(),
            "No derived metrics should return Ok"
        );
    }

    #[test]
    fn validate_derived_metrics_quoted_spaced_reference_resolves() {
        // A base metric whose name contains a space is referenced by a quoted
        // identifier in a derived expression. The tokenizer treats `"Total
        // Revenue"` as ONE reference (key `total revenue`) matching the base
        // metric — the former quote-blind scan split it into `Total` + `Revenue`
        // and falsely rejected this valid DDL (E-2 / E-3).
        let def = make_def_with_derived_metrics(
            vec![("total revenue", "SUM(o.amount)", "o")],
            vec![("double", "\"Total Revenue\" * 2")],
        );
        assert!(
            validate_derived_metrics(&def).is_ok(),
            "quoted spaced metric reference should resolve: {:?}",
            validate_derived_metrics(&def)
        );
    }

    #[test]
    fn validate_derived_metrics_quoted_unknown_reference_reports_raw() {
        // An unknown quoted reference is reported with the raw text the user
        // wrote (quotes and all), not a mangled split.
        let def = make_def_with_derived_metrics(
            vec![("revenue", "SUM(o.amount)", "o")],
            vec![("bad", "\"No Such Metric\" + 1")],
        );
        let err = validate_derived_metrics(&def).unwrap_err().message;
        assert!(
            err.contains("unknown metric") && err.contains("\"No Such Metric\""),
            "Expected raw quoted name in error, got: {err}"
        );
    }

    #[test]
    fn validate_derived_metrics_qualified_column_reference_is_not_a_metric() {
        // A qualified chain (`o.amount`) in a derived expression is a raw column,
        // not a metric reference — it must not be flagged as an unknown metric.
        let def = make_def_with_derived_metrics(
            vec![("revenue", "SUM(o.amount)", "o")],
            vec![("scaled", "revenue + o.amount")],
        );
        assert!(
            validate_derived_metrics(&def).is_ok(),
            "qualified column in derived expr should be ignored: {:?}",
            validate_derived_metrics(&def)
        );
    }

    #[test]
    fn validate_derived_metrics_duplicate_name_rejected() {
        // Same name used by both base and derived metric
        let def = make_def_with_derived_metrics(
            vec![("revenue", "SUM(o.amount)", "o")],
            vec![("revenue", "cost + 1")],
        );
        let err = validate_derived_metrics(&def).unwrap_err().message;
        assert!(
            err.contains("duplicate metric name"),
            "Expected duplicate name error, got: {err}"
        );
    }
}
