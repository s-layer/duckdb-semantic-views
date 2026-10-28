use std::collections::{HashMap, HashSet, VecDeque};

use crate::expr_tokens::{inline_references, references_ref};
use crate::ident::normalize_ident_part;
use crate::model::{Fact, Join, TableRef};
use crate::util::is_word_boundary_char;

use super::resolution::quote_stored_ident;

/// Maximum allowed nesting depth for derived metric resolution.
/// Prevents stack overflow from deeply nested metric chains that pass
/// cycle detection (linear chains: a->b->c->d->... up to 64 levels).
const MAX_DERIVATION_DEPTH: usize = 64;

/// Collect `using_relationships` from all transitive base metrics referenced by a derived metric.
pub(super) fn collect_derived_metric_using(
    met: &crate::model::Metric,
    all_metrics: &[crate::model::Metric],
) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = vec![met.name.to_ascii_lowercase()];

    let name_map: HashMap<String, &crate::model::Metric> = all_metrics
        .iter()
        .map(|m| (m.name.to_ascii_lowercase(), m))
        .collect();

    let all_names: Vec<String> = all_metrics
        .iter()
        .map(|m| m.name.to_ascii_lowercase())
        .collect();

    while let Some(current_name) = stack.pop() {
        if !visited.insert(current_name.clone()) {
            continue;
        }
        let Some(current_met) = name_map.get(&current_name) else {
            continue;
        };

        if current_met.source_table.is_some() {
            // Base metric: collect its USING relationships
            for rel in &current_met.using_relationships {
                if !result.contains(rel) {
                    result.push(rel.clone());
                }
            }
        } else {
            // Derived metric: find referenced metric names and push to stack.
            // A base metric may be referenced bare or by its own source table.
            for name in &all_names {
                let src = name_map.get(name).and_then(|m| m.source_table.as_deref());
                if *name != current_name && references_ref(&current_met.expr, name, src) {
                    stack.push(name.clone());
                }
            }
        }
    }

    result
}

/// Topologically sort facts by their inter-dependencies (leaf facts first).
///
/// Uses Kahn's algorithm. Returns indices into the `facts` slice in topological
/// order (facts with no dependencies on other facts come first).
///
/// Returns `Err` if a cycle is detected (defensive -- `validate_facts` should
/// have already rejected cycles at CREATE time).
pub(super) fn toposort_facts(facts: &[Fact]) -> Result<Vec<usize>, String> {
    let n = facts.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    // Build name -> index map (case-insensitive)
    let name_to_idx: HashMap<String, usize> = facts
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.to_ascii_lowercase(), i))
        .collect();

    // Build adjacency: edges[i] = set of indices that fact i depends on
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n]; // dependents[dep] = facts that depend on dep

    for (i, fact) in facts.iter().enumerate() {
        for (name, &dep_idx) in &name_to_idx {
            if dep_idx == i {
                continue; // skip self
            }
            // Does this fact's expr reference the other fact — bare, or
            // qualified by that fact's own source table (`o.b`)? A foreign
            // qualifier is a column on another relation, not a fact ref (E-3).
            if references_ref(&fact.expr, name, facts[dep_idx].source_table.as_deref()) {
                in_degree[i] += 1;
                dependents[dep_idx].push(i);
            }
        }
    }

    // Kahn's algorithm
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(i);
        }
    }

    let mut order = Vec::with_capacity(n);
    while let Some(idx) = queue.pop_front() {
        order.push(idx);
        for &dep in &dependents[idx] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                queue.push_back(dep);
            }
        }
    }

    if order.len() != n {
        return Err("cycle detected in facts".to_string());
    }

    Ok(order)
}

/// The `source_table` aliases of every fact `expr` references, transitively.
///
/// PAR-6 (code-review 2026-08-03, TECH-DEBT #53). [`inline_facts`] splices a
/// referenced fact's expression into the host expression wherever it appears,
/// including a fact declared on *another* logical table —
/// [`fact_replacement_map`] keys each fact by its own `source_table.name`
/// precisely so that cross-table reference resolves. Nothing told
/// `join_resolver` about it, so the spliced expression named an alias that was
/// never joined and the emitted SQL could not bind. This is the missing half:
/// the tables a member's expression reaches *through* its fact references,
/// which the caller adds to the set it joins.
///
/// The walk is transitive because facts chain — a fact on `o` may reference one
/// on `c`, and both tables have to be in scope. `seen` guards against
/// re-visiting a fact (and so against a cycle, which `validate_facts` rejects
/// at CREATE but which must not hang the expander regardless). Facts with no
/// `source_table` contribute no table: they resolve against the host
/// expression's own scope.
///
/// Returns `(fact name, lowercased alias)` pairs in first-reached order, one
/// per reached fact that declares a table. The fact name is carried because the
/// fan-trap fence names it when the reached table cannot be joined safely.
pub(super) fn collect_referenced_facts(expr: &str, facts: &[Fact]) -> Vec<(String, String)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut reached: Vec<(String, String)> = Vec::new();
    let mut pending: Vec<String> = vec![expr.to_string()];

    while let Some(current) = pending.pop() {
        for fact in facts {
            let key = fact.name.to_ascii_lowercase();
            if seen.contains(&key) {
                continue;
            }
            if !references_ref(&current, &fact.name, fact.source_table.as_deref()) {
                continue;
            }
            seen.insert(key);
            if let Some(ref src) = fact.source_table {
                let alias = src.to_ascii_lowercase();
                if !reached.iter().any(|(_, a)| *a == alias) {
                    reached.push((fact.name.clone(), alias));
                }
            }
            pending.push(fact.expr.clone());
        }
    }

    reached
}

/// A dimension's expression with the facts it references inlined.
///
/// TECH-DEBT #54. `inline_facts` was applied to metric expressions and to other
/// facts' expressions, never to a dimension's — so a dimension declared
/// `band AS CASE WHEN o.net_line > 100 …`, over a fact `net_line` on its own
/// table, emitted `o.net_line` verbatim and `DuckDB` failed on the unknown
/// column. Snowflake's validation rules permit exactly this ("expressions can
/// refer to base table columns **or other expressions** on the same logical
/// table"), so the gap was a parity defect rather than an unsupported
/// extension, and it applied to the plain same-table case — not just to the
/// cross-table form PAR-6 dealt with for metrics.
///
/// Every emitter that renders a dimension expression goes through here, so the
/// inlined form reaches the base-anchored SELECT, the grain CTEs, the facts
/// path, the semi-additive snapshot and its `ORDER BY`, the window CTE, and
/// `where_clause` members alike. Role-playing alias rewriting is applied by the
/// callers *after* this, which is the correct order: the fact's expression is
/// spliced in first, then the qualifier it carries is rewritten along with the
/// rest.
///
/// A cyclic fact set (rejected at CREATE by `validate_facts`) leaves the
/// expression untouched rather than panicking.
pub(super) fn inline_dimension_facts(expr: &str, facts: &[Fact]) -> String {
    if facts.is_empty() {
        return expr.to_string();
    }
    let Ok(topo) = toposort_facts(facts) else {
        return expr.to_string();
    };
    inline_facts(expr, facts, &topo)
}

/// The reached tables alone — [`collect_referenced_facts`] without the names,
/// for the join resolver, which only needs to know what to join.
pub(super) fn collect_referenced_fact_tables(expr: &str, facts: &[Fact]) -> Vec<String> {
    collect_referenced_facts(expr, facts)
        .into_iter()
        .map(|(_, alias)| alias)
        .collect()
}

/// Inline fact expressions into a metric expression.
///
/// Processes facts in topological order (leaf facts first), resolving each fact's
/// expression by inlining any previously-resolved facts. Then applies all resolved
/// facts to the input `expr`.
///
/// Each inlined fact expression is parenthesized to preserve operator precedence:
/// `net_price = price * (1 - discount)` inlined into `SUM(net_price)` becomes
/// `SUM((price * (1 - discount)))`.
pub(super) fn inline_facts(expr: &str, facts: &[Fact], topo_order: &[usize]) -> String {
    if facts.is_empty() || topo_order.is_empty() {
        return expr.to_string();
    }

    // Build resolved expressions in topological order.
    let mut resolved: HashMap<String, String> = HashMap::new();

    for &idx in topo_order {
        let fact = &facts[idx];
        // Inline any already-resolved facts into this fact's expression, keyed
        // by their normalized (quote-/case-insensitive) form. A fact's own
        // qualified form `alias.name` and its bare `name` are distinct keys, so
        // an identity fact's `alias.name` is matched as a whole while a foreign
        // `x.name` is left intact (E-3) — the shared tokenizer handles both by
        // chain key, and never rescans inserted text.
        let resolved_expr = if resolved.is_empty() {
            fact.expr.clone()
        } else {
            let map = fact_replacement_map(facts, &resolved);
            inline_references(&fact.expr, &map)
        };
        resolved.insert(fact.name.clone(), format!("({resolved_expr})"));
    }

    // Apply all resolved facts to the input expression in one pass.
    let map = fact_replacement_map(facts, &resolved);
    inline_references(expr, &map)
}

/// Build the `{normalized key -> replacement}` map for inlining resolved facts.
///
/// Each resolved fact is keyed by its **own** bare name and — when it has one —
/// its own `source_table.name` qualified form. Keying by the fact's own source
/// table (not the host expression's) keeps this consistent with dependency
/// detection (`toposort_facts` / `build_fact_dag`), which recognise a reference
/// to fact `f` written as `f.source_table.f.name`; a fact referenced across
/// tables in its own-qualified form is then actually inlined, not just
/// detected. Only facts present in `resolved` contribute, so during
/// topological resolution the map naturally holds just the already-resolved
/// (earlier) facts.
fn fact_replacement_map<'a>(
    facts: &[Fact],
    resolved: &'a HashMap<String, String>,
) -> HashMap<String, &'a str> {
    let mut map: HashMap<String, &str> = HashMap::with_capacity(resolved.len() * 2);
    for fact in facts {
        if let Some(replacement) = resolved.get(&fact.name) {
            insert_fact_keys(
                &mut map,
                fact.source_table.as_deref(),
                &fact.name,
                replacement,
            );
        }
    }
    map
}

/// The bare and (optionally) `source_table.name` normalized keys for one metric
/// name — the metric-side twin of [`insert_fact_keys`].
///
/// EXP-24: the derived-metric replacement map was keyed by the BARE name only,
/// while every detection site (`references_ref`) also matches the qualified
/// spelling and `graph/member_refs.rs` blesses `t1.metric_a + t2.metric_b` as a
/// legal cross-table form. So a qualified reference contributed its table to
/// grain/join resolution and was then left in the SQL verbatim, as a raw
/// unaggregated column.
fn metric_keys(source_table: Option<&str>, name: &str) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    if let Some(st) = source_table {
        keys.push(normalize_ident_part(&format!("{st}.{name}")));
    }
    keys.push(normalize_ident_part(name));
    keys
}

/// Insert the bare and (optionally) `source_table.name` normalized keys for one
/// fact `name` into `map`, both pointing at `replacement`.
fn insert_fact_keys<'a>(
    map: &mut HashMap<String, &'a str>,
    source_table: Option<&str>,
    name: &str,
    replacement: &'a str,
) {
    if let Some(st) = source_table {
        map.insert(normalize_ident_part(&format!("{st}.{name}")), replacement);
    }
    map.insert(normalize_ident_part(name), replacement);
}

/// Replace every `COUNT(*)` call in `expr` with `COUNT(<replacement_arg>)`.
///
/// Matches `count` case-insensitively at a word boundary, followed by
/// optional whitespace, `(`, optional whitespace, `*`, optional whitespace,
/// `)` — i.e. `COUNT(*)`, `count( * )`, etc. The original casing of the
/// function name is preserved; only the argument is replaced.
///
/// Occurrences inside a quoted region are left untouched: single-quoted string
/// literals, double-quoted identifiers and `$tag$ … $tag$` dollar-quoted
/// strings alike, per [`crate::util::QuoteState`]. Before EXP-16 only single
/// quotes were tracked, so `"my count(*) col"` and `$$count(*)$$` were rewritten
/// as though they were live calls.
///
/// Returns `None` when the expression contains no `COUNT(*)` call.
pub(super) fn rewrite_count_star(expr: &str, replacement_arg: &str) -> Option<String> {
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len() + replacement_arg.len());
    let mut copied = 0usize; // byte offset copied into `out` so far
    let mut pos = 0usize;
    // EXP-16: single-quote tracking alone rewrote `count(*)` occurring inside a
    // double-quoted identifier or a `$tag$` literal, corrupting it. `QuoteState`
    // is the crate's one scanner and treats all three regions as inert.
    let mut quotes = crate::util::QuoteState::default();
    let mut changed = false;
    while pos < bytes.len() {
        let (next, is_live_code) = quotes.step(bytes, pos);
        if !is_live_code {
            pos = next;
            continue;
        }
        let byte = bytes[pos];
        if (byte == b'c' || byte == b'C')
            && pos + 5 <= bytes.len()
            && bytes[pos..pos + 5].eq_ignore_ascii_case(b"count")
            && (pos == 0 || is_word_boundary_char(bytes[pos - 1]))
        {
            let mut open_paren = pos + 5;
            while open_paren < bytes.len() && bytes[open_paren].is_ascii_whitespace() {
                open_paren += 1;
            }
            if open_paren < bytes.len() && bytes[open_paren] == b'(' {
                let mut star = open_paren + 1;
                while star < bytes.len() && bytes[star].is_ascii_whitespace() {
                    star += 1;
                }
                if star < bytes.len() && bytes[star] == b'*' {
                    let mut close_paren = star + 1;
                    while close_paren < bytes.len() && bytes[close_paren].is_ascii_whitespace() {
                        close_paren += 1;
                    }
                    if close_paren < bytes.len() && bytes[close_paren] == b')' {
                        // All scanned offsets sit on ASCII bytes, so slicing
                        // is char-boundary safe. Copy through the `(`, swap
                        // the `*` (and its padding) for the replacement.
                        out.push_str(&expr[copied..=open_paren]);
                        out.push_str(replacement_arg);
                        out.push(')');
                        copied = close_paren + 1;
                        pos = close_paren + 1;
                        changed = true;
                        continue;
                    }
                }
            }
        }
        pos += 1;
    }
    if !changed {
        return None;
    }
    out.push_str(&expr[copied..]);
    Some(out)
}

/// Aggregates on the shared list ([`crate::graph::aggregate_name`]) that
/// [`guard_aggregate_args`] leaves alone, because their first argument is not
/// the value being aggregated: `count_star` takes no argument, and
/// `percentile_cont` / `percentile_disc` take the fraction, with the value in
/// `WITHIN GROUP (ORDER BY ...)`.
const UNGUARDED_AGGREGATES: &[&str] = &["count_star", "percentile_cont", "percentile_disc"];

/// Whether `word` names an aggregate whose first argument
/// [`guard_aggregate_args`] guards, case-insensitively.
///
/// The names come from the list CREATE refuses in a dimension or fact, which
/// `every_duckdb_aggregate_is_known` keeps complete against `DuckDB`'s
/// `duckdb_functions()`, so the guard cannot fall behind it.
///
/// Every such aggregate is guarded: on a NULL-extended (LEFT-JOINed) source
/// table the phantom row must not reach ANY aggregate, whatever its argument.
/// EXP-21 fenced only a five-name whitelist over recognized constant literals;
/// EXP-25/EXP-26 (code-review 2026-08-08) showed that whitelist leaking four
/// ways — `COUNT(DISTINCT 1)`, `COUNT(1+0)`, `MIN(1)`, and any NULL-insensitive
/// expression such as `SUM(COALESCE(li.qty, 99))` or a fact reference reaching
/// another table.
///
/// Matching is over the whole identifier word, so `sum_no_overflow(` matches as
/// itself rather than as `sum` (a prefix-scan would have missed it entirely).
fn is_aggregate_name(word: &str) -> bool {
    let key = word.to_ascii_lowercase();
    crate::graph::aggregate_name(&key).is_some() && !UNGUARDED_AGGREGATES.contains(&key.as_str())
}

/// Whether `arg` is a SQL constant: a numeric literal, a single-quoted string
/// literal, or `TRUE`/`FALSE`/`NULL` — each optionally wrapped in redundant
/// parentheses.
///
/// Deliberately conservative — a false negative leaves an expression alone
/// (the status quo), while a false positive would wrap something row-dependent
/// in a `CASE` and change what it means.
fn is_constant_literal(arg: &str) -> bool {
    let mut t = arg.trim();
    // Peel redundant outer parentheses: `(1)` is the same constant as `1`, and
    // left unpeeled it fails every check below on the leading `(`.
    //
    // Only when the opening paren's match IS the final character. `(1)+(2)` also
    // starts with `(` and ends with `)` without those two being a pair, and
    // peeling them blindly would hand the checks below the garbage `1)+(2` —
    // harmless for a numeric literal, but `('a')||('b')` would peel to
    // `'a')||('b'` and pass the starts-and-ends-with-quote test by accident.
    while t.starts_with('(') {
        match super::semi_additive::find_matching_paren(t, 0) {
            Some(close) if close == t.len() - 1 => t = t[1..close].trim(),
            _ => break,
        }
    }
    if t.is_empty() {
        return false;
    }
    if t.eq_ignore_ascii_case("TRUE")
        || t.eq_ignore_ascii_case("FALSE")
        || t.eq_ignore_ascii_case("NULL")
    {
        return true;
    }
    // A single-quoted string literal, whole and entire: an argument that merely
    // starts and ends with a quote but re-enters live code in between (`'a' ||
    // o.col` cannot, but `'a' || 'b'` can) is still constant, so accepting it
    // is safe; anything ending outside a literal is rejected.
    if t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2 {
        return true;
    }
    // A numeric literal: digits with at most one decimal point and an optional
    // exponent. Hand-rolled rather than `parse::<f64>()` so SQL spellings that
    // Rust rejects (a trailing `.`) and Rust spellings SQL rejects (`inf`,
    // `1_000`) both classify the SQL way.
    let bytes = t.as_bytes();
    let mut i = 0;
    if bytes[i] == b'+' || bytes[i] == b'-' {
        i += 1;
    }
    let mut seen_digit = false;
    let mut seen_dot = false;
    while i < bytes.len() {
        match bytes[i] {
            b'0'..=b'9' => seen_digit = true,
            b'.' if !seen_dot => seen_dot = true,
            b'e' | b'E' if seen_digit => {
                // Exponent: optional sign then at least one digit, then end.
                i += 1;
                if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                    i += 1;
                }
                if i >= bytes.len() {
                    return false;
                }
                return bytes[i..].iter().all(u8::is_ascii_digit);
            }
            _ => return false,
        }
        i += 1;
    }
    seen_digit
}

/// The span of an aggregate call's FIRST argument inside `inner` (the text
/// between the call's parentheses), as `(quantifier_end, arg_end)` byte
/// offsets.
///
/// `inner[..quantifier_end]` is a leading `DISTINCT` / `ALL` set quantifier
/// (empty when absent) and `inner[quantifier_end..arg_end]` is the first
/// argument. `inner[arg_end..]` is everything the guard must leave verbatim:
/// further arguments (`STRING_AGG(x, ',')` — the separator is a parameter of
/// the CALL, not a value being aggregated) and a trailing in-call `ORDER BY`
/// (`ARRAY_AGG(x ORDER BY y)` — wrapping that inside the `CASE` would be a
/// syntax error).
///
/// Returns `None` when there is no argument to guard: an empty argument list,
/// or the `*` of `COUNT(*)` (SG-8's [`rewrite_count_star`] owns that spelling —
/// guarding it too would emit `COUNT(CASE WHEN pk IS NOT NULL THEN pk END)`).
fn first_aggregate_arg_span(inner: &str) -> Option<(usize, usize)> {
    let bytes = inner.as_bytes();
    let quantifier_end = leading_quantifier_end(inner);
    // Scan from 0 (not `quantifier_end`) so `QuoteState` sees the whole string;
    // a quantifier keyword contains no quote bytes, so nothing before
    // `quantifier_end` can match the terminators below.
    let mut quotes = crate::util::QuoteState::default();
    let mut depth = 0i32;
    let mut arg_end = bytes.len();
    let mut i = 0;
    while i < bytes.len() {
        let (next, is_live_code) = quotes.step(bytes, i);
        if is_live_code {
            if depth == 0 && i >= quantifier_end && (bytes[i] == b',' || is_order_by_at(bytes, i)) {
                arg_end = i;
                break;
            }
            match bytes[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
        }
        i = next;
    }
    let arg = inner.get(quantifier_end..arg_end)?.trim();
    if arg.is_empty() || arg == "*" {
        return None;
    }
    Some((quantifier_end, arg_end))
}

/// Byte offset just past a leading `DISTINCT` / `ALL` set quantifier in an
/// aggregate's argument list, or 0 when there is none.
fn leading_quantifier_end(inner: &str) -> usize {
    let lead = inner.len() - inner.trim_start().len();
    let rest = &inner.as_bytes()[lead..];
    for kw in [b"DISTINCT".as_slice(), b"ALL".as_slice()] {
        if rest.len() >= kw.len()
            && rest[..kw.len()].eq_ignore_ascii_case(kw)
            && rest.get(kw.len()).is_none_or(|b| is_word_boundary_char(*b))
        {
            return lead + kw.len();
        }
    }
    0
}

/// Whether an in-call `ORDER BY` clause starts at byte `i` — the `ORDER` of
/// `ARRAY_AGG(x ORDER BY y)`, at a word boundary and followed by `BY`.
fn is_order_by_at(bytes: &[u8], i: usize) -> bool {
    if !(i == 0 || is_word_boundary_char(bytes[i - 1])) {
        return false;
    }
    if i + 5 > bytes.len() || !bytes[i..i + 5].eq_ignore_ascii_case(b"ORDER") {
        return false;
    }
    let mut j = i + 5;
    let ws_start = j;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if j == ws_start {
        return false; // `ORDERED`, `ORDER_ID`, … — not the keyword
    }
    j + 2 <= bytes.len()
        && bytes[j..j + 2].eq_ignore_ascii_case(b"BY")
        && bytes.get(j + 2).is_none_or(|b| is_word_boundary_char(*b))
}

/// Whether `expr` contains an aggregate call over a recognized CONSTANT
/// literal — the subset of the phantom-row hazard that is provably wrong when
/// [`guard_aggregate_args`] cannot run at all because the source table declares
/// no PRIMARY KEY.
///
/// Used only to decide whether to raise the no-PK error (see
/// [`inline_derived_metrics`]); the guard itself is not selective.
fn has_constant_arg_aggregate(expr: &str) -> bool {
    scan_aggregate_calls(expr, |inner, (quantifier_end, arg_end)| {
        is_constant_literal(&inner[quantifier_end..arg_end])
    })
}

/// Guard the first argument of EVERY aggregate call in `expr` with `pk_ref`, so
/// a NULL-extended LEFT JOIN row contributes nothing (EXP-21, EXP-25, EXP-26).
///
/// `COUNT(1)` becomes `COUNT(CASE WHEN <pk> IS NOT NULL THEN 1 END)` and
/// `SUM(COALESCE(li.qty, 99))` becomes
/// `SUM(CASE WHEN <pk> IS NOT NULL THEN COALESCE(li.qty, 99) END)`: the
/// argument evaluates to NULL exactly on the phantom row, and a NULL first
/// argument drops the row from every aggregate [`is_aggregate_name`] names. On a
/// real row the PK is non-NULL by definition, so the `CASE` returns the
/// original value — the rewrite is semantically neutral on real data whatever
/// the argument is, which is why it no longer needs to RECOGNIZE the argument
/// (the EXP-21 whitelist did, and leaked four ways: EXP-25/EXP-26). It also
/// restores the empty-group semantics a childless parent should have had —
/// `NULL` for `SUM(1)` / `MIN(1)`, `0` for `COUNT(1)`.
///
/// Only the FIRST argument is guarded: a later argument is a parameter of the
/// call rather than a value being aggregated (`STRING_AGG`'s separator,
/// `QUANTILE`'s fraction), and every multi-argument aggregate drops a row whose
/// first argument is NULL anyway. A trailing in-call `ORDER BY` and an outer
/// `FILTER (WHERE …)` clause (which sits outside the parentheses) are likewise
/// left verbatim.
///
/// Returns `None` when the expression contains no guardable aggregate call,
/// matching [`rewrite_count_star`]'s contract. Quoted regions are inert
/// throughout ([`crate::util::QuoteState`]), so `'count(1)'` and
/// `"sum(1) col"` are left alone.
///
/// Two residuals are recorded in TECH-DEBT #67: the NULL-RETAINING aggregates
/// (`list` / `array_agg` / `first` / `last` / `arbitrary`, which keep a NULL
/// element rather than skipping it, so the phantom leaves a NULL in the list
/// instead of a value), and the case where the source table declares no
/// PRIMARY KEY, where there is no column to guard with at all.
pub(super) fn guard_aggregate_args(expr: &str, pk_ref: &str) -> Option<String> {
    let mut out = String::with_capacity(expr.len() + pk_ref.len() + 32);
    let mut copied = 0usize;
    let mut changed = false;
    scan_aggregate_calls(expr, |inner, (quantifier_end, arg_end)| {
        // `inner` is a subslice of `expr`, so its start converts back by
        // pointer offset — `scan_aggregate_calls` cuts it from `expr` itself.
        let inner_start = inner.as_ptr() as usize - expr.as_ptr() as usize;
        out.push_str(&expr[copied..inner_start]);
        let quantifier = inner[..quantifier_end].trim();
        if !quantifier.is_empty() {
            out.push_str(quantifier);
            out.push(' ');
        }
        out.push_str("CASE WHEN ");
        out.push_str(pk_ref);
        out.push_str(" IS NOT NULL THEN ");
        out.push_str(inner[quantifier_end..arg_end].trim());
        out.push_str(" END");
        copied = inner_start + arg_end;
        changed = true;
        false // never stop early: every call has to be guarded
    });
    if !changed {
        return None;
    }
    out.push_str(&expr[copied..]);
    Some(out)
}

/// Walk every aggregate call in `expr`, in source order, invoking `visit` with
/// the text between its parentheses and the [`first_aggregate_arg_span`] of
/// that text. Stops early (returning `true`) as soon as `visit` returns `true`.
///
/// Shared by [`guard_aggregate_args`] (which rewrites, ignoring the answer) and
/// [`has_constant_arg_aggregate`] (which only asks a question), so the two
/// cannot disagree about what counts as an aggregate call.
///
/// A function name is matched as a WHOLE identifier word at a word boundary,
/// never as a prefix: `sum_no_overflow(` reads as itself rather than as `sum`
/// followed by junk (which the prefix scan it replaces skipped entirely), and
/// `miscount(*)` matches nothing.
fn scan_aggregate_calls(expr: &str, mut visit: impl FnMut(&str, (usize, usize)) -> bool) -> bool {
    let bytes = expr.as_bytes();
    let mut quotes = crate::util::QuoteState::default();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let (next, is_live_code) = quotes.step(bytes, pos);
        if !is_live_code || !crate::util::is_ident_byte(bytes[pos]) {
            pos = next;
            continue;
        }
        // An identifier run: every byte in it is ordinary live code, so jumping
        // over the run leaves `quotes` in the same (idle) state.
        let word_start = pos;
        let mut word_end = pos;
        while word_end < bytes.len() && crate::util::is_ident_continue(bytes[word_end]) {
            word_end += 1;
        }
        pos = word_end;
        if word_start != 0 && !is_word_boundary_char(bytes[word_start - 1]) {
            continue;
        }
        if !is_aggregate_name(&expr[word_start..word_end]) {
            continue;
        }
        let mut open = word_end;
        while open < bytes.len() && bytes[open].is_ascii_whitespace() {
            open += 1;
        }
        if open >= bytes.len() || bytes[open] != b'(' {
            continue;
        }
        let Some(close) = super::semi_additive::find_matching_paren(expr, open) else {
            continue;
        };
        // Every scanned offset sits on an ASCII byte, so slicing is
        // char-boundary safe.
        let inner = &expr[open + 1..close];
        if let Some(span) = first_aggregate_arg_span(inner) {
            if visit(inner, span) {
                return true;
            }
        }
        // The parenthesized region is balanced and every quote inside it is
        // closed, so resuming after `)` keeps `quotes` idle.
        pos = close + 1;
    }
    false
}

/// Resolved metric expressions plus SG-8 rewrite failures.
///
/// Produced by [`inline_derived_metrics`]. Both maps are keyed by the metric's
/// canonical identifier key ([`crate::ident::normalize_ident_part`] — quotes
/// stripped and case-folded), the same key every consumer resolves through
/// (the window path's inner-metric lookup, the semi-additive path, and the
/// top-level SELECT). Keying on the raw lowercased name instead left a quoted
/// stored name (`"Item_Count"`) unreachable from a quote-stripped lookup, so a
/// window metric over it silently lost fact inlining and the SG-8
/// `COUNT(*)`->`COUNT(pk)` rewrite (EXP-6, code-review 2026-07-18).
///
/// `count_star_no_pk` holds the lowercased source-table alias of each base
/// metric whose `COUNT(*)` could NOT be rewritten (non-base source table with
/// no PRIMARY KEY declared). Erroring is the caller's job so that only queries
/// which actually use such a metric fail — unrelated metrics on the same view
/// keep working.
#[derive(Debug)]
pub(super) struct ResolvedMetricExprs {
    /// Canonical metric key -> fully-resolved expression.
    pub exprs: HashMap<String, String>,
    /// Canonical metric key -> lowercased source-table alias for metrics
    /// with an unrewritable `COUNT(*)` (SG-8).
    pub count_star_no_pk: HashMap<String, String>,
}

/// Topologically sort derived metrics by their inter-dependencies.
///
/// Uses Kahn's algorithm. Only derived-to-derived edges are considered;
/// references to base metrics are external and do not contribute to in-degree.
/// Returns indices into the `derived` slice in resolution order.
fn toposort_derived(
    derived: &[(usize, &crate::model::Metric)],
    _resolved_names: &HashMap<String, String>,
) -> Result<Vec<usize>, String> {
    let n = derived.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    // Build name -> index-in-derived-slice map (canonical identifier keys, so a
    // quoted stored name resolves consistently with the rest of the pipeline).
    let name_to_idx: HashMap<String, usize> = derived
        .iter()
        .enumerate()
        .map(|(i, (_, m))| (normalize_ident_part(&m.name), i))
        .collect();

    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, (_, met)) in derived.iter().enumerate() {
        for (name, &dep_idx) in &name_to_idx {
            if dep_idx == i {
                continue; // skip self
            }
            // Only derived-to-derived edges: does this expr reference `name`?
            // Derived metrics have no source table, so a bare reference only.
            if references_ref(&met.expr, name, None) {
                in_degree[i] += 1;
                dependents[dep_idx].push(i);
            }
        }
    }

    // Kahn's algorithm
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(i);
        }
    }

    let mut order = Vec::with_capacity(n);
    while let Some(idx) = queue.pop_front() {
        order.push(idx);
        for &dep in &dependents[idx] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                queue.push_back(dep);
            }
        }
    }

    if order.len() != n {
        // Build cycle description from remaining nodes
        let remaining: Vec<&str> = derived
            .iter()
            .enumerate()
            .filter(|(i, _)| !order.contains(i))
            .map(|(_, (_, m))| m.name.as_str())
            .collect();
        return Err(format!(
            "cycle in derived metrics: [{}]",
            remaining.join(", ")
        ));
    }
    Ok(order)
}

/// [`inline_derived_metrics_with_joins`] with no relationships — the test
/// fixtures that exercise inlining alone declare none.
#[cfg(test)]
pub(super) fn inline_derived_metrics(
    metrics: &[crate::model::Metric],
    facts: &[Fact],
    fact_topo_order: &[usize],
    tables: &[TableRef],
) -> Result<ResolvedMetricExprs, String> {
    inline_derived_metrics_with_joins(metrics, facts, fact_topo_order, tables, &[])
}

/// Resolve all metric expressions: inline facts into base metrics, then inline
/// base/derived metric references into derived metrics in topological order.
///
/// Returns a map from lowercased metric name to its fully-resolved expression,
/// plus the SG-8 rewrite failures (see [`ResolvedMetricExprs`]).
///
/// Processing order:
/// 1. Base metrics (`source_table.is_some()`): inline facts, apply the SG-8
///    `COUNT(*)` rewrite (below), store resolved expression
/// 2. Derived metrics (`source_table.is_none()`): topologically sort by inter-metric deps,
///    then for each derived metric, replace all known metric name references with
///    parenthesized resolved expressions
///
/// # SG-8: `COUNT(*)` rewrite for non-base source tables
///
/// All synthesized joins are LEFT JOINs, so a metric sourced on a table other
/// than the base/root table sees one NULL-extended row per base row with no
/// match — `COUNT(*)` silently over-counts by one per childless parent. For
/// every base metric whose `source_table` is not the first declared table,
/// `COUNT(*)` is rewritten to `COUNT("<alias>"."<pk>")` using the FIRST
/// PRIMARY KEY column declared for that table (NULL-extended rows have a NULL
/// PK and are excluded from the count). The alias is lowercased to match the
/// alias emitted in the JOIN clause. Metrics on the base table keep plain
/// `COUNT(*)` — the base table is never NULL-extended. Because the rewrite
/// runs here, at the shared base-metric resolution step, it propagates to
/// every emission path that consumes resolved expressions: the main
/// aggregation path, derived-metric inlining, semi-additive co-query
/// decomposition, and window-metric inner aggregates. If the source table has
/// no PRIMARY KEY declared the rewrite is impossible; the metric is recorded
/// in `count_star_no_pk` and the caller errors when (and only when) a query
/// actually uses it.
///
/// # TECH-DEBT #69: `USING`-scoped facts
///
/// `joins` lets a base metric's `USING` scope the facts it inlines. A metric
/// with `USING (r_dep)` is aggregated against the role-played table
/// joined under its scoped alias (`"ap" AS "ap__r_dep"`) — the bare alias is
/// never joined. A fact on that table inlines as its RAW expression (`ap.alt`),
/// so the inlined text must be requalified to the scoped alias, exactly as a
/// dimension expression on the same table is. Done here, at the shared
/// base-metric resolution step, and *after* fact inlining (the #54 order:
/// inline first, then rewrite) so it reaches every consumer of the resolved
/// expressions — the base-anchored SELECT, derived-metric inlining, the grain
/// CTEs, the semi-additive and window paths. Test callers that build no
/// relationships use the test-only `inline_derived_metrics`.
pub(super) fn inline_derived_metrics_with_joins(
    metrics: &[crate::model::Metric],
    facts: &[Fact],
    fact_topo_order: &[usize],
    tables: &[TableRef],
    joins: &[Join],
) -> Result<ResolvedMetricExprs, String> {
    let mut resolved: HashMap<String, String> = HashMap::new();
    let mut count_star_no_pk: HashMap<String, String> = HashMap::new();
    let base_alias = tables.first().map(|t| t.alias.to_ascii_lowercase());

    // Step 1: Resolve base metrics (have source_table) with fact inlining
    for met in metrics.iter().filter(|m| m.source_table.is_some()) {
        let mut expr = if facts.is_empty() {
            met.expr.clone()
        } else {
            inline_facts(&met.expr, facts, fact_topo_order)
        };
        // TECH-DEBT #69: requalify facts inlined from a USING-scoped table.
        expr = rewrite_using_scoped_refs(&expr, met, joins);
        // SG-8: COUNT(*) on a non-base source table (see doc comment above).
        if let Some(ref st) = met.source_table {
            let st_lower = st.to_ascii_lowercase();
            if base_alias.as_deref() != Some(st_lower.as_str()) {
                let pk = tables
                    .iter()
                    .find(|t| t.alias.to_ascii_lowercase() == st_lower)
                    .and_then(|t| t.pk_columns.first());
                if let Some(pk) = pk {
                    let qualified_pk = format!(
                        "{}.{}",
                        quote_stored_ident(&st_lower),
                        quote_stored_ident(pk)
                    );
                    // EXP-21/25/26: `COUNT(*)` is only the best-known spelling
                    // of the hazard — EVERY aggregate over the NULL-extended
                    // table needs its argument fenced. The argument guard runs
                    // FIRST and skips the bare `*`, so `COUNT(*)` reaches the
                    // star rewrite untouched and comes out as the plain
                    // `COUNT(<pk>)` SG-8 has always emitted rather than
                    // double-wrapped in a `CASE` over its own PK.
                    if let Some(rewritten) = guard_aggregate_args(&expr, &qualified_pk) {
                        expr = rewritten;
                    }
                    if let Some(rewritten) = rewrite_count_star(&expr, &qualified_pk) {
                        expr = rewritten;
                    }
                } else if rewrite_count_star(&expr, "*").is_some()
                    || has_constant_arg_aggregate(&expr)
                {
                    // No PK declared (or unknown alias): rewrite impossible.
                    // EXP-24: keyed like `resolved`, so a QUALIFIED reference
                    // to this metric still trips the no-PK error rather than
                    // silently emitting an un-guarded count.
                    for key in metric_keys(met.source_table.as_deref(), &met.name) {
                        count_star_no_pk.insert(key, st_lower.clone());
                    }
                }
            }
        }
        for key in metric_keys(met.source_table.as_deref(), &met.name) {
            resolved.insert(key, expr.clone());
        }
    }

    // Step 2: Collect derived metrics (no source_table)
    let derived: Vec<(usize, &crate::model::Metric)> = metrics
        .iter()
        .enumerate()
        .filter(|(_, m)| m.source_table.is_none())
        .collect();

    if derived.is_empty() {
        return Ok(ResolvedMetricExprs {
            exprs: resolved,
            count_star_no_pk,
        });
    }

    // Step 3: Topologically sort derived metrics and inline in order
    let derived_topo = toposort_derived(&derived, &resolved)?;

    // Step 3b: Enforce depth limit to prevent stack overflow from long chains
    if derived_topo.len() > MAX_DERIVATION_DEPTH {
        return Err(format!(
            "derived metric nesting depth {} exceeds maximum of {}",
            derived_topo.len(),
            MAX_DERIVATION_DEPTH
        ));
    }

    for idx in derived_topo {
        let met = derived[idx].1;
        // Start with the raw expression, with facts inlined first
        let raw_expr = if facts.is_empty() {
            met.expr.clone()
        } else {
            inline_facts(&met.expr, facts, fact_topo_order)
        };
        // Replace every known metric name with its resolved expression
        // (parenthesized) in ONE pass over the original text via the shared
        // reference tokenizer. Each bare metric reference resolves to exactly
        // one key, so — unlike the former word-boundary substitution — there is
        // no needle-ordering concern and no rescanning of inserted text (the
        // SG-3 double-substitution hazard): a metric named like a column used
        // qualified in another metric's expression (`revenue` vs `x.revenue`)
        // or appearing inside a string literal is left untouched (E-3).
        let expr = {
            let parenthesized: HashMap<String, String> = resolved
                .iter()
                .map(|(name, replacement)| (normalize_ident_part(name), format!("({replacement})")))
                .collect();
            let map: HashMap<String, &str> = parenthesized
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str()))
                .collect();
            inline_references(&raw_expr, &map)
        };
        resolved.insert(normalize_ident_part(&met.name), expr);
    }

    Ok(ResolvedMetricExprs {
        exprs: resolved,
        count_star_no_pk,
    })
}

/// Requalify references to each table a base metric's `USING` scopes, from the
/// bare alias to the role-playing scoped alias the join resolver emits for it
/// (TECH-DEBT #69, MM-1).
///
/// `USING` names relationships; each resolves (by the same case-insensitive name
/// match `join_resolver::resolve_joins_pkfk` uses to emit the scoped join) to
/// its target table. A target named by exactly ONE of the metric's
/// relationships (however many spellings of it the `USING` lists) is
/// rewritten; a target named by several distinct ones has no single role to
/// pick and is left alone, as is the metric's own table, which is never joined
/// under a scoped alias. Member expressions cannot reference another table's
/// raw columns (#52), so on a DDL-created view any reference to a scoped table
/// reached this point through an inlined fact.
fn rewrite_using_scoped_refs(expr: &str, met: &crate::model::Metric, joins: &[Join]) -> String {
    if met.using_relationships.is_empty() || joins.is_empty() {
        return expr.to_string();
    }
    let own = met.source_table.as_deref().map(normalize_ident_part);
    // target table -> the distinct relationships (join edges) of this metric
    // that reach it. Distinct by the EDGE a spelling resolves to, not by the
    // spelling: `USING (dep_airport, "DEP_AIRPORT")` names one relationship —
    // validation, the role-playing fence and the join resolver all resolve it
    // with `graph::find_relationship` — so it is one role, and must be
    // rewritten as one (PR #247 review). Counting spellings left the fact on
    // the never-joined bare alias. Conversely a view stored before #249 may
    // hold `dep` and `"dep"` as two edges; those spellings resolve to two
    // edges, so the fact is not rewritten (and the #69 fence reports it).
    let mut by_target: Vec<(String, Vec<(&str, &Join)>)> = Vec::new();
    for rel in &met.using_relationships {
        let Some(join) = crate::graph::find_relationship(joins, rel) else {
            continue;
        };
        let target = normalize_ident_part(&join.table);
        if own.as_deref() == Some(target.as_str()) {
            continue;
        }
        match by_target.iter_mut().find(|(t, _)| *t == target) {
            Some((_, rels)) => {
                if !rels.iter().any(|(_, j)| std::ptr::eq(*j, join)) {
                    rels.push((rel.as_str(), join));
                }
            }
            None => by_target.push((target, vec![(rel.as_str(), join)])),
        }
    }
    let mut out = expr.to_string();
    for (target, rels) in &by_target {
        if let [(rel, _)] = rels.as_slice() {
            let scoped = super::join_resolver::scoped_join_alias(target, rel);
            out = super::join_resolver::rewrite_to_scoped_alias(&out, target, &scoped);
        }
    }
    out
}

/// Collect the lowercased names of `met` and every metric it transitively
/// depends on: derived metrics contribute the metric names referenced in
/// their expressions; window metrics contribute their inner metric.
///
/// Used by the SG-8 check in `expand()` to decide whether a requested metric
/// reaches a base metric whose `COUNT(*)` could not be rewritten.
pub(super) fn collect_transitive_metric_names(
    met: &crate::model::Metric,
    all_metrics: &[crate::model::Metric],
) -> HashSet<String> {
    // Canonical identifier keys (quote-stripped + folded) throughout, so the
    // returned set is directly comparable to `count_star_no_pk`'s keys and a
    // quoted stored name matches its (quote-stripped) references — EXP-6.
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = vec![normalize_ident_part(&met.name)];

    let name_map: HashMap<String, &crate::model::Metric> = all_metrics
        .iter()
        .map(|m| (normalize_ident_part(&m.name), m))
        .collect();
    let all_names: Vec<String> = all_metrics
        .iter()
        .map(|m| normalize_ident_part(&m.name))
        .collect();

    while let Some(current_name) = stack.pop() {
        if !visited.insert(current_name.clone()) {
            continue;
        }
        let Some(current_met) = name_map.get(&current_name) else {
            continue;
        };
        if let Some(ref ws) = current_met.window_spec {
            stack.push(normalize_ident_part(&ws.inner_metric));
        }
        if current_met.source_table.is_none() {
            // Derived metric: find referenced metric names and push to stack.
            // A base metric may be referenced bare or by its own source table.
            for name in &all_names {
                let src = name_map.get(name).and_then(|m| m.source_table.as_deref());
                if *name != current_name && references_ref(&current_met.expr, name, src) {
                    stack.push(name.clone());
                }
            }
        }
    }

    visited
}

/// What a walk of the metric dependency graph found about where a metric
/// aggregates.
pub(crate) struct DerivedMetricGrain {
    /// The source tables of the base metrics reached, lowercased.
    pub(crate) tables: Vec<String>,
    /// Whether some metric on the walk aggregates at the ROOT grain without
    /// naming a table: no `source_table`, but an aggregate in its expression.
    ///
    /// EXP-11 (code-review 2026-08-03): such a metric contributes no entry to
    /// `tables`, so a derived metric mixing one with a real-table component
    /// used to report only the other component's grain — the root component
    /// vanished, and callers anchored the whole expression at the survivor.
    /// Reported separately rather than pushed into `tables` because the root
    /// alias is the *caller's* to supply (`GrainGraph::root`); the walk sees
    /// only the metric list.
    pub(crate) at_root: bool,
}

/// Collect source tables needed by a derived metric by walking the metric
/// dependency graph transitively.
pub(crate) fn collect_derived_metric_source_tables(
    met: &crate::model::Metric,
    all_metrics: &[crate::model::Metric],
) -> Vec<String> {
    collect_derived_metric_grain(met, all_metrics).tables
}

/// [`collect_derived_metric_source_tables`], keeping the root-grain component
/// the table list cannot express. See [`DerivedMetricGrain::at_root`].
pub(crate) fn collect_derived_metric_grain(
    met: &crate::model::Metric,
    all_metrics: &[crate::model::Metric],
) -> DerivedMetricGrain {
    let mut sources: HashSet<String> = HashSet::new();
    let mut at_root = false;
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = vec![met.name.to_ascii_lowercase()];

    // Build name -> metric lookup
    let name_map: HashMap<String, &crate::model::Metric> = all_metrics
        .iter()
        .map(|m| (m.name.to_ascii_lowercase(), m))
        .collect();

    // Collect all metric names for word-boundary scanning
    let all_names: Vec<String> = all_metrics
        .iter()
        .map(|m| m.name.to_ascii_lowercase())
        .collect();

    while let Some(current_name) = stack.pop() {
        if !visited.insert(current_name.clone()) {
            continue;
        }
        let Some(current_met) = name_map.get(&current_name) else {
            continue;
        };

        if let Some(ref st) = current_met.source_table {
            // Base metric: add its source table
            sources.insert(st.to_ascii_lowercase());
        } else {
            // A source-less metric that AGGREGATES is not a pure composition of
            // other metrics: it reads columns of its own, and with no alias to
            // name them it reads the root table's (EXP-8). That is a grain, and
            // it is invisible in `sources` — record it (EXP-11).
            //
            // A WINDOW metric is exempt: its expression is `<fn>(<inner>) OVER
            // (…)`, an aggregate over a *metric reference*, and the row set it
            // runs over is the inner metric's pre-aggregated CTE rather than
            // any table of its own. Its grain is the inner metric's, which
            // `metric_grain` adds separately — counting the outer window
            // function here would put every window metric at the root grain on
            // top of that, and a window metric over a child-table aggregate
            // would then fan-trap against its own dimension.
            if current_met.window_spec.is_none()
                && crate::graph::contains_aggregate_function(&current_met.expr).is_some()
            {
                at_root = true;
            }
            // Derived metric: find referenced metric names and push to stack.
            // A base metric may be referenced bare or by its own source table.
            for name in &all_names {
                let src = name_map.get(name).and_then(|m| m.source_table.as_deref());
                if *name != current_name && references_ref(&current_met.expr, name, src) {
                    stack.push(name.clone());
                }
            }
        }
    }

    DerivedMetricGrain {
        tables: sources.into_iter().collect(),
        at_root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccessModifier, Metric};

    #[test]
    fn the_guard_names_the_shared_aggregates_but_not_the_unguarded_ones() {
        for name in [
            "sum",
            "SUM",
            "count",
            "sum_no_overflow",
            "arg_max",
            "quantile_disc",
        ] {
            assert!(is_aggregate_name(name), "{name}");
        }
        for name in [
            "count_star",
            "percentile_cont",
            "PERCENTILE_DISC",
            "coalesce",
            "lag",
        ] {
            assert!(!is_aggregate_name(name), "{name}");
        }
    }

    fn make_metric(name: &str, expr: &str, source_table: Option<&str>) -> Metric {
        Metric {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: source_table.map(|s| s.to_string()),
            output_type: None,
            using_relationships: vec![],
            comment: None,
            synonyms: vec![],
            access: AccessModifier::Public,
            non_additive_by: vec![],
            window_spec: None,
            tags: Vec::new(),
        }
    }

    fn make_fact_on(name: &str, expr: &str, source_table: Option<&str>) -> Fact {
        Fact {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: source_table.map(std::string::ToString::to_string),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        }
    }

    /// PAR-6: the reference has to be *own-qualified* to count. `c.discount`
    /// names a column on `c`; `c.cust_discount` names the fact `cust_discount`
    /// declared on `c`. Only the latter reaches the fact — and reaching it is
    /// what puts `c` in the join set.
    #[test]
    fn referenced_facts_finds_an_own_qualified_cross_table_reference() {
        let facts = vec![make_fact_on("cust_discount", "c.discount", Some("c"))];
        assert_eq!(
            collect_referenced_facts("SUM(o.amount - c.cust_discount)", &facts),
            vec![("cust_discount".to_string(), "c".to_string())]
        );
    }

    /// A bare reference resolves too — the same rule `toposort_facts` and
    /// `inline_facts` apply, so the join set cannot disagree with what was
    /// actually inlined.
    #[test]
    fn referenced_facts_finds_a_bare_reference() {
        let facts = vec![make_fact_on("cust_discount", "c.discount", Some("c"))];
        assert_eq!(
            collect_referenced_facts("SUM(o.amount - cust_discount)", &facts),
            vec![("cust_discount".to_string(), "c".to_string())]
        );
    }

    /// A *foreign* qualifier is a column on another relation, not a fact
    /// reference (E-3) — so it must not drag that table into the join set.
    #[test]
    fn referenced_facts_ignores_a_foreign_qualified_name() {
        let facts = vec![make_fact_on("discount", "c.raw_discount", Some("c"))];
        assert!(
            collect_referenced_facts("SUM(o.amount - x.discount)", &facts).is_empty(),
            "x.discount is a column on x, not the fact `discount` declared on c"
        );
    }

    /// Facts chain, so the walk is transitive: reaching `a` on `t1` through
    /// `b` on `t2` has to put BOTH tables in the set, or whichever expression
    /// got inlined second names an alias that is not in scope.
    #[test]
    fn referenced_facts_walks_transitively() {
        let facts = vec![
            make_fact_on("outer", "t1.x + t2.middle", Some("t1")),
            make_fact_on("middle", "t2.y", Some("t2")),
        ];
        let reached = collect_referenced_facts("SUM(t0.v - t1.outer)", &facts);
        assert_eq!(
            reached,
            vec![
                ("outer".to_string(), "t1".to_string()),
                ("middle".to_string(), "t2".to_string()),
            ]
        );
    }

    /// A fact that declares no source table contributes no join: it resolves
    /// against the host expression's own scope. It is still walked through, so
    /// a table reached *beyond* it is not lost.
    #[test]
    fn referenced_facts_skips_a_source_less_fact_but_walks_through_it() {
        let facts = vec![
            make_fact_on("plain", "raw + u.parent", None),
            make_fact_on("parent", "u.w", Some("u")),
        ];
        assert_eq!(
            collect_referenced_facts("SUM(plain)", &facts),
            vec![("parent".to_string(), "u".to_string())]
        );
    }

    /// `validate_facts` rejects a cycle at CREATE, but the expander must not
    /// hang if one ever reaches it — `seen` bounds the walk.
    #[test]
    fn referenced_facts_terminates_on_a_cycle() {
        let facts = vec![
            make_fact_on("a", "t1.x + t2.b", Some("t1")),
            make_fact_on("b", "t2.y + t1.a", Some("t2")),
        ];
        assert_eq!(
            collect_referenced_facts("SUM(t1.a)", &facts),
            vec![
                ("a".to_string(), "t1".to_string()),
                ("b".to_string(), "t2".to_string()),
            ],
            "both tables reached, and the walk back into `a` stops rather than looping"
        );
    }

    /// Two facts on the SAME table collapse to one join: the set is keyed by
    /// alias, because joining a table twice for two references would be wrong,
    /// not merely redundant.
    #[test]
    fn referenced_facts_dedupes_two_facts_on_one_table() {
        let facts = vec![
            make_fact_on("lo", "c.floor", Some("c")),
            make_fact_on("hi", "c.ceiling", Some("c")),
        ];
        assert_eq!(
            collect_referenced_fact_tables("SUM(o.v - c.lo + c.hi)", &facts),
            vec!["c".to_string()]
        );
    }

    #[test]
    fn toposort_derived_detects_cycle() {
        let met_a = make_metric("a", "b + 1", None);
        let met_b = make_metric("b", "a + 1", None);
        let derived: Vec<(usize, &Metric)> = vec![(0, &met_a), (1, &met_b)];
        let resolved = HashMap::new();
        let result = toposort_derived(&derived, &resolved);
        assert!(result.is_err(), "Expected cycle error");
        assert!(
            result.unwrap_err().contains("cycle"),
            "Error should mention cycle"
        );
    }

    #[test]
    fn toposort_derived_no_cycle_succeeds() {
        let _met_a = make_metric("a", "SUM(x)", Some("t"));
        let met_b = make_metric("b", "a + 1", None);
        // Only derived metrics go into toposort_derived; 'a' is base
        let derived: Vec<(usize, &Metric)> = vec![(1, &met_b)];
        let resolved = HashMap::new();
        let result = toposort_derived(&derived, &resolved);
        assert!(result.is_ok(), "Non-cyclic should succeed");
    }

    #[test]
    fn max_derivation_depth_constant() {
        assert_eq!(MAX_DERIVATION_DEPTH, 64);
    }

    #[test]
    fn inline_derived_metrics_name_matching_column_is_not_double_substituted() {
        // SG-3 regression (code-review 2026-07-02): metric `revenue` also
        // appears as the column reference `o.revenue` inside `tax`'s
        // expression (`.` is a word boundary). The old sequential per-name
        // substitution re-scanned inserted text in HashMap iteration order:
        // when `tax` happened to be inlined first, the subsequent `revenue`
        // pass also matched `revenue` inside the freshly inserted
        // `SUM(o.revenue * 0.1)`, corrupting the expression into invalid
        // nested-aggregate SQL on a hash-seed-dependent fraction of runs.
        // The single combined pass must produce this exact expression, every
        // run.
        let metrics = vec![
            make_metric("revenue", "SUM(o.revenue)", Some("o")),
            make_metric("tax", "SUM(o.revenue * 0.1)", Some("o")),
            make_metric("after_tax", "revenue - tax", None),
        ];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
            .unwrap()
            .exprs;
        assert_eq!(
            resolved.get("after_tax").unwrap(),
            "(SUM(o.revenue)) - (SUM(o.revenue * 0.1))"
        );
    }

    #[test]
    fn inline_derived_metrics_chained_derived_not_rescanned() {
        // A derived metric referencing another derived metric: the inner
        // resolution is inserted verbatim and must not be re-scanned even
        // though it contains the names of other metrics.
        let metrics = vec![
            make_metric("revenue", "SUM(o.revenue)", Some("o")),
            make_metric("cost", "SUM(o.cost)", Some("o")),
            make_metric("profit", "revenue - cost", None),
            make_metric("margin", "profit / revenue", None),
        ];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
            .unwrap()
            .exprs;
        assert_eq!(
            resolved.get("margin").unwrap(),
            "((SUM(o.revenue)) - (SUM(o.cost))) / (SUM(o.revenue))"
        );
    }

    #[test]
    fn inline_derived_metrics_cycle_returns_err() {
        let metrics = vec![
            make_metric("a", "b + 1", None),
            make_metric("b", "a + 1", None),
        ];
        let result = inline_derived_metrics(&metrics, &[], &[], &[]);
        assert!(result.is_err(), "Cycle should produce error");
        let err = result.unwrap_err();
        assert!(err.contains("cycle"), "Error should mention cycle: {err}");
    }

    #[test]
    fn inline_derived_metrics_normal_succeeds() {
        let metrics = vec![
            make_metric("revenue", "SUM(amount)", Some("o")),
            make_metric("cost", "SUM(unit_cost)", Some("o")),
            make_metric("profit", "revenue - cost", None),
        ];
        let result = inline_derived_metrics(&metrics, &[], &[], &[]);
        assert!(result.is_ok(), "Non-cyclic should succeed");
        let resolved = result.unwrap().exprs;
        assert_eq!(
            resolved.get("profit").unwrap(),
            "(SUM(amount)) - (SUM(unit_cost))"
        );
    }

    #[test]
    fn inline_derived_metrics_mixed_case_references_are_inlined() {
        // E-2 regression (code-review 2026-07-11): the CREATE-time validators
        // resolve metric references case-insensitively, but the substitution
        // scanner compared raw bytes — `profit AS REVENUE - Cost` passed
        // validation, skipped inlining, and leaked raw identifiers into the
        // generated SQL (erroring or silently "working" depending on which
        // other metrics were co-queried).
        let metrics = vec![
            make_metric("revenue", "SUM(o.rev)", Some("o")),
            make_metric("cost", "SUM(o.cost)", Some("o")),
            make_metric("profit", "REVENUE - Cost", None),
        ];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
            .unwrap()
            .exprs;
        assert_eq!(
            resolved.get("profit").unwrap(),
            "(SUM(o.rev)) - (SUM(o.cost))"
        );
    }

    #[test]
    fn inline_facts_mixed_case_references_are_inlined() {
        // E-2, facts arm: fact references are validated case-insensitively
        // (graph/facts.rs lowercases both sides), so inlining must match
        // any-case references to an as-declared fact name.
        let facts = vec![Fact {
            name: "net_price".to_string(),
            expr: "price * (1 - discount)".to_string(),
            source_table: Some("o".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        }];
        let topo = toposort_facts(&facts).unwrap();
        let result = inline_facts("SUM(Net_Price)", &facts, &topo);
        assert_eq!(result, "SUM((price * (1 - discount)))");
    }

    #[test]
    fn inline_facts_does_not_capture_qualified_column_on_other_table() {
        // E-3 (code-review 2026-07-11): a bare fact reference is inlined, but
        // the fact name appearing as the column part of a qualified reference
        // on a *different* relation (`x.net_price`) must be left untouched —
        // substituting there produced invalid SQL (`x.(price * ...)`).
        let facts = vec![Fact {
            name: "net_price".to_string(),
            expr: "price * (1 - discount)".to_string(),
            source_table: Some("o".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        }];
        let topo = toposort_facts(&facts).unwrap();
        // Bare reference inlined; `x.net_price` (other table's column) intact.
        assert_eq!(
            inline_facts("SUM(net_price) + x.net_price", &facts, &topo),
            "SUM((price * (1 - discount))) + x.net_price"
        );
        // The fact's own qualified form is still matched as a whole.
        assert_eq!(
            inline_facts("o.net_price + x.net_price", &facts, &topo),
            "(price * (1 - discount)) + x.net_price"
        );
    }

    #[test]
    fn inline_facts_cross_table_own_qualified_reference_is_inlined() {
        // A fact on one table referenced by a fact on ANOTHER table via the
        // referenced fact's OWN source-qualified form (`b_tbl.leaf`) must be
        // inlined — the replacement map keys each fact by its own source table,
        // not the host expression's, so detection (toposort) and inlining agree.
        let facts = vec![
            Fact {
                name: "leaf".to_string(),
                expr: "b_tbl.col".to_string(),
                source_table: Some("b_tbl".to_string()),
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                access: AccessModifier::Public,
                tags: Vec::new(),
                sample_values: Vec::new(),
            },
            Fact {
                name: "top".to_string(),
                // references `leaf` qualified by leaf's own table `b_tbl`
                expr: "b_tbl.leaf + 1".to_string(),
                source_table: Some("a_tbl".to_string()),
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                access: AccessModifier::Public,
                tags: Vec::new(),
                sample_values: Vec::new(),
            },
        ];
        let topo = toposort_facts(&facts).unwrap();
        assert_eq!(
            inline_facts("SUM(top)", &facts, &topo),
            "SUM(((b_tbl.col) + 1))"
        );
    }

    #[test]
    fn inline_derived_metrics_does_not_capture_qualified_column_on_other_table() {
        // E-3, derived-metric arm: a bare metric reference is inlined, but the
        // same name as a qualified column on another relation (`x.revenue`) is
        // left alone.
        let metrics = vec![
            make_metric("revenue", "SUM(o.rev)", Some("o")),
            make_metric("profit", "revenue - x.revenue", None),
        ];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
            .unwrap()
            .exprs;
        assert_eq!(resolved.get("profit").unwrap(), "(SUM(o.rev)) - x.revenue");
    }

    #[test]
    fn inline_facts_leaves_string_literals_intact() {
        // E-3 string arm (code-review 2026-07-16): a fact name appearing inside
        // a single-quoted string literal must never be substituted into — only
        // the bare identifier reference is inlined.
        let facts = vec![Fact {
            name: "net_price".to_string(),
            expr: "price * (1 - discount)".to_string(),
            source_table: Some("o".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        }];
        let topo = toposort_facts(&facts).unwrap();
        assert_eq!(
            inline_facts("COALESCE(net_price, 'net_price missing')", &facts, &topo),
            "COALESCE((price * (1 - discount)), 'net_price missing')"
        );
    }

    #[test]
    fn inline_derived_metrics_leaves_string_literals_intact() {
        let metrics = vec![
            make_metric("revenue", "SUM(o.rev)", Some("o")),
            make_metric("label", "revenue || ' revenue total'", None),
        ];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
            .unwrap()
            .exprs;
        assert_eq!(
            resolved.get("label").unwrap(),
            "(SUM(o.rev)) || ' revenue total'"
        );
    }

    #[test]
    fn inline_facts_quoted_reference_is_inlined() {
        // TECH-DEBT #28 (code-review 2026-07-16): a reference written `"Net_Price"`
        // matches the declaration `net_price` — DuckDB treats quoted identifiers
        // as case-insensitive, and the shared tokenizer normalizes both sides.
        let facts = vec![Fact {
            name: "net_price".to_string(),
            expr: "price * (1 - discount)".to_string(),
            source_table: Some("o".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        }];
        let topo = toposort_facts(&facts).unwrap();
        assert_eq!(
            inline_facts("SUM(\"Net_Price\")", &facts, &topo),
            "SUM((price * (1 - discount)))"
        );
    }

    #[test]
    fn inline_derived_metrics_quoted_and_mixed_case_references_are_inlined() {
        // E-2 + #28: a mixed-case and a quoted derived-metric reference both
        // resolve against the lowercase declaration.
        let metrics = vec![
            make_metric("revenue", "SUM(o.rev)", Some("o")),
            make_metric("cost", "SUM(o.cost)", Some("o")),
            make_metric("profit", "REVENUE - \"Cost\"", None),
        ];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &[])
            .unwrap()
            .exprs;
        assert_eq!(
            resolved.get("profit").unwrap(),
            "(SUM(o.rev)) - (SUM(o.cost))"
        );
    }

    #[test]
    fn inline_derived_metrics_depth_limit_exceeded() {
        // Create a chain of 65 derived metrics: m0 -> m1 -> ... -> m64
        // m0 is base, m1..m64 are derived (64 derived exceeds the limit since
        // MAX_DERIVATION_DEPTH == 64 and we check > not >=)
        let mut metrics = vec![make_metric("m0", "SUM(x)", Some("t"))];
        for i in 1..=MAX_DERIVATION_DEPTH + 1 {
            metrics.push(make_metric(
                &format!("m{i}"),
                &format!("m{} + 1", i - 1),
                None,
            ));
        }
        let result = inline_derived_metrics(&metrics, &[], &[], &[]);
        assert!(result.is_err(), "Depth exceeding limit should error");
        let err = result.unwrap_err();
        assert!(
            err.contains("nesting depth") && err.contains("maximum"),
            "Error should mention depth limit: {err}"
        );
    }

    // --- rewrite_count_star tests (SG-8) ---

    fn make_table(alias: &str, pk: &[&str]) -> TableRef {
        TableRef {
            alias: alias.to_string(),
            table: alias.to_string(),
            pk_columns: pk.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn rewrite_count_star_basic() {
        assert_eq!(
            rewrite_count_star("COUNT(*)", "\"li\".\"id\"").as_deref(),
            Some("COUNT(\"li\".\"id\")")
        );
    }

    #[test]
    fn rewrite_count_star_preserves_case_and_handles_spaces() {
        assert_eq!(
            rewrite_count_star("count( * )", "\"li\".\"id\"").as_deref(),
            Some("count(\"li\".\"id\")")
        );
        assert_eq!(
            rewrite_count_star("Count (*)", "x").as_deref(),
            Some("Count (x)")
        );
    }

    #[test]
    fn rewrite_count_star_inside_larger_expression() {
        assert_eq!(
            rewrite_count_star("COUNT(*) * 2 + COUNT(*)", "\"li\".\"id\"").as_deref(),
            Some("COUNT(\"li\".\"id\") * 2 + COUNT(\"li\".\"id\")")
        );
    }

    #[test]
    fn rewrite_count_star_none_when_absent() {
        assert!(rewrite_count_star("COUNT(li.id)", "x").is_none());
        assert!(rewrite_count_star("SUM(amount)", "x").is_none());
        // `*` as multiplication, not a star argument
        assert!(rewrite_count_star("COUNT(a * b)", "x").is_none());
    }

    #[test]
    fn rewrite_count_star_skips_string_literals_and_word_boundaries() {
        // Inside a single-quoted literal: untouched.
        assert!(rewrite_count_star("'COUNT(*)'", "x").is_none());
        // `miscount(*)` is not `count` at a word boundary.
        assert!(rewrite_count_star("miscount(*)", "x").is_none());
    }

    #[test]
    fn rewrite_count_star_skips_a_name_continued_by_a_dollar() {
        // `x$count` is one function name in DuckDB, not a call to COUNT.
        assert!(rewrite_count_star("x$count(*)", "x").is_none());
    }

    // EXP-16: this scanner tracked only single quotes, so `count(*)` occurring
    // inside a double-quoted identifier or a dollar-quoted literal was rewritten
    // as if it were live code — corrupting the identifier or the literal. Both
    // regions are inert everywhere else in the codebase (PARSE-1); they are
    // inert here too. Two separate tests so each region reports independently.

    #[test]
    fn rewrite_count_star_ignores_a_double_quoted_identifier() {
        // A column literally named `count(*)` — the text is an identifier, not a call.
        assert!(rewrite_count_star("\"my count(*) col\"", "x").is_none());
        // …and one alongside a genuine call: only the call is rewritten.
        assert_eq!(
            rewrite_count_star("COUNT(*) + \"count(*)\"", "\"li\".\"id\"").as_deref(),
            Some("COUNT(\"li\".\"id\") + \"count(*)\"")
        );
    }

    #[test]
    fn rewrite_count_star_ignores_a_dollar_quoted_literal() {
        assert!(rewrite_count_star("$$count(*)$$", "x").is_none());
        // Tagged form, and a genuine call outside it.
        assert_eq!(
            rewrite_count_star("COUNT(*) || $tag$count(*)$tag$", "\"li\".\"id\"").as_deref(),
            Some("COUNT(\"li\".\"id\") || $tag$count(*)$tag$")
        );
    }

    // --- inline_derived_metrics COUNT(*) rewrite tests (SG-8) ---

    #[test]
    fn inline_derived_metrics_rewrites_count_star_on_non_base_table() {
        let tables = vec![make_table("o", &["id"]), make_table("li", &["id"])];
        let metrics = vec![make_metric("item_count", "COUNT(*)", Some("li"))];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &tables).unwrap();
        assert_eq!(
            resolved.exprs.get("item_count").unwrap(),
            "COUNT(\"li\".\"id\")"
        );
        assert!(resolved.count_star_no_pk.is_empty());
    }

    #[test]
    fn inline_derived_metrics_keeps_count_star_on_base_table() {
        let tables = vec![make_table("o", &["id"]), make_table("li", &["id"])];
        let metrics = vec![make_metric("order_count", "COUNT(*)", Some("o"))];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &tables).unwrap();
        assert_eq!(resolved.exprs.get("order_count").unwrap(), "COUNT(*)");
        assert!(resolved.count_star_no_pk.is_empty());
    }

    #[test]
    fn inline_derived_metrics_records_no_pk_failure() {
        // li declares no PRIMARY KEY: the rewrite is impossible and the
        // metric is recorded so the caller can error when it is queried.
        let tables = vec![make_table("o", &["id"]), make_table("li", &[])];
        let metrics = vec![make_metric("item_count", "COUNT(*)", Some("li"))];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &tables).unwrap();
        assert_eq!(resolved.exprs.get("item_count").unwrap(), "COUNT(*)");
        assert_eq!(
            resolved
                .count_star_no_pk
                .get("item_count")
                .map(String::as_str),
            Some("li")
        );
    }

    #[test]
    fn inline_derived_metrics_rewrite_propagates_into_derived() {
        // The rewrite runs at base-metric resolution, BEFORE derived-metric
        // inlining, so derived metrics inherit the rewritten text.
        let tables = vec![make_table("o", &["id"]), make_table("li", &["li_id"])];
        let metrics = vec![
            make_metric("item_count", "COUNT(*)", Some("li")),
            make_metric("double_items", "item_count * 2", None),
        ];
        let resolved = inline_derived_metrics(&metrics, &[], &[], &tables).unwrap();
        assert_eq!(
            resolved.exprs.get("double_items").unwrap(),
            "(COUNT(\"li\".\"li_id\")) * 2"
        );
    }

    // --- collect_transitive_metric_names tests (SG-8 check support) ---

    #[test]
    fn collect_transitive_metric_names_derived_and_window() {
        let mut window_met = make_metric("rolling_items", "AVG(item_count)", None);
        window_met.window_spec = Some(crate::model::WindowSpec {
            window_function: "AVG".to_string(),
            inner_metric: "item_count".to_string(),
            ..Default::default()
        });
        let metrics = vec![
            make_metric("item_count", "COUNT(*)", Some("li")),
            make_metric("double_items", "item_count * 2", None),
            window_met,
        ];
        let via_derived = collect_transitive_metric_names(&metrics[1], &metrics);
        assert!(via_derived.contains("double_items"));
        assert!(via_derived.contains("item_count"));
        let via_window = collect_transitive_metric_names(&metrics[2], &metrics);
        assert!(via_window.contains("rolling_items"));
        assert!(
            via_window.contains("item_count"),
            "window metrics must chase their inner metric: {via_window:?}"
        );
    }

    // --- Helper for metrics with using_relationships ---

    fn make_metric_with_using(
        name: &str,
        expr: &str,
        source_table: Option<&str>,
        using: &[&str],
    ) -> Metric {
        let mut m = make_metric(name, expr, source_table);
        m.using_relationships = using.iter().map(|s| s.to_string()).collect();
        m
    }

    fn make_fact(name: &str, expr: &str) -> Fact {
        Fact {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some("t".to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        }
    }

    // --- rewrite_using_scoped_refs: legacy stored names (PR #249 review) ---

    /// A pre-#249 stored row holding `dep` and `"dep"` as two roles of `a`
    /// (CREATE then compared names by spelling). `USING (dep, "dep")` names two
    /// different edges, so the fact has no single role and must stay on the
    /// bare alias — the #69 fence reports it. Resolving both spellings to the
    /// first identifier match counted one edge and rewrote the reference.
    #[test]
    fn rewrite_using_scoped_refs_keeps_two_legacy_spellings_as_two_roles() {
        let join = |name: &str, fk: &str| Join {
            table: "a".to_string(),
            from_alias: "f".to_string(),
            fk_columns: vec![fk.to_string()],
            name: Some(name.to_string()),
            ..Default::default()
        };
        let joins = vec![join("dep", "dep_code"), join("\"dep\"", "arr_code")];
        let met = make_metric_with_using("m", "SUM(a.elevation)", Some("f"), &["dep", "\"dep\""]);
        assert_eq!(
            rewrite_using_scoped_refs("SUM(a.elevation)", &met, &joins),
            "SUM(a.elevation)"
        );
        // Control: one spelling names one edge, which is rewritten.
        let met = make_metric_with_using("m", "SUM(a.elevation)", Some("f"), &["\"dep\""]);
        assert_eq!(
            rewrite_using_scoped_refs("SUM(a.elevation)", &met, &joins),
            "SUM(a__dep.elevation)"
        );
    }

    // --- collect_derived_metric_using tests ---

    #[test]
    fn test_collect_derived_metric_using_base_with_using() {
        let met = make_metric_with_using(
            "flight_count",
            "count(*)",
            Some("flights"),
            &["dep_airport"],
        );
        let result = collect_derived_metric_using(&met, &[met.clone()]);
        assert!(
            result.contains(&"dep_airport".to_string()),
            "Should contain dep_airport, got: {result:?}"
        );
    }

    #[test]
    fn test_collect_derived_metric_using_derived_transitive() {
        let base_met = make_metric_with_using("base_count", "count(*)", Some("flights"), &["rel1"]);
        let derived = make_metric("derived_total", "base_count + 1", None);
        let all = vec![base_met, derived.clone()];
        let result = collect_derived_metric_using(&derived, &all);
        assert!(
            result.contains(&"rel1".to_string()),
            "Should transitively contain rel1, got: {result:?}"
        );
    }

    #[test]
    fn test_collect_derived_metric_using_no_using() {
        let met = make_metric("revenue", "sum(amount)", Some("orders"));
        let result = collect_derived_metric_using(&met, &[met.clone()]);
        assert!(
            result.is_empty(),
            "No using_relationships should return empty"
        );
    }

    #[test]
    fn test_collect_derived_metric_using_multiple_transitive() {
        // Derived metric references two base metrics each with different USING
        let base1 =
            make_metric_with_using("dep_count", "count(*)", Some("flights"), &["dep_airport"]);
        let base2 =
            make_metric_with_using("arr_count", "count(*)", Some("flights"), &["arr_airport"]);
        let derived = make_metric("total_count", "dep_count + arr_count", None);
        let all = vec![base1, base2, derived.clone()];
        let result = collect_derived_metric_using(&derived, &all);
        assert!(
            result.contains(&"dep_airport".to_string()),
            "Should contain dep_airport, got: {result:?}"
        );
        assert!(
            result.contains(&"arr_airport".to_string()),
            "Should contain arr_airport, got: {result:?}"
        );
    }

    // --- toposort_facts tests ---

    #[test]
    fn test_toposort_facts_empty() {
        let result = toposort_facts(&[]);
        assert_eq!(result.unwrap(), Vec::<usize>::new());
    }

    #[test]
    fn test_toposort_facts_single() {
        let facts = vec![make_fact("net_price", "price * (1 - discount)")];
        let result = toposort_facts(&facts).unwrap();
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn test_toposort_facts_chain() {
        let facts = vec![
            make_fact("net_price", "price * (1 - discount)"),
            make_fact("total", "net_price * quantity"),
        ];
        let result = toposort_facts(&facts).unwrap();
        // net_price (index 0) must come before total (index 1)
        let pos_net = result.iter().position(|&x| x == 0).unwrap();
        let pos_total = result.iter().position(|&x| x == 1).unwrap();
        assert!(
            pos_net < pos_total,
            "net_price should come before total in topo order"
        );
    }

    #[test]
    fn test_toposort_facts_independent() {
        let facts = vec![
            make_fact("tax_amount", "price * tax_rate"),
            make_fact("discount_amount", "price * discount_rate"),
        ];
        let result = toposort_facts(&facts).unwrap();
        assert_eq!(result.len(), 2, "Both facts should appear");
        assert!(result.contains(&0));
        assert!(result.contains(&1));
    }

    #[test]
    fn test_toposort_facts_cycle() {
        let facts = vec![make_fact("a", "b + 1"), make_fact("b", "a + 1")];
        let result = toposort_facts(&facts);
        assert!(result.is_err(), "Cycle should be detected");
        assert!(
            result.unwrap_err().contains("cycle"),
            "Error should mention cycle"
        );
    }

    // --- inline_facts tests ---

    #[test]
    fn test_inline_facts_empty_facts() {
        let result = inline_facts("SUM(amount)", &[], &[]);
        assert_eq!(
            result, "SUM(amount)",
            "Empty facts should return expr unchanged"
        );
    }

    #[test]
    fn test_inline_facts_single_substitution() {
        let facts = vec![make_fact("net_price", "price * (1 - discount)")];
        let topo = vec![0];
        let result = inline_facts("SUM(net_price)", &facts, &topo);
        assert_eq!(
            result, "SUM((price * (1 - discount)))",
            "Should inline the fact expression parenthesized"
        );
    }

    #[test]
    fn test_inline_facts_chained_substitution() {
        let facts = vec![
            make_fact("net_price", "price * (1 - discount)"),
            make_fact("total", "net_price * quantity"),
        ];
        // topo order: net_price first (index 0), then total (index 1)
        let topo = vec![0, 1];
        let result = inline_facts("SUM(total)", &facts, &topo);
        // total resolves to ((price * (1 - discount)) * quantity)
        assert!(
            result.contains("price * (1 - discount)"),
            "Should resolve inner fact first, got: {result}"
        );
        assert!(
            result.contains("quantity"),
            "Should contain quantity, got: {result}"
        );
    }

    #[test]
    fn test_inline_facts_qualified_form() {
        let mut fact = make_fact("net_price", "price * (1 - discount)");
        fact.source_table = Some("o".to_string());
        let facts = vec![fact];
        let topo = vec![0];
        let result = inline_facts("SUM(o.net_price)", &facts, &topo);
        assert!(
            result.contains("price * (1 - discount)"),
            "Should replace qualified form o.net_price, got: {result}"
        );
    }

    #[test]
    fn test_inline_facts_identity_qualified_no_double_sub() {
        // Identity passthrough: fact `unit_price` whose expression is the qualified
        // column `s.unit_price`. The SELECT path passes the fact's own expr through
        // inline_facts. It must NOT double-substitute into `(s.(s.unit_price))`.
        let mut fact = make_fact("unit_price", "s.unit_price");
        fact.source_table = Some("s".to_string());
        let facts = vec![fact];
        let topo = vec![0];
        let result = inline_facts("s.unit_price", &facts, &topo);
        assert_eq!(
            result, "(s.unit_price)",
            "Identity fact must resolve to its column once, got: {result}"
        );
    }

    #[test]
    fn test_inline_facts_identity_referenced_by_metric() {
        // A metric referencing an identity fact by its qualified column must inline
        // cleanly to a single column reference.
        let mut fact = make_fact("unit_price", "s.unit_price");
        fact.source_table = Some("s".to_string());
        let facts = vec![fact];
        let topo = vec![0];
        let result = inline_facts("SUM(s.unit_price)", &facts, &topo);
        assert_eq!(
            result, "SUM((s.unit_price))",
            "Metric over identity fact must inline once, got: {result}"
        );
    }

    // --- collect_derived_metric_source_tables tests ---

    #[test]
    fn test_collect_source_tables_base_metric() {
        let met = make_metric("revenue", "sum(amount)", Some("orders"));
        let result = collect_derived_metric_source_tables(&met, &[met.clone()]);
        assert!(
            result.contains(&"orders".to_string()),
            "Should contain orders, got: {result:?}"
        );
    }

    #[test]
    fn test_collect_source_tables_derived_transitive() {
        let base = make_metric("revenue", "sum(amount)", Some("orders"));
        let derived = make_metric("profit", "revenue - cost", None);
        let cost = make_metric("cost", "sum(unit_cost)", Some("items"));
        let all = vec![base, derived.clone(), cost];
        let result = collect_derived_metric_source_tables(&derived, &all);
        assert!(
            result.contains(&"orders".to_string()),
            "Should transitively contain orders, got: {result:?}"
        );
        assert!(
            result.contains(&"items".to_string()),
            "Should transitively contain items, got: {result:?}"
        );
    }

    #[test]
    fn test_collect_source_tables_cycle_handling() {
        // Two metrics referencing each other (defensive: visited set prevents infinite loop)
        let met_a = make_metric("a", "b + 1", None);
        let met_b = make_metric("b", "a + 1", None);
        let all = vec![met_a.clone(), met_b];
        // Should terminate without hanging
        let result = collect_derived_metric_source_tables(&met_a, &all);
        // No source tables found (both are derived with no base)
        assert!(
            result.is_empty(),
            "Cycle with no base metrics should return empty, got: {result:?}"
        );
    }

    // --- collect_derived_metric_grain: the root-grain component (EXP-11) ---

    /// A dependency that aggregates but names no table contributes no entry to
    /// the table list — it reads the ROOT table's columns. The walk reports it
    /// separately so callers can substitute their own root alias; without that
    /// the grain of `mixed` below is `{customers}` alone and the `sum(amount)`
    /// half silently rides whatever anchor `customers` gets.
    #[test]
    fn test_collect_grain_rootless_aggregate_dependency_reports_at_root() {
        let rootless = make_metric("rootless_total", "sum(amount)", None);
        let balance = make_metric("balance", "sum(c.balance)", Some("customers"));
        let mixed = make_metric("mixed", "balance - rootless_total", None);
        let all = vec![rootless, balance, mixed.clone()];
        let grain = collect_derived_metric_grain(&mixed, &all);
        assert!(
            grain.at_root,
            "the source-less aggregate dependency sits at the root grain"
        );
        assert_eq!(
            grain.tables,
            vec!["customers".to_string()],
            "the named half is unchanged, got: {:?}",
            grain.tables
        );
    }

    /// The flag is about *aggregation without a table*, not about being
    /// source-less: an ordinary derived metric composing two base metrics
    /// carries no grain of its own and must not be pushed to the root, or every
    /// derived metric would fan-trap against the root's own dimensions.
    #[test]
    fn test_collect_grain_plain_derived_metric_is_not_at_root() {
        let base = make_metric("revenue", "sum(amount)", Some("orders"));
        let cost = make_metric("cost", "sum(unit_cost)", Some("orders"));
        let derived = make_metric("margin", "revenue - cost", None);
        let all = vec![base, cost, derived.clone()];
        assert!(
            !collect_derived_metric_grain(&derived, &all).at_root,
            "a pure composition of base metrics reads no columns of its own"
        );
    }

    /// A WINDOW metric's expression is an aggregate over a metric *reference*,
    /// evaluated on the inner metric's pre-aggregated row set — not a read of
    /// the root table. `metric_grain` accounts for the inner metric's own grain
    /// separately; counting the outer window function here too would put every
    /// window metric at the root grain on top of its real one.
    #[test]
    fn test_collect_grain_window_metric_outer_aggregate_is_not_at_root() {
        let inner = make_metric("item_count", "count(*)", Some("line_items"));
        let mut window = make_metric("rolling_items", "avg(item_count)", None);
        window.window_spec = Some(crate::model::WindowSpec {
            window_function: "AVG".to_string(),
            inner_metric: "item_count".to_string(),
            ..Default::default()
        });
        let all = vec![inner, window.clone()];
        assert!(
            !collect_derived_metric_grain(&window, &all).at_root,
            "the window function is not a root-grain read"
        );
    }
}
