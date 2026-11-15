//! Level-of-detail (LOD) expressions (Phase 8.5): what a view's LOD facts,
//! dimensions and metrics are, and the rules `CREATE` checks them against.
//!
//! Snowflake computes an LOD expression at a grain other than the query's:
//!
//! ```sql
//! FACTS (orders_per_customer AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id))
//! DIMENSIONS (orders_by_region AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.region))
//! METRICS (avg_orders_per_customer AS AVG(orders_per_customer),
//!          inline AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id)))
//! ```
//!
//! A named LOD fact or dimension has no table prefix, and is stored as
//! written, clause and all, with no `source_table`. A metric that reads one,
//! or holds one inline, has no table prefix either (Snowflake: "Entity-level
//! expressions cannot reference LOD expression"). `expand::lod` lowers each
//! into a hidden table; this module only reads them.

use std::collections::HashSet;

use crate::errors::ParseError;
pub(crate) use crate::expr_tokens::LodKind;
use crate::expr_tokens::{
    call_end, call_is_windowed, enclosing_paren, lod_clauses, scan_function_heads, scan_references,
    IdentRef, LodClause,
};
use crate::ident::{ident_matches, normalize_ident_part};
use crate::model::{Dimension, Fact, Metric, SemanticViewDefinition};

/// The body and clause of a named LOD member's expression: the clause ends
/// the expression, outside any bracket.
pub(crate) fn member_lod(expr: &str) -> Option<(&str, LodClause)> {
    let clause = lod_clauses(expr).pop()?;
    if enclosing_paren(expr, clause.start).is_some() || !expr[clause.end..].trim().is_empty() {
        return None;
    }
    Some((expr[..clause.start].trim_end(), clause))
}

/// Whether `f` is an LOD fact: no table prefix and an LOD clause.
pub(crate) fn is_lod_fact(f: &Fact) -> bool {
    f.source_table.is_none() && member_lod(&f.expr).is_some()
}

/// Whether `d` is an LOD dimension: no table prefix and an LOD clause.
pub(crate) fn is_lod_dimension(d: &Dimension) -> bool {
    d.source_table.is_none() && member_lod(&d.expr).is_some()
}

/// The normalized names of the view's LOD facts and dimensions.
pub(crate) fn lod_member_keys(def: &SemanticViewDefinition) -> HashSet<String> {
    def.facts
        .iter()
        .filter(|f| is_lod_fact(f))
        .map(|f| normalize_ident_part(&f.name))
        .chain(
            def.dimensions
                .iter()
                .filter(|d| is_lod_dimension(d))
                .map(|d| normalize_ident_part(&d.name)),
        )
        .collect()
}

/// Whether metric `m` is an LOD metric: no table prefix, and an inline LOD
/// expression or a bare reference to an LOD fact or dimension.
pub(crate) fn is_lod_metric(m: &Metric, lod_keys: &HashSet<String>) -> bool {
    m.source_table.is_none()
        && (!lod_clauses(&m.expr).is_empty()
            || scan_references(&m.expr)
                .iter()
                .any(|r| r.is_bare() && lod_keys.contains(&r.key())))
}

/// Whether the view holds an LOD expression anywhere.
pub(crate) fn has_lod(def: &SemanticViewDefinition) -> bool {
    let keys = lod_member_keys(def);
    !keys.is_empty() || def.metrics.iter().any(|m| is_lod_metric(m, &keys))
}

/// The names `text` reads: its references, less literals, keywords
/// (`DISTINCT`, `NULL`) and a lambda's parameters, as the bare-column rule
/// reads them (`expand::bare_columns`).
pub(crate) fn names(text: &str) -> Vec<IdentRef<'_>> {
    let lambdas = crate::query::expr_items::lambda_scopes(text);
    scan_references(text)
        .into_iter()
        .filter(|r| {
            !crate::query::expr_items::is_not_a_column(text, r)
                && !crate::query::expr_items::is_local_name(text, r, &lambdas)
        })
        .collect()
}

/// What a name in an LOD expression or a metric refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A fact (index into `def.facts`) that is no LOD.
    Fact(usize),
    /// A dimension (index into `def.dimensions`) that is no LOD.
    Dimension(usize),
    /// An LOD fact or dimension, named bare.
    Lod,
    /// A metric, named bare.
    Metric,
    /// A view variable.
    Variable,
    /// Anything else: a column, or a name the view does not declare.
    Other,
}

/// What `chain` names in `def`: `table.member` for a fact or dimension of
/// that table, or a bare name of a member or variable. Names match as `DuckDB`
/// matches identifiers.
pub(crate) fn resolve(def: &SemanticViewDefinition, chain: &IdentRef<'_>) -> Resolved {
    let ends = chain.part_ends();
    let key_of = |text: &str| normalize_ident_part(text);
    if ends.len() == 1 {
        let key = chain.key();
        let named = |name: &str| key_of(name) == key;
        if let Some(i) = def.facts.iter().position(|f| named(&f.name)) {
            return if is_lod_fact(&def.facts[i]) {
                Resolved::Lod
            } else {
                Resolved::Fact(i)
            };
        }
        if let Some(i) = def.dimensions.iter().position(|d| named(&d.name)) {
            return if is_lod_dimension(&def.dimensions[i]) {
                Resolved::Lod
            } else {
                Resolved::Dimension(i)
            };
        }
        if def.metrics.iter().any(|m| named(&m.name)) {
            return Resolved::Metric;
        }
        if def.variables.iter().any(|v| named(&v.name)) {
            return Resolved::Variable;
        }
        return Resolved::Other;
    }
    if ends.len() != 2 {
        return Resolved::Other;
    }
    let table = &chain.raw[..ends[0]];
    let name = key_of(&chain.raw[ends[0] + 1..]);
    let on =
        |src: Option<&str>| src.is_some_and(|s| ident_matches(s, table) || ident_matches(table, s));
    if let Some(i) = def
        .facts
        .iter()
        .position(|f| on(f.source_table.as_deref()) && key_of(&f.name) == name)
    {
        return Resolved::Fact(i);
    }
    if let Some(i) = def
        .dimensions
        .iter()
        .position(|d| on(d.source_table.as_deref()) && key_of(&d.name) == name)
    {
        return Resolved::Dimension(i);
    }
    Resolved::Other
}

/// The table of a resolved fact or dimension, as stored.
pub(crate) fn member_table(def: &SemanticViewDefinition, r: Resolved) -> Option<&str> {
    match r {
        Resolved::Fact(i) => def.facts[i].source_table.as_deref(),
        Resolved::Dimension(i) => def.dimensions[i].source_table.as_deref(),
        _ => None,
    }
}

/// The aggregate calls of `expr` that no other aggregate call holds, each as
/// the span of the call (head to closing bracket) and the offset of its `(`.
/// A windowed aggregate is no aggregate here.
pub(crate) fn outer_aggregates(expr: &str) -> Vec<(usize, usize, usize)> {
    let mut out: Vec<(usize, usize, usize)> = Vec::new();
    for head in scan_function_heads(expr) {
        if crate::graph::aggregate_name(&head.last_part_key()).is_none()
            || call_is_windowed(expr, &head)
        {
            continue;
        }
        let Some(end) = call_end(expr, &head) else {
            continue;
        };
        if out
            .iter()
            .any(|&(s, e, _)| s <= head.start && head.start < e)
        {
            continue;
        }
        let open = head.end + expr[head.end..].find('(').unwrap_or(0);
        out.push((head.start, end, open));
    }
    out
}

/// One aggregate call of an LOD metric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetricPart {
    /// The call's span in the metric's expression.
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// The LOD expression it aggregates, if any.
    pub(crate) lod: Option<PartLod>,
}

/// The LOD expression an aggregate call of an LOD metric reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PartLod {
    /// Written inline: the span of its body and clause in the metric's
    /// expression, and its clause.
    Inline {
        start: usize,
        end: usize,
        clause: LodClause,
    },
    /// An LOD fact or dimension, named bare: the name's span and its key.
    Named {
        start: usize,
        end: usize,
        key: String,
    },
}

/// The aggregate calls of LOD metric `m`, or a message saying which rule it
/// breaks: an LOD expression is read only inside an aggregate call, one per
/// call, with nothing else the view declares beside it there; an inline one
/// is the whole argument of its call.
#[allow(clippy::too_many_lines)]
pub(crate) fn metric_parts(
    def: &SemanticViewDefinition,
    m: &Metric,
) -> Result<Vec<MetricPart>, String> {
    let expr = m.expr.as_str();
    let calls = outer_aggregates(expr);
    let clauses = lod_clauses(expr);
    let refs = names(expr);
    let name = &m.name;
    let inside = |pos: usize| calls.iter().position(|&(s, e, _)| s <= pos && pos < e);
    let mut parts: Vec<MetricPart> = calls
        .iter()
        .map(|&(start, end, _)| MetricPart {
            start,
            end,
            lod: None,
        })
        .collect();
    let mut taken: Vec<Option<(usize, usize)>> = vec![None; calls.len()];
    for clause in clauses {
        let Some(c) = inside(clause.start) else {
            return Err(format!(
                "metric '{name}' holds an LOD expression outside an aggregate call. An LOD \
                 expression is always aggregated: write it as the argument of an aggregate \
                 function, such as AVG(<expression> {} DIMENSIONS (...)).",
                clause.kind.keyword()
            ));
        };
        let (_, _, open) = calls[c];
        // The argument list's own `)`: a `FILTER (...)` may follow it.
        let args_close = crate::expr_tokens::closing_paren(expr, open);
        if enclosing_paren(expr, clause.start) != Some(open)
            || expr[clause.end..args_close.saturating_sub(1)].trim() != ""
        {
            return Err(format!(
                "metric '{name}': an inline LOD expression must be the whole argument of its \
                 aggregate function, as in AVG(COUNT(orders.order_id) FIXED DIMENSIONS \
                 (customers.customer_id))."
            ));
        }
        if parts[c].lod.is_some() {
            return Err(one_lod_per_aggregate(name));
        }
        taken[c] = Some((open + 1, clause.end));
        parts[c].lod = Some(PartLod::Inline {
            start: open + 1,
            end: clause.end,
            clause,
        });
    }
    let in_inline = |c: usize, pos: usize| taken[c].is_some_and(|(s, e)| s <= pos && pos < e);
    for r in &refs {
        let resolved = resolve(def, r);
        let call = inside(r.start);
        if call.is_some_and(|c| in_inline(c, r.start)) {
            // The inline expression's own body, which `resolve_lod` checks.
            continue;
        }
        match (resolved, call) {
            (Resolved::Lod, None) => {
                return Err(format!(
                    "metric '{name}' reads LOD expression '{}' outside an aggregate call. An \
                     LOD expression is always aggregated: write AVG({}), SUM({}) or another \
                     aggregate of it.",
                    r.raw, r.raw, r.raw
                ));
            }
            (Resolved::Lod, Some(c)) => {
                // The same LOD expression read again in its call, as in
                // `AVG(CASE WHEN opc > 1 THEN opc END)`, is one LOD expression.
                if matches!(&parts[c].lod, Some(PartLod::Named { key, .. }) if *key == r.key()) {
                    continue;
                }
                if parts[c].lod.is_some() {
                    return Err(one_lod_per_aggregate(name));
                }
                parts[c].lod = Some(PartLod::Named {
                    start: r.start,
                    end: r.end,
                    key: r.key(),
                });
            }
            _ => {}
        }
    }
    // Nothing the view declares beside the LOD expression in its call.
    for (c, part) in parts.iter().enumerate() {
        if part.lod.is_none() {
            continue;
        }
        for r in refs
            .iter()
            .filter(|r| part.start <= r.start && r.start < part.end && !in_inline(c, r.start))
        {
            match resolve(def, r) {
                Resolved::Lod | Resolved::Variable => {}
                _ => {
                    return Err(format!(
                        "metric '{name}' reads '{}' in the aggregate call that reads an LOD \
                         expression. That call can aggregate only the LOD expression, \
                         with constants and variables.",
                        r.raw
                    ));
                }
            }
        }
        let nested = scan_function_heads(&expr[part.start..part.end])
            .iter()
            .skip(1)
            .filter(|h| !in_inline(c, part.start + h.start))
            .any(|h| crate::graph::aggregate_name(&h.last_part_key()).is_some());
        if nested {
            return Err(format!(
                "metric '{name}' nests an aggregate call around an LOD expression's aggregate. \
                 Aggregate the LOD expression once."
            ));
        }
    }
    Ok(parts)
}

fn one_lod_per_aggregate(metric: &str) -> String {
    format!(
        "metric '{metric}' reads two LOD expressions in one aggregate call. An aggregate \
         function can read only one LOD expression; aggregate each on its own."
    )
}

/// Where an LOD expression is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    /// LOD fact `def.facts[i]`.
    Fact(usize),
    /// LOD dimension `def.dimensions[i]`.
    Dimension(usize),
    /// Inline in metric `def.metrics[metric]`, its aggregate call `part`.
    Metric { metric: usize, part: usize },
}

/// An LOD expression of the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lod {
    pub(crate) origin: Origin,
    pub(crate) kind: LodKind,
    /// The aggregate it computes, as written.
    pub(crate) body: String,
    /// Its `DIMENSIONS (...)` entries, as written.
    pub(crate) entries: Vec<String>,
}

impl Lod {
    /// A name for messages.
    pub(crate) fn describe(&self, def: &SemanticViewDefinition) -> String {
        match self.origin {
            Origin::Fact(i) => format!("LOD fact '{}'", def.facts[i].name),
            Origin::Dimension(i) => format!("LOD dimension '{}'", def.dimensions[i].name),
            Origin::Metric { metric, .. } => {
                format!(
                    "the LOD expression in metric '{}'",
                    def.metrics[metric].name
                )
            }
        }
    }
}

/// An LOD metric's index into `def.metrics` and its aggregate calls.
pub(crate) type LodMetric = (usize, Vec<MetricPart>);

/// Every LOD expression of the view, named ones first, with the aggregate
/// calls of each LOD metric by metric index.
///
/// # Errors
///
/// A message when an LOD metric breaks [`metric_parts`]'s rules.
pub(crate) fn collect_lods(
    def: &SemanticViewDefinition,
) -> Result<(Vec<Lod>, Vec<LodMetric>), String> {
    let mut lods = Vec::new();
    for (i, f) in def.facts.iter().enumerate() {
        if let (None, Some((body, clause))) = (&f.source_table, member_lod(&f.expr)) {
            lods.push(Lod {
                origin: Origin::Fact(i),
                kind: clause.kind,
                body: body.to_string(),
                entries: clause.entries,
            });
        }
    }
    for (i, d) in def.dimensions.iter().enumerate() {
        if let (None, Some((body, clause))) = (&d.source_table, member_lod(&d.expr)) {
            lods.push(Lod {
                origin: Origin::Dimension(i),
                kind: clause.kind,
                body: body.to_string(),
                entries: clause.entries,
            });
        }
    }
    let keys = lod_member_keys(def);
    let mut metrics = Vec::new();
    for (mi, m) in def.metrics.iter().enumerate() {
        if !is_lod_metric(m, &keys) {
            continue;
        }
        let parts = metric_parts(def, m)?;
        for (p, part) in parts.iter().enumerate() {
            if let Some(PartLod::Inline { start, clause, .. }) = &part.lod {
                lods.push(Lod {
                    origin: Origin::Metric {
                        metric: mi,
                        part: p,
                    },
                    kind: clause.kind,
                    body: m.expr[*start..clause.start].trim().to_string(),
                    entries: clause.entries.clone(),
                });
            }
        }
        metrics.push((mi, parts));
    }
    Ok((lods, metrics))
}

/// The members an LOD expression's body reads and its entries name, and its
/// home: the table whose rows it aggregates, from which every table it reads
/// is reached along foreign keys by one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolution {
    pub(crate) home: String,
    /// The members its `DIMENSIONS (...)` entries name, in order.
    pub(crate) keys: Vec<Resolved>,
    /// The tables it reads other than its home: the body's and the entries'.
    pub(crate) tables: Vec<String>,
}

/// Resolve `lod` (see [`Resolution`]).
///
/// # Errors
///
/// A message naming the rule it breaks: the body is one aggregate over facts
/// and dimensions that are no LOD expression; each entry names one such fact
/// or dimension; every table read is reached from one of them along foreign
/// keys by one path.
#[allow(clippy::too_many_lines)]
pub(crate) fn resolve_lod(def: &SemanticViewDefinition, lod: &Lod) -> Result<Resolution, String> {
    let what = lod.describe(def);
    if lod.kind != LodKind::Fixed {
        return Err(format!(
            "{what} uses {} DIMENSIONS, which this version does not support yet; only FIXED \
             DIMENSIONS is.",
            lod.kind.keyword()
        ));
    }
    let body = lod.body.as_str();
    if !lod_clauses(body).is_empty() {
        return Err(nested_lod(&what));
    }
    if crate::expr_tokens::has_window_call(body) || outer_aggregates(body).is_empty() {
        return Err(single_aggregate(&what));
    }
    let calls = outer_aggregates(body);
    let add = |tables: &mut Vec<String>, t: &str| {
        if !tables.iter().any(|x| ident_matches(x, t)) {
            tables.push(t.to_string());
        }
    };
    let mut body_tables: Vec<String> = Vec::new();
    for r in names(body) {
        let resolved = resolve(def, &r);
        match resolved {
            Resolved::Lod => return Err(nested_lod(&what)),
            Resolved::Fact(_) | Resolved::Dimension(_) => {
                if !calls.iter().any(|&(s, e, _)| s <= r.start && r.start < e) {
                    return Err(single_aggregate(&what));
                }
                if let Some(t) = member_table(def, resolved) {
                    add(&mut body_tables, t);
                }
            }
            Resolved::Variable => {}
            Resolved::Metric | Resolved::Other => {
                return Err(format!(
                    "{what} reads '{}', which is no fact or dimension of the view. An LOD \
                     expression is one aggregate over facts and dimensions, named as \
                     <table>.<name>.",
                    r.raw
                ));
            }
        }
    }
    if body_tables.is_empty() {
        return Err(single_aggregate(&what));
    }
    let mut keys = Vec::new();
    let mut key_tables: Vec<String> = Vec::new();
    for entry in &lod.entries {
        if entry.is_empty() {
            return Err(format!(
                "{what} has an empty entry in {} DIMENSIONS: a stray ','. Remove the extra comma.",
                lod.kind.keyword()
            ));
        }
        let refs = scan_references(entry);
        let whole = refs.len() == 1
            && refs[0].raw == entry.as_str()
            && scan_function_heads(entry).is_empty();
        if !whole {
            return Err(format!(
                "{what} lists '{entry}' in {} DIMENSIONS. Only a dimension or fact the view \
                 defines can be listed there, not an expression; define the expression as a \
                 dimension first.",
                lod.kind.keyword()
            ));
        }
        let resolved = resolve(def, &refs[0]);
        match resolved {
            Resolved::Fact(_) | Resolved::Dimension(_) => {
                if let Some(t) = member_table(def, resolved) {
                    add(&mut key_tables, t);
                }
                keys.push(resolved);
            }
            Resolved::Lod => {
                return Err(format!(
                    "{what} lists LOD expression '{entry}' in {} DIMENSIONS. Only a dimension \
                     or fact that is no LOD expression can be listed there.",
                    lod.kind.keyword()
                ));
            }
            _ => {
                return Err(format!(
                    "{what} lists '{entry}' in {} DIMENSIONS, which is no dimension or fact of \
                     the view.",
                    lod.kind.keyword()
                ));
            }
        }
    }
    // The home: the first table of the body's from which every other table
    // is reached by one path.
    let mut all = body_tables.clone();
    for t in &key_tables {
        add(&mut all, t);
    }
    let reaches = |from: &str, to: &str| {
        ident_matches(from, to) || crate::graph::chain_paths(def, from, to, &|_| true).len() == 1
    };
    let home = body_tables
        .iter()
        .find(|h| all.iter().all(|t| reaches(h, t)))
        .cloned();
    let Some(home) = home else {
        let first = &body_tables[0];
        let other = all
            .iter()
            .find(|t| !reaches(first, t))
            .cloned()
            .unwrap_or_default();
        let two = crate::graph::chain_paths(def, first, &other, &|_| true).len() > 1;
        return Err(if two {
            format!(
                "{what} reads table '{other}', which table '{first}' reaches by more than one \
                 path, so which one is meant is not said."
            )
        } else {
            format!(
                "{what} reads tables '{first}' and '{other}', which are not related along \
                 foreign keys: the table it aggregates must reference every table its \
                 expression and its DIMENSIONS read, directly or through other tables."
            )
        });
    };
    let tables = all
        .into_iter()
        .filter(|t| !ident_matches(t, &home))
        .collect();
    Ok(Resolution { home, keys, tables })
}

fn nested_lod(what: &str) -> String {
    format!("{what} reads another LOD expression. An LOD expression cannot read another one.")
}

fn single_aggregate(what: &str) -> String {
    format!(
        "{what} must be a single aggregate over facts or dimensions, such as \
         COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id)."
    )
}

/// Refuse at CREATE an LOD expression that breaks Snowflake's rules (see the
/// module docs): a table prefix on an LOD fact or dimension, an LOD
/// expression read from a member with a table prefix, a broken LOD metric or
/// LOD expression ([`metric_parts`], [`resolve_lod`]), and an LOD member in a
/// materialization.
///
/// # Errors
///
/// A [`ParseError`] naming the member and the rule.
pub fn validate_lod(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    let err = ParseError::positionless;
    // An unprefixed fact or dimension whose LOD clause sits inside a larger
    // expression: the parser took it as an LOD member, but it is none.
    let unprefixed = def
        .facts
        .iter()
        .filter(|f| f.source_table.is_none())
        .map(|f| ("fact", &f.name, &f.expr))
        .chain(
            def.dimensions
                .iter()
                .filter(|d| d.source_table.is_none())
                .map(|d| ("dimension", &d.name, &d.expr)),
        );
    for (kind, name, expr) in unprefixed {
        if member_lod(expr).is_none() && !lod_clauses(expr).is_empty() {
            return Err(err(format!(
                "{kind} '{name}' holds an LOD expression inside a larger expression. An LOD \
                 {kind} is one aggregate followed by its DIMENSIONS clause, as in \
                 COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id); compute \
                 the rest in a metric."
            )));
        }
    }
    // A table prefix on an LOD fact or dimension.
    let prefixed = def
        .facts
        .iter()
        .filter(|f| f.source_table.is_some())
        .map(|f| ("fact", &f.name, &f.expr))
        .chain(
            def.dimensions
                .iter()
                .filter(|d| d.source_table.is_some())
                .map(|d| ("dimension", &d.name, &d.expr)),
        );
    let keys = lod_member_keys(def);
    for (kind, name, expr) in prefixed {
        if member_lod(expr).is_some() {
            return Err(err(format!(
                "{kind} '{name}' is an LOD expression with a table prefix. An LOD {kind} is \
                 declared without one: '{name} AS <aggregate> FIXED DIMENSIONS (...)'."
            )));
        }
        entity_level_check(kind, name, expr, &keys)?;
    }
    for m in def.metrics.iter().filter(|m| m.source_table.is_some()) {
        entity_level_check("metric", &m.name, &m.expr, &keys)?;
    }
    if !has_lod(def) {
        return Ok(());
    }
    let (lods, metrics) = collect_lods(def).map_err(err)?;
    for lod in &lods {
        resolve_lod(def, lod).map_err(err)?;
    }
    for (mi, parts) in &metrics {
        let m = &def.metrics[*mi];
        if m.window_spec.is_some() || !m.non_additive_by.is_empty() {
            return Err(err(format!(
                "metric '{}' reads an LOD expression and is a window or semi-additive metric, \
                 which an LOD metric cannot be.",
                m.name
            )));
        }
        for part in parts.iter().filter(|p| p.lod.is_none()) {
            let text = &m.expr[part.start..part.end];
            let table = names(text)
                .iter()
                .find_map(|r| member_table(def, resolve(def, r)).map(str::to_string));
            if table.is_none() {
                return Err(err(format!(
                    "metric '{}' aggregates '{text}', which reads no table's fact or \
                     dimension. In a metric without a table prefix, each aggregate call reads \
                     an LOD expression or a table's facts and dimensions.",
                    m.name
                )));
            }
        }
    }
    let lod_metric_keys: HashSet<String> = metrics
        .iter()
        .map(|(mi, _)| normalize_ident_part(&def.metrics[*mi].name))
        .collect();
    for mat in &def.materializations {
        if let Some(name) = mat.dimensions.iter().chain(&mat.metrics).find(|n| {
            let key = normalize_ident_part(n);
            keys.contains(&key) || lod_metric_keys.contains(&key)
        }) {
            return Err(err(format!(
                "materialization '{}' covers '{name}', an LOD expression. LOD expressions are \
                 not supported in materializations.",
                mat.name
            )));
        }
    }
    Ok(())
}

/// Refuse a member with a table prefix that reads an LOD expression
/// (Snowflake: "Entity-level expressions cannot reference LOD expression").
fn entity_level_check(
    kind: &str,
    name: &str,
    expr: &str,
    lod_keys: &HashSet<String>,
) -> Result<(), ParseError> {
    if !lod_clauses(expr).is_empty() {
        return Err(ParseError::positionless(format!(
            "{kind} '{name}' has a table prefix and holds an inline LOD expression. A {kind} \
             with a table prefix cannot hold one: define the LOD expression as a fact or \
             dimension, or declare the {kind} without a table prefix."
        )));
    }
    if let Some(r) = scan_references(expr)
        .into_iter()
        .find(|r| r.is_bare() && lod_keys.contains(&r.key()))
    {
        return Err(ParseError::positionless(format!(
            "{kind} '{name}' has a table prefix and reads LOD expression '{}'. Entity-level \
             expressions cannot reference an LOD expression: declare the {kind} without a \
             table prefix.",
            r.raw
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented view's FIXED members, parsed from DDL.
    fn parsed(facts: &str, dims: &str, metrics: &str) -> SemanticViewDefinition {
        let body = format!(
            "TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders) \
             RELATIONSHIPS (orders(customer_id) REFERENCES customers) \
             FACTS (orders.order_amount AS amount{facts}) \
             DIMENSIONS (customers.customer_id AS customer_id, customers.region AS region, \
             orders.order_id AS order_id, orders.category AS category{dims}) \
             METRICS (orders.total AS SUM(orders.order_amount){metrics})"
        );
        let kb = crate::body_parser::parse_keyword_body(&body, 0).unwrap();
        let mut def = SemanticViewDefinition {
            tables: kb.tables,
            joins: kb.relationships,
            facts: kb.facts,
            dimensions: kb.dimensions,
            metrics: kb.metrics,
            materializations: kb.materializations,
            ..Default::default()
        };
        crate::graph::infer_cardinality(&def.tables, &mut def.joins).unwrap();
        def
    }

    fn refusal(facts: &str, dims: &str, metrics: &str) -> String {
        validate_lod(&parsed(facts, dims, metrics))
            .err()
            .map_or_else(|| "<accepted>".to_string(), |e| e.message)
    }

    /// An LOD fact or dimension parses without a table prefix, stored as
    /// written, and is read back as its body and clause.
    #[test]
    fn lod_members_parse_without_a_table_prefix() {
        let def = parsed(
            ", opc AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id) COMMENT = 'c'",
            ", obr AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.region)",
            ", avg_opc AS AVG(opc)",
        );
        let opc = &def.facts[1];
        assert_eq!(opc.source_table, None);
        assert_eq!(opc.comment.as_deref(), Some("c"));
        let (body, clause) = member_lod(&opc.expr).unwrap();
        assert_eq!(body, "COUNT(orders.order_id)");
        assert_eq!(clause.entries, vec!["customers.customer_id"]);
        assert!(is_lod_dimension(&def.dimensions[4]));
        assert!(is_lod_metric(&def.metrics[1], &lod_member_keys(&def)));
        assert!(!is_lod_metric(&def.metrics[0], &lod_member_keys(&def)));
        assert!(has_lod(&def));
        validate_lod(&def).unwrap();
        assert_eq!(def.required_schema_version(), 4);
    }

    /// A member without an LOD clause still needs its table prefix.
    #[test]
    fn a_plain_member_without_a_prefix_is_refused_at_parse() {
        let body = "TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (d AS o.region)";
        let err = crate::body_parser::parse_keyword_body(body, 0).unwrap_err();
        assert!(
            err.message.contains("Expected 'alias.name'"),
            "{}",
            err.message
        );
    }

    #[test]
    fn resolve_lod_finds_the_home_and_the_keys() {
        let def = parsed(
            ", opc AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id, category)",
            "",
            "",
        );
        let (lods, _) = collect_lods(&def).unwrap();
        let res = resolve_lod(&def, &lods[0]).unwrap();
        assert_eq!(res.home, "orders");
        assert_eq!(res.tables, vec!["customers"]);
        assert_eq!(
            res.keys,
            vec![Resolved::Dimension(0), Resolved::Dimension(3)]
        );
    }

    /// Each rule Snowflake applies, with the rule's wording.
    #[test]
    fn broken_lod_expressions_are_refused() {
        let opc = ", opc AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id)";
        let cases: Vec<(String, &str)> = vec![
            (refusal(opc, "", ", orders.x AS AVG(opc)"), "has a table prefix and reads LOD expression 'opc'"),
            (
                refusal(", orders.opc AS COUNT(orders.order_id) FIXED DIMENSIONS ()", "", ""),
                "is an LOD expression with a table prefix",
            ),
            (
                refusal(&format!("{opc}, o2 AS SUM(opc) FIXED DIMENSIONS ()"), "", ""),
                "reads another LOD expression",
            ),
            (
                refusal(", r AS SUM(orders.amount) FIXED DIMENSIONS ()", "", ""),
                "reads 'orders.amount', which is no fact or dimension",
            ),
            (
                refusal(", r AS orders.order_amount FIXED DIMENSIONS ()", "", ""),
                "must be a single aggregate",
            ),
            (
                refusal(", r AS SUM(orders.order_amount) FIXED DIMENSIONS (UPPER(region))", "", ""),
                "not an expression",
            ),
            (
                refusal(", r AS SUM(orders.order_amount) FIXED DIMENSIONS (nope)", "", ""),
                "which is no dimension or fact of the view",
            ),
            (
                refusal(&format!("{opc}, rpc AS SUM(orders.order_amount) FIXED DIMENSIONS ()"), "", ", x AS AVG(opc / rpc)"),
                "reads two LOD expressions in one aggregate call",
            ),
            (refusal(opc, "", ", x AS opc + 1"), "outside an aggregate call"),
            (
                refusal(opc, "", ", x AS AVG(opc * orders.order_amount)"),
                "That call can aggregate only the LOD expression",
            ),
            (refusal(opc, "", ", x AS SUM(AVG(opc))"), "nests an aggregate call"),
            (
                refusal("", "", ", x AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS ()) FIXED DIMENSIONS ()"),
                "outside an aggregate call",
            ),
            (
                refusal("", "", ", x AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS () + 1)"),
                "must be the whole argument of its aggregate function",
            ),
            (refusal(opc, "", ", x AS AVG(opc) + COUNT(*)"), "reads no table's fact or dimension"),
            (
                refusal(
                    ", c AS COUNT(customers.customer_id) FIXED DIMENSIONS (orders.category)",
                    "",
                    "",
                ),
                "which are not related along foreign keys",
            ),
            (
                refusal(", n AS COUNT(orders.order_id) EXCLUDE DIMENSIONS (category)", "", ""),
                "uses EXCLUDE DIMENSIONS, which this version does not support yet",
            ),
            (
                refusal(
                    "",
                    ", d2 AS COUNT(orders.order_id) FIXED DIMENSIONS (d3), d3 AS COUNT(orders.order_id) FIXED DIMENSIONS ()",
                    "",
                ),
                "lists LOD expression 'd3' in FIXED DIMENSIONS",
            ),
            (
                refusal(", x AS SUM(orders.order_amount) + (COUNT(orders.order_id) FIXED DIMENSIONS ())", "", ""),
                "fact 'x' holds an LOD expression inside a larger expression",
            ),
            (
                refusal("", ", x AS (COUNT(orders.order_id) FIXED DIMENSIONS ())", ""),
                "dimension 'x' holds an LOD expression inside a larger expression",
            ),
            (
                refusal(", r AS SUM(orders.order_amount) FIXED DIMENSIONS (customers.region,,orders.category)", "", ""),
                "has an empty entry in FIXED DIMENSIONS",
            ),
            (
                refusal(", r AS SUM(orders.order_amount) FIXED DIMENSIONS (,customers.region)", "", ""),
                "has an empty entry in FIXED DIMENSIONS",
            ),
        ];
        // Each case reports on its own, so one red doesn't hide another.
        let wrong: Vec<String> = cases
            .iter()
            .filter(|(got, want)| !got.contains(want))
            .map(|(got, want)| format!("{want:?} not in {got:?}"))
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Constants, keywords and variables sit beside an LOD expression in its
    /// aggregate call.
    #[test]
    fn constants_and_keywords_are_no_members() {
        let def = parsed(
            ", opc AS COUNT(DISTINCT orders.order_id) FIXED DIMENSIONS (customers.customer_id)",
            "",
            ", x AS AVG(CASE WHEN opc > 1 THEN opc + 1.5 ELSE NULL END)",
        );
        validate_lod(&def).unwrap();
    }

    /// An inline LOD expression's call can carry a `FILTER`, as a named one's
    /// can (review of #368: it was refused as not the whole argument).
    #[test]
    fn an_inline_lod_call_can_carry_a_filter() {
        let def = parsed(
            "",
            "",
            ", x AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id)) FILTER (WHERE true)",
        );
        validate_lod(&def).unwrap();
        let lowered = crate::expand::lower_lod(&def);
        let hidden = lowered
            .metrics
            .iter()
            .find(|m| m.name.starts_with("__sv_lodm"))
            .unwrap();
        assert!(
            hidden.expr.ends_with("FILTER (WHERE true)"),
            "{}",
            hidden.expr
        );
    }

    #[test]
    fn an_lod_member_in_a_materialization_is_refused() {
        let mut def = parsed(
            "",
            ", obr AS COUNT(orders.order_id) FIXED DIMENSIONS (customers.region)",
            "",
        );
        def.materializations.push(crate::model::Materialization {
            name: "m".into(),
            table: "t".into(),
            dimensions: vec!["OBR".into()],
            ..Default::default()
        });
        let err = validate_lod(&def).unwrap_err().message;
        assert!(err.contains("not supported in materializations"), "{err}");
    }
}
