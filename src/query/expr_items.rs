//! Expression items (TECH-DEBT #76): a query item that is an expression over
//! members rather than a member's name, as Snowflake's `SEMANTIC_VIEW`
//! construct allows:
//!
//! ```sql
//! SEMANTIC_VIEW(v DIMENSIONS DATE_PART('year', o.order_date) AS year
//!                 METRICS o.revenue / o.order_count AS avg_order, SUM(o.amount) AS total)
//! ```
//!
//! Each expression item becomes a member of the query's own copy of the
//! definition, under a name no declared member has, and the item then names
//! that member, with its alias (or the expression as written) as the column's
//! name. So the expansion plans an expression item exactly as it plans a
//! declared member: every emission path, and every check a declared member
//! goes through (the fan-trap checks, role-playing, the joins a member's table
//! needs, materialization routing), sees an ordinary member.
//!
//! The rules are Snowflake's:
//!
//! - A `DIMENSIONS` or `FACTS` item is a row-level expression over dimensions
//!   and facts that all belong to one logical table. It becomes a dimension
//!   (or fact) on that table. It holds no aggregate and no metric.
//! - A `METRICS` item is an expression over metrics and over aggregates of
//!   dimensions and facts. Each aggregate, whose dimensions and facts must
//!   belong to one table, becomes a metric on that table; the expression
//!   around them becomes a derived metric over those metrics and the ones it
//!   names. An item that is one aggregate is that metric.
//!
//! No item holds a subquery, a window function or a prepared statement
//! parameter, and a private fact or metric cannot be named, as in a query
//! anywhere else.

use std::collections::{HashMap, HashSet};

use crate::expand::{quote_ident, ExpandError};
use crate::expr_tokens::{
    call_end, call_is_windowed, find_parameters, has_statement_separator, inline_references,
    scan_function_heads, scan_references, subquery_spans, IdentRef,
};
use crate::ident::normalize_ident_part;
use crate::model::{AccessModifier, Dimension, Fact, Metric, SemanticViewDefinition};

use super::items::{Group, Item, RequestItems};

/// A request with its expression items lowered to members.
#[derive(Debug)]
pub struct LoweredItems {
    /// The definition with a member added for each expression item, or `None`
    /// when the request has none (the stored definition is used as it is).
    pub def: Option<SemanticViewDefinition>,
    /// The items, each expression item now naming its member and carrying its
    /// column's name as its alias.
    pub items: RequestItems,
    /// Each added member's name and the item text it stands for, for display.
    pub labels: Vec<(String, String)>,
}

impl LoweredItems {
    /// `message` with each added member's name, quoted as the expansion's
    /// errors quote a member (`'__sv_expr_1'`), replaced by the item it
    /// stands for, so an error about an expression item names what was
    /// written.
    #[must_use]
    pub fn relabel(&self, message: &str) -> String {
        let mut out = message.to_string();
        for (name, text) in &self.labels {
            out = out.replace(&format!("'{name}'"), &format!("'{text}'"));
        }
        out
    }

    /// An error from expanding the lowered request, about the request as
    /// written: an added member is not offered as an available name or a
    /// suggestion (it is one of the request's own items), and is named by its
    /// item where the error lists the query's metrics. Where the message
    /// quotes one, [`relabel`](Self::relabel) names it.
    #[must_use]
    pub fn for_request(&self, mut e: ExpandError) -> ExpandError {
        let item_of = |name: &str| {
            self.labels
                .iter()
                .find(|(added, _)| added == name)
                .map(|(_, text)| text)
        };
        match &mut e {
            ExpandError::UnknownDimension {
                available,
                suggestion,
                ..
            }
            | ExpandError::UnknownMetric {
                available,
                suggestion,
                ..
            }
            | ExpandError::UnknownFact {
                available,
                suggestion,
                ..
            } => {
                available.retain(|n| item_of(n).is_none());
                if suggestion.as_deref().and_then(item_of).is_some() {
                    *suggestion = None;
                }
            }
            ExpandError::WindowAggregateMixing {
                aggregate_metrics, ..
            } => {
                for n in aggregate_metrics.iter_mut() {
                    if let Some(text) = item_of(n) {
                        n.clone_from(text);
                    }
                }
            }
            _ => {}
        }
        e
    }
}

/// Why an item was refused: a message for the request, or an expansion error
/// reported as the expansion reports it (boxed, so the `Result`s that carry it
/// stay under clippy's `result_large_err` threshold).
#[derive(Debug)]
pub enum ItemError {
    Invalid(String),
    Expand(Box<ExpandError>),
}

/// Lower the expression items of `items` against `def`.
///
/// An item is an expression when it is not a wildcard, not one dotted name, and
/// not the name of a declared member (a name with a space can be written
/// unquoted, `Total Revenue`). Names, wildcards and unknown names are left for
/// resolution as before.
///
/// # Errors
///
/// [`ItemError::Invalid`] for an expression item that breaks the rules above,
/// and [`ItemError::Expand`] for one that names a private fact or metric.
pub fn lower_expression_items(
    view_name: &str,
    def: &SemanticViewDefinition,
    mut items: RequestItems,
) -> Result<LoweredItems, ItemError> {
    let index = MemberIndex::new(def);
    let mut lowering = Lowering {
        view_name,
        def,
        index: &index,
        added: Added::default(),
        taken: index.names.clone(),
        labels: Vec::new(),
    };
    for (group, list) in [
        (Group::Dimensions, &mut items.dimensions),
        (Group::Metrics, &mut items.metrics),
        (Group::Facts, &mut items.facts),
    ] {
        for item in list.iter_mut() {
            if !is_expression(&index, &item.member) {
                continue;
            }
            let text = crate::util::blank_sql_comments(&item.member)
                .trim()
                .to_string();
            let name = lowering.lower(group, &text)?;
            lowering
                .labels
                .push((name.clone(), item.member.trim().to_string()));
            *item = Item {
                member: quote_ident(&name),
                alias: Some(item.alias.take().unwrap_or(text)),
            };
        }
    }
    let Lowering { added, labels, .. } = lowering;
    let def = (!labels.is_empty()).then(|| {
        let mut def = def.clone();
        def.dimensions.extend(added.dimensions);
        def.facts.extend(added.facts);
        def.metrics.extend(added.metrics);
        def
    });
    Ok(LoweredItems { def, items, labels })
}

/// Whether `member` is an expression rather than a name or a wildcard.
fn is_expression(index: &MemberIndex<'_>, member: &str) -> bool {
    let text = member.trim();
    if text.ends_with('*') {
        return false;
    }
    let refs = scan_references(text);
    // One name is an item, unless it is a struct field of a member
    // (`o.addr.city`), which is read from the member as in an expression.
    if let [r] = refs.as_slice() {
        if r.start == 0 && r.end == text.len() {
            return index.lookup(r).is_empty() && index.field_path(r).is_some();
        }
    }
    !index.by_key.contains_key(&normalize_ident_part(text))
}

/// A declared member.
#[derive(Debug, Clone, Copy)]
enum Member<'a> {
    Dimension(&'a Dimension),
    Fact(&'a Fact),
    Metric(&'a Metric),
}

/// The declared members by every key a reference can use: the name, and the
/// name qualified by the member's table.
struct MemberIndex<'a> {
    by_key: HashMap<String, Vec<Member<'a>>>,
    /// The tables' aliases, as keys.
    tables: HashSet<String>,
    /// The base table's alias, as a key: the table of a member declared
    /// without one.
    base: String,
    /// Every member and materialization name, as a key, which an added
    /// member's name must avoid.
    names: HashSet<String>,
}

impl<'a> MemberIndex<'a> {
    fn new(def: &'a SemanticViewDefinition) -> Self {
        let mut index = Self {
            by_key: HashMap::new(),
            tables: def
                .tables
                .iter()
                .map(|t| normalize_ident_part(&t.alias))
                .collect(),
            base: def
                .tables
                .first()
                .map(|t| normalize_ident_part(&t.alias))
                .unwrap_or_default(),
            names: HashSet::new(),
        };
        let members = def
            .dimensions
            .iter()
            .map(|d| (Member::Dimension(d), &d.name, &d.source_table))
            .chain(
                def.facts
                    .iter()
                    .map(|f| (Member::Fact(f), &f.name, &f.source_table)),
            )
            .chain(
                def.metrics
                    .iter()
                    .map(|m| (Member::Metric(m), &m.name, &m.source_table)),
            );
        for (member, name, table) in members {
            index.names.insert(normalize_ident_part(name));
            let mut keys = vec![normalize_ident_part(name)];
            // A member declared without a table is on the base table, so
            // `<base>.<name>` names it, as it does as a plain item.
            let table = table
                .as_deref()
                .or_else(|| def.tables.first().map(|t| t.alias.as_str()));
            if let Some(t) = table {
                keys.push(normalize_ident_part(&format!("{t}.{name}")));
            }
            for key in keys {
                index.by_key.entry(key).or_default().push(member);
            }
        }
        for mat in &def.materializations {
            index.names.extend(
                mat.dimensions
                    .iter()
                    .chain(&mat.metrics)
                    .map(|n| normalize_ident_part(n)),
            );
        }
        index
    }

    /// The members `r` can name.
    fn lookup(&self, r: &IdentRef<'_>) -> &[Member<'a>] {
        self.lookup_key(&r.key())
    }

    /// The members `key` names.
    fn lookup_key(&self, key: &str) -> &[Member<'a>] {
        self.by_key.get(key).map_or(&[], Vec::as_slice)
    }

    /// A struct field of a dimension or fact, as in `o.addr.city` or
    /// `addr.city` for a dimension `addr`: the dimension (else the fact) named
    /// by the longest leading parts of `r` that name one, and where the field
    /// path after them starts. A table's alias alone is not a member, so a
    /// chain that starts with one needs two parts to name one.
    fn field_path(&self, r: &IdentRef<'_>) -> Option<(Member<'a>, usize)> {
        let ends = r.part_ends();
        let fewest = if self.tables.contains(&r.first_part_key()) {
            2
        } else {
            1
        };
        (fewest..ends.len()).rev().find_map(|parts| {
            let end = ends[parts - 1];
            let key = normalize_ident_part(&r.raw[..end]);
            let candidates = self.lookup_key(&key);
            candidates
                .iter()
                .find(|m| matches!(m, Member::Dimension(_)))
                .or_else(|| candidates.iter().find(|m| matches!(m, Member::Fact(_))))
                .map(|&m| (m, end))
        })
    }

    /// The table a member is on, as a key.
    fn table_of(&self, table: Option<&str>) -> String {
        table.map_or_else(|| self.base.clone(), normalize_ident_part)
    }
}

/// The members added so far.
#[derive(Default)]
struct Added {
    dimensions: Vec<Dimension>,
    facts: Vec<Fact>,
    metrics: Vec<Metric>,
}

struct Lowering<'d, 'i> {
    view_name: &'d str,
    def: &'d SemanticViewDefinition,
    index: &'i MemberIndex<'d>,
    added: Added,
    taken: HashSet<String>,
    labels: Vec<(String, String)>,
}

/// What one reference in an expression item names.
enum Resolved<'a> {
    /// A dimension or fact, with its table, and where in the reference a
    /// struct field path after it starts (the reference's length when it
    /// names the member alone).
    RowLevel(Member<'a>, String, usize),
    Metric(&'a Metric),
    /// Nothing declared: left as written, as in `where_clause`, for `DuckDB` to
    /// read (a keyword, a function's argument such as `'year'`, or a name it
    /// reports as unknown).
    Unknown,
}

impl<'d> Lowering<'d, '_> {
    /// Add the member `text` (an item of `group`) stands for, returning its
    /// name.
    fn lower(&mut self, group: Group, text: &str) -> Result<String, ItemError> {
        check_one_expression(text)?;
        if group == Group::Metrics {
            return self.lower_metric(text);
        }
        let kind = if group == Group::Dimensions {
            "DIMENSIONS"
        } else {
            "FACTS"
        };
        for head in scan_function_heads(text) {
            if call_is_windowed(text, &head) {
                return Err(window_refused(text));
            }
            if let Some(func) = crate::graph::aggregate_name(&head.last_part_key()) {
                return Err(invalid(format!(
                    "'{text}' in {kind} holds the aggregate function '{func}'; an aggregate \
                     goes in METRICS"
                )));
            }
        }
        let (expr, table) = self.row_level(text, kind, 0..text.len())?;
        let name = self.fresh_name();
        let source_table = self.source_table(&table);
        let expr = format!("({expr})");
        if group == Group::Dimensions {
            self.added.dimensions.push(Dimension {
                name: name.clone(),
                expr,
                source_table,
                ..Dimension::default()
            });
        } else {
            self.added.facts.push(Fact {
                name: name.clone(),
                expr,
                source_table,
                ..Fact::default()
            });
        }
        Ok(name)
    }

    /// A `METRICS` item: each aggregate becomes a metric, and the rest a
    /// derived metric over them.
    fn lower_metric(&mut self, text: &str) -> Result<String, ItemError> {
        let mut aggregates: Vec<std::ops::Range<usize>> = Vec::new();
        for head in scan_function_heads(text) {
            if call_is_windowed(text, &head) {
                return Err(window_refused(text));
            }
            if aggregates.iter().any(|a| a.contains(&head.start)) {
                continue;
            }
            if crate::graph::aggregate_name(&head.last_part_key()).is_some() {
                let end = call_end(text, &head).unwrap_or(text.len());
                aggregates.push(head.start..end);
            }
        }
        let mut bases: Vec<String> = Vec::with_capacity(aggregates.len());
        for span in &aggregates {
            let (expr, table) = self.row_level(text, "an aggregate in METRICS", span.clone())?;
            let name = self.fresh_name();
            self.added.metrics.push(Metric {
                name: name.clone(),
                expr,
                source_table: self.source_table(&table),
                ..Metric::default()
            });
            bases.push(name);
        }
        if let [span] = aggregates.as_slice() {
            if span.start == 0 && span.end == text.len() {
                return Ok(bases.pop().unwrap_or_default());
            }
        }
        // An error about one of these metrics names its aggregate.
        for (span, base) in aggregates.iter().zip(&bases) {
            self.labels
                .push((base.clone(), text[span.clone()].trim().to_string()));
        }

        // The rest names metrics; each aggregate is replaced by its metric.
        let mut out = Splice::new(text);
        let mut names_any = !aggregates.is_empty();
        let mut next_aggregate = aggregates.iter().zip(&bases).peekable();
        for r in scan_references(text) {
            while let Some((span, base)) = next_aggregate.next_if(|(s, _)| s.start <= r.start) {
                out.replace(span.clone(), &quote_ident(base));
            }
            if r.start < out.copied {
                continue;
            }
            match self.resolve(&r, true)? {
                Resolved::Metric(m) => {
                    names_any = true;
                    out.replace(r.start..r.end, &quote_ident(&m.name));
                }
                Resolved::RowLevel(member, _, _) => {
                    return Err(invalid(format!(
                        "'{}' in '{text}' is a {}; in METRICS a dimension or fact can only be \
                         used inside an aggregate",
                        r.raw,
                        member.kind()
                    )));
                }
                Resolved::Unknown => {}
            }
        }
        for (span, base) in next_aggregate {
            out.replace(span.clone(), &quote_ident(base));
        }
        let out = out.finish();
        if !names_any {
            return Err(invalid(format!(
                "'{text}' in METRICS refers to no metric and aggregates no dimension or fact"
            )));
        }
        let name = self.fresh_name();
        self.added.metrics.push(Metric {
            name: name.clone(),
            expr: out,
            source_table: None,
            ..Metric::default()
        });
        Ok(name)
    }

    /// `text[span]` with each dimension written as its expression and each
    /// fact as its qualified name, and the one table they belong to. `what`
    /// names the expression in errors.
    fn row_level(
        &self,
        text: &str,
        what: &str,
        span: std::ops::Range<usize>,
    ) -> Result<(String, String), ItemError> {
        let part = &text[span];
        let mut replacements: HashMap<String, String> = HashMap::new();
        let mut table: Option<(String, String)> = None;
        for r in scan_references(part) {
            let (member, member_table, field_at) = match self.resolve(&r, false)? {
                Resolved::RowLevel(member, member_table, field_at) => {
                    (member, member_table, field_at)
                }
                Resolved::Metric(m) => {
                    return Err(invalid(format!(
                        "'{}' in '{text}' is a metric; {what} can only use dimensions and facts",
                        m.name
                    )));
                }
                Resolved::Unknown => continue,
            };
            match &table {
                Some((t, first)) if *t != member_table => {
                    return Err(invalid(format!(
                        "'{text}' uses '{first}' and '{}', which belong to different tables; \
                         the dimensions and facts of {what} must belong to one table",
                        r.raw
                    )));
                }
                Some(_) => {}
                None => table = Some((member_table.clone(), r.raw.to_string())),
            }
            let replacement = match member {
                Member::Dimension(d) => format!("({})", d.expr),
                // A fact by its name, qualified when it has a table, as fact
                // inlining keys it, and in brackets when a field path follows,
                // so that inlining still sees the fact's name on its own.
                Member::Fact(f) => {
                    let name = match &f.source_table {
                        Some(t) => format!("{}.{}", quote_ident(t), quote_ident(&f.name)),
                        None => quote_ident(&f.name),
                    };
                    if field_at < r.raw.len() {
                        format!("({name})")
                    } else {
                        name
                    }
                }
                Member::Metric(_) => unreachable!("a metric is not row-level"),
            };
            replacements.insert(r.key(), replacement + &r.raw[field_at..]);
        }
        let Some((table, _)) = table else {
            return Err(invalid(format!(
                "'{text}' refers to no dimension or fact; {what} must use at least one"
            )));
        };
        let borrowed: HashMap<String, &str> = replacements
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str()))
            .collect();
        Ok((inline_references(part, &borrowed), table))
    }

    /// What `r` names. Outside an aggregate of `METRICS` (`metrics_first`) a
    /// metric is preferred; anywhere else a dimension, then a fact.
    fn resolve(&self, r: &IdentRef<'_>, metrics_first: bool) -> Result<Resolved<'d>, ItemError> {
        let candidates = self.index.lookup(r);
        if candidates.is_empty() {
            if let Some((member, field_at)) = self.index.field_path(r) {
                return self.row_level_member(member, field_at);
            }
        }
        let pick = |want_metric: bool| {
            let rank = |m: &Member<'_>| match (m, want_metric) {
                (Member::Metric(_), true) | (Member::Dimension(_), false) => 0,
                (Member::Dimension(_), true) | (Member::Fact(_), false) => 1,
                (Member::Fact(_), true) | (Member::Metric(_), false) => 2,
            };
            candidates.iter().copied().min_by_key(rank)
        };
        match pick(metrics_first) {
            Some(Member::Metric(m)) => {
                if m.access == AccessModifier::Private {
                    return Err(ItemError::Expand(Box::new(ExpandError::PrivateMetric {
                        view_name: self.view_name.to_string(),
                        name: m.name.clone(),
                    })));
                }
                Ok(Resolved::Metric(m))
            }
            Some(member) => self.row_level_member(member, r.raw.len()),
            None if !r.is_bare() && self.index.tables.contains(&r.first_part_key()) => {
                Err(invalid(format!(
                    "'{}' is not a dimension, fact or metric of semantic view '{}'",
                    r.raw, self.view_name
                )))
            }
            None => Ok(Resolved::Unknown),
        }
    }

    /// A dimension or fact as [`Resolved::RowLevel`], refused when it is a
    /// private fact.
    fn row_level_member(
        &self,
        member: Member<'d>,
        field_at: usize,
    ) -> Result<Resolved<'d>, ItemError> {
        let table = match member {
            Member::Fact(f) => {
                if f.access == AccessModifier::Private {
                    return Err(ItemError::Expand(Box::new(ExpandError::PrivateFact {
                        view_name: self.view_name.to_string(),
                        name: f.name.clone(),
                    })));
                }
                &f.source_table
            }
            Member::Dimension(d) => &d.source_table,
            Member::Metric(_) => unreachable!("only a dimension or fact is row-level"),
        };
        Ok(Resolved::RowLevel(
            member,
            self.index.table_of(table.as_deref()),
            field_at,
        ))
    }

    /// The `source_table` of an added member on `table`: the declared alias.
    fn source_table(&self, table: &str) -> Option<String> {
        self.def
            .tables
            .iter()
            .find(|t| normalize_ident_part(&t.alias) == table)
            .map(|t| t.alias.clone())
    }

    /// A member name no declared or added member has.
    fn fresh_name(&mut self) -> String {
        let mut n = 1;
        loop {
            let name = format!("__sv_expr_{n}");
            if self.taken.insert(name.clone()) {
                return name;
            }
            n += 1;
        }
    }
}

/// `text` rebuilt with some of its spans replaced, in order.
struct Splice<'t> {
    text: &'t str,
    out: String,
    /// How far `text` has been copied or replaced.
    copied: usize,
}

impl<'t> Splice<'t> {
    fn new(text: &'t str) -> Self {
        Self {
            text,
            out: String::with_capacity(text.len()),
            copied: 0,
        }
    }

    fn replace(&mut self, span: std::ops::Range<usize>, with: &str) {
        self.out.push_str(&self.text[self.copied..span.start]);
        self.out.push_str(with);
        self.copied = span.end;
    }

    fn finish(mut self) -> String {
        self.out.push_str(&self.text[self.copied..]);
        self.out
    }
}

impl Member<'_> {
    fn kind(&self) -> &'static str {
        match self {
            Self::Dimension(_) => "dimension",
            Self::Fact(_) => "fact",
            Self::Metric(_) => "metric",
        }
    }
}

/// Refuse what cannot be one expression of a query's select list: a `;`, a
/// top-level `,`, a bracket that does not pair up by kind, an unclosed quote,
/// a parameter or a subquery.
fn check_one_expression(text: &str) -> Result<(), ItemError> {
    if has_statement_separator(text) {
        return Err(invalid(format!(
            "'{text}' holds a ';'; an item is one expression"
        )));
    }
    let bytes = text.as_bytes();
    let mut quotes = crate::util::QuoteState::default();
    let mut open: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let (next, live) = quotes.step(bytes, i);
        if live {
            match bytes[i] {
                b @ (b'(' | b'[' | b'{') => open.push(b),
                b @ (b')' | b']' | b'}') => {
                    let opener = match b {
                        b')' => b'(',
                        b']' => b'[',
                        _ => b'{',
                    };
                    if open.pop() != Some(opener) {
                        return Err(invalid(format!(
                            "'{text}' has a '{}' that closes no matching bracket",
                            char::from(b)
                        )));
                    }
                }
                b',' if open.is_empty() => {
                    return Err(invalid(format!(
                        "'{text}' is more than one expression; give each its own item"
                    )));
                }
                _ => {}
            }
        }
        i = next;
    }
    if quotes.in_string || quotes.in_ident || quotes.in_dollar() {
        return Err(invalid(format!(
            "'{text}' has a quote that is never closed"
        )));
    }
    if let Some(&b) = open.last() {
        return Err(invalid(format!(
            "'{text}' has a '{}' that is never closed",
            char::from(b)
        )));
    }
    if let Some((_, parameter)) = find_parameters(text).into_iter().next() {
        return Err(invalid(format!(
            "'{text}' holds the parameter {parameter}; parameters are only supported in the \
             WHERE clause"
        )));
    }
    if !subquery_spans(text).is_empty() {
        return Err(invalid(format!(
            "'{text}' holds a subquery; a query item cannot, only the WHERE clause can"
        )));
    }
    Ok(())
}

fn window_refused(text: &str) -> ItemError {
    invalid(format!(
        "'{text}' holds a window function; a query item cannot, so declare a window metric \
         in the semantic view"
    ))
}

fn invalid(message: String) -> ItemError {
    ItemError::Invalid(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TableRef;

    fn table(alias: &str) -> TableRef {
        TableRef {
            alias: alias.to_string(),
            table: alias.to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }
    }

    fn dim(table: &str, name: &str, expr: &str) -> Dimension {
        Dimension {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some(table.to_string()),
            ..Default::default()
        }
    }

    fn fact(table: &str, name: &str, expr: &str, access: AccessModifier) -> Fact {
        Fact {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some(table.to_string()),
            access,
            ..Default::default()
        }
    }

    fn metric(table: Option<&str>, name: &str, expr: &str, access: AccessModifier) -> Metric {
        Metric {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: table.map(str::to_string),
            access,
            ..Default::default()
        }
    }

    /// Orders `o` with customers `c` and line items `li`.
    fn def() -> SemanticViewDefinition {
        use AccessModifier::{Private, Public};
        SemanticViewDefinition {
            tables: vec![table("o"), table("c"), table("li")],
            dimensions: vec![
                dim("o", "region", "upper(o.region)"),
                dim("o", "order_date", "o.order_date"),
                dim("o", "Total Revenue Band", "o.band"),
                dim("c", "name", "c.name"),
            ],
            facts: vec![
                fact("o", "amount", "o.amount", Public),
                fact("o", "secret", "o.secret", Private),
                fact("li", "qty", "li.qty", Public),
            ],
            metrics: vec![
                metric(Some("o"), "revenue", "SUM(o.amount)", Public),
                metric(Some("o"), "order_count", "COUNT(o.id)", Public),
                metric(Some("o"), "hidden", "SUM(o.secret)", Private),
                metric(None, "per_order", "revenue / order_count", Public),
            ],
            ..Default::default()
        }
    }

    fn items(dimensions: &[&str], metrics: &[&str], facts: &[&str]) -> RequestItems {
        let owned = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        RequestItems::parse(&owned(dimensions), &owned(metrics), &owned(facts), &[]).unwrap()
    }

    fn lower(dimensions: &[&str], metrics: &[&str], facts: &[&str]) -> LoweredItems {
        lower_expression_items("v", &def(), items(dimensions, metrics, facts)).unwrap()
    }

    fn refused(dimensions: &[&str], metrics: &[&str], facts: &[&str]) -> String {
        match lower_expression_items("v", &def(), items(dimensions, metrics, facts)) {
            Err(ItemError::Invalid(message)) => message,
            Err(ItemError::Expand(e)) => e.to_string(),
            Ok(lowered) => panic!("accepted: {lowered:?}"),
        }
    }

    fn added_dimension(lowered: &LoweredItems) -> &Dimension {
        lowered.def.as_ref().unwrap().dimensions.last().unwrap()
    }

    fn added_metrics(lowered: &LoweredItems) -> Vec<(String, String, Option<String>)> {
        lowered.def.as_ref().unwrap().metrics[def().metrics.len()..]
            .iter()
            .map(|m| (m.name.clone(), m.expr.clone(), m.source_table.clone()))
            .collect()
    }

    #[test]
    fn names_and_wildcards_are_left_as_they_are() {
        let lowered = lower(
            &["o.region AS r", "c.*", "Total Revenue Band", "nope"],
            &["revenue"],
            &[],
        );
        assert!(lowered.def.is_none());
        assert!(lowered.labels.is_empty());
        let members: Vec<_> = lowered.items.dimensions.iter().map(|i| &i.member).collect();
        assert_eq!(members, ["o.region", "c.*", "Total Revenue Band", "nope"]);
        assert_eq!(lowered.items.dimensions[0].alias.as_deref(), Some("r"));
    }

    #[test]
    fn a_dimension_expression_becomes_a_dimension_on_its_table() {
        let lowered = lower(
            &["DATE_PART('year', o.order_date) AS year"],
            &["revenue"],
            &[],
        );
        let added = added_dimension(&lowered);
        assert_eq!(added.name, "__sv_expr_1");
        assert_eq!(added.expr, "(DATE_PART('year', (o.order_date)))");
        assert_eq!(added.source_table.as_deref(), Some("o"));
        let item = &lowered.items.dimensions[0];
        assert_eq!(item.member, "\"__sv_expr_1\"");
        assert_eq!(item.alias.as_deref(), Some("year"));
    }

    /// Without an alias, the column is named by the expression as written.
    #[test]
    fn an_expression_without_an_alias_names_its_column_as_written() {
        let lowered = lower(&["lower(region)  /* note */"], &[], &[]);
        assert_eq!(added_dimension(&lowered).expr, "(lower((upper(o.region))))");
        assert_eq!(
            lowered.items.dimensions[0].alias.as_deref(),
            Some("lower(region)")
        );
        assert_eq!(
            lowered.labels,
            [(
                "__sv_expr_1".to_string(),
                "lower(region)  /* note */".to_string()
            )]
        );
    }

    #[test]
    fn a_fact_in_a_dimension_expression_is_named_for_fact_inlining() {
        let lowered = lower(&["o.amount > 100 AS big"], &[], &[]);
        assert_eq!(added_dimension(&lowered).expr, "(\"o\".\"amount\" > 100)");
    }

    #[test]
    fn a_facts_expression_becomes_a_fact() {
        let lowered = lower(&[], &[], &["li.qty * 2 AS q2"]);
        let added = lowered.def.as_ref().unwrap().facts.last().unwrap();
        assert_eq!(added.expr, "(\"li\".\"qty\" * 2)");
        assert_eq!(added.source_table.as_deref(), Some("li"));
        assert_eq!(added.access, AccessModifier::Public);
    }

    #[test]
    fn a_dimension_expression_over_two_tables_is_refused() {
        let msg = refused(&["region || c.name"], &[], &[]);
        assert!(msg.contains("different tables"), "{msg}");
        assert!(
            msg.contains("'region'") && msg.contains("'c.name'"),
            "{msg}"
        );
    }

    #[test]
    fn an_aggregate_in_dimensions_or_facts_is_refused() {
        let msg = refused(&["sum(o.amount)"], &[], &[]);
        assert!(
            msg.contains("aggregate function 'sum'") && msg.contains("METRICS"),
            "{msg}"
        );
        let msg = refused(&[], &[], &["max(li.qty)"]);
        assert!(msg.contains("in FACTS"), "{msg}");
    }

    #[test]
    fn a_metric_in_a_dimension_expression_is_refused() {
        let msg = refused(&["revenue + 1"], &[], &[]);
        assert!(
            msg.contains("'revenue' in 'revenue + 1' is a metric"),
            "{msg}"
        );
    }

    #[test]
    fn an_expression_naming_no_member_is_refused() {
        let msg = refused(&["1 + 1"], &[], &[]);
        assert!(msg.contains("refers to no dimension or fact"), "{msg}");
    }

    #[test]
    fn an_unknown_member_of_a_table_is_refused() {
        let msg = refused(&["upper(o.nope)"], &[], &[]);
        assert!(
            msg.contains("'o.nope' is not a dimension, fact or metric"),
            "{msg}"
        );
    }

    #[test]
    fn a_private_fact_or_metric_is_refused() {
        let msg = refused(&["o.secret + 1"], &[], &[]);
        assert!(msg.contains("fact 'secret' is private"), "{msg}");
        let msg = refused(&[], &["sum(o.secret)"], &[]);
        assert!(msg.contains("fact 'secret' is private"), "{msg}");
        let msg = refused(&[], &["hidden * 2"], &[]);
        assert!(msg.contains("metric 'hidden' is private"), "{msg}");
    }

    #[test]
    fn an_aggregate_in_metrics_becomes_a_metric_on_its_table() {
        let lowered = lower(&[], &["SUM(o.amount) AS total"], &[]);
        assert_eq!(
            added_metrics(&lowered),
            [(
                "__sv_expr_1".to_string(),
                "SUM(\"o\".\"amount\")".to_string(),
                Some("o".to_string())
            )]
        );
        assert_eq!(lowered.items.metrics[0].alias.as_deref(), Some("total"));
    }

    #[test]
    fn an_aggregate_of_a_dimension_aggregates_its_expression() {
        let lowered = lower(&[], &["count(DISTINCT region)"], &[]);
        assert_eq!(
            added_metrics(&lowered)[0].1,
            "count(DISTINCT (upper(o.region)))"
        );
    }

    #[test]
    fn a_filter_clause_belongs_to_its_aggregate() {
        let lowered = lower(
            &[],
            &["sum(o.amount) FILTER (WHERE region = 'EU') / revenue"],
            &[],
        );
        assert_eq!(
            added_metrics(&lowered),
            [
                (
                    "__sv_expr_1".to_string(),
                    "sum(\"o\".\"amount\") FILTER (WHERE (upper(o.region)) = 'EU')".to_string(),
                    Some("o".to_string())
                ),
                (
                    "__sv_expr_2".to_string(),
                    "\"__sv_expr_1\" / \"revenue\"".to_string(),
                    None
                ),
            ]
        );
    }

    #[test]
    fn an_expression_over_metrics_becomes_a_derived_metric() {
        let lowered = lower(&[], &["round(o.revenue / per_order, 2)"], &[]);
        assert_eq!(
            added_metrics(&lowered),
            [(
                "__sv_expr_1".to_string(),
                "round(\"revenue\" / \"per_order\", 2)".to_string(),
                None
            )]
        );
    }

    #[test]
    fn aggregates_beside_metrics_become_metrics_under_a_derived_one() {
        let lowered = lower(&[], &["sum(o.amount) / count(c.name) + revenue"], &[]);
        assert_eq!(
            added_metrics(&lowered),
            [
                (
                    "__sv_expr_1".to_string(),
                    "sum(\"o\".\"amount\")".to_string(),
                    Some("o".to_string())
                ),
                (
                    "__sv_expr_2".to_string(),
                    "count((c.name))".to_string(),
                    Some("c".to_string())
                ),
                (
                    "__sv_expr_3".to_string(),
                    "\"__sv_expr_1\" / \"__sv_expr_2\" + \"revenue\"".to_string(),
                    None
                ),
            ]
        );
    }

    #[test]
    fn a_dimension_outside_an_aggregate_in_metrics_is_refused() {
        let msg = refused(&[], &["revenue + region"], &[]);
        assert!(
            msg.contains("'region' in 'revenue + region' is a dimension"),
            "{msg}"
        );
    }

    #[test]
    fn an_aggregate_of_a_metric_is_refused() {
        let msg = refused(&[], &["max(revenue)"], &[]);
        assert!(
            msg.contains("'revenue' in 'max(revenue)' is a metric"),
            "{msg}"
        );
    }

    #[test]
    fn an_aggregate_of_no_member_is_refused() {
        let msg = refused(&[], &["count(*)"], &[]);
        assert!(msg.contains("refers to no dimension or fact"), "{msg}");
        let msg = refused(&[], &["1 + 2"], &[]);
        assert!(msg.contains("refers to no metric"), "{msg}");
    }

    #[test]
    fn an_aggregate_over_two_tables_is_refused() {
        let msg = refused(&[], &["sum(o.amount * li.qty)"], &[]);
        assert!(msg.contains("different tables"), "{msg}");
    }

    #[test]
    fn a_window_function_is_refused() {
        let msg = refused(&[], &["sum(o.amount) OVER ()"], &[]);
        assert!(msg.contains("window function"), "{msg}");
        let msg = refused(&["row_number() OVER (ORDER BY region)"], &[], &[]);
        assert!(msg.contains("window function"), "{msg}");
    }

    #[test]
    fn what_is_not_one_expression_is_refused() {
        for (item, expected) in [
            ("upper(region), lower(region)", "more than one expression"),
            ("upper(region); SELECT 1", "';'"),
            ("upper(region))", "closes no matching bracket"),
            ("upper(region]", "closes no matching bracket"),
            ("upper((region)", "never closed"),
            ("region || 'x", "quote that is never closed"),
            ("region = $1", "parameter $1"),
            ("region IN (SELECT 1)", "subquery"),
        ] {
            let msg = refused(&[item], &[], &[]);
            assert!(msg.contains(expected), "{item}: {msg}");
        }
    }

    #[test]
    fn an_error_names_the_item_not_its_member() {
        let lowered = lower(&["upper(region) AS u"], &["sum(o.amount)"], &[]);
        assert_eq!(
            lowered.relabel("metric '__sv_expr_2' joined to dimension '__sv_expr_1'"),
            "metric 'sum(o.amount)' joined to dimension 'upper(region)'"
        );
    }

    #[test]
    fn an_error_about_an_aggregate_inside_an_item_names_the_aggregate() {
        let lowered = lower(&[], &["sum(o.amount) / revenue"], &[]);
        assert_eq!(
            lowered.relabel("metric '__sv_expr_1' and metric '__sv_expr_2'"),
            "metric 'sum(o.amount)' and metric 'sum(o.amount) / revenue'"
        );
    }

    #[test]
    fn an_added_member_takes_a_name_no_member_has() {
        let mut def = def();
        def.dimensions.push(dim("o", "__sv_expr_1", "o.x"));
        let lowered = lower_expression_items(
            "v",
            &def,
            items(&["upper(region)", "lower(region)"], &[], &[]),
        )
        .unwrap();
        let names: Vec<_> = lowered.labels.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["__sv_expr_2", "__sv_expr_3"]);
    }

    /// A member declared without a table is on the base table, so the base
    /// table's alias qualifies it inside an expression, as it does as an item.
    #[test]
    fn a_member_without_a_table_can_be_qualified_by_the_base_table() {
        let lowered = lower(&[], &["o.per_order * 2 AS twice"], &[]);
        assert_eq!(added_metrics(&lowered)[0].1, "\"per_order\" * 2");
    }

    /// `o.addr.city`, or `addr.city`, is the field `city` of the dimension
    /// `addr`; a fact's field reads it through its name.
    #[test]
    fn a_struct_field_of_a_member_is_read_from_the_member() {
        let mut def = def();
        def.dimensions.push(dim("o", "addr", "o.address"));
        def.facts
            .push(fact("li", "size", "li.dims", AccessModifier::Public));
        let lowered = lower_expression_items(
            "v",
            &def,
            items(&["upper(o.addr.city)", "addr.zip"], &[], &["li.size.w * 2"]),
        )
        .unwrap();
        let added = lowered.def.as_ref().unwrap();
        let exprs: Vec<_> = added.dimensions[def.dimensions.len()..]
            .iter()
            .map(|d| d.expr.as_str())
            .collect();
        assert_eq!(exprs, ["(upper((o.address).city))", "((o.address).zip)"]);
        assert_eq!(
            added.facts.last().unwrap().expr,
            "((\"li\".\"size\").w * 2)"
        );
        // A field of a table's column, with no member under it, is still not
        // a member.
        assert!(refused(&["upper(o.nope.city)"], &[], &[]).contains("'o.nope.city' is not"));
    }

    /// The added members are the request's own items: an unknown name's
    /// error does not offer them, and an error listing the query's metrics
    /// names them by their items.
    #[test]
    fn an_expansion_error_does_not_offer_the_added_members() {
        let lowered = lower(&["lower(region)"], &["max(o.amount)"], &[]);
        let unknown = lowered.for_request(ExpandError::UnknownDimension {
            view_name: "v".to_string(),
            name: "regio".to_string(),
            available: vec![
                "region".to_string(),
                "__sv_expr_1".to_string(),
                "order_date".to_string(),
            ],
            suggestion: Some("__sv_expr_1".to_string()),
        });
        assert!(matches!(
            &unknown,
            ExpandError::UnknownDimension { available, suggestion: None, .. }
                if available == &["region", "order_date"]
        ));
        let mixing = lowered.for_request(ExpandError::WindowAggregateMixing {
            view_name: "v".to_string(),
            window_metrics: vec!["running".to_string()],
            aggregate_metrics: vec!["revenue".to_string(), "__sv_expr_2".to_string()],
        });
        assert!(matches!(
            &mixing,
            ExpandError::WindowAggregateMixing { aggregate_metrics, .. }
                if aggregate_metrics == &["revenue", "max(o.amount)"]
        ));
    }
}
