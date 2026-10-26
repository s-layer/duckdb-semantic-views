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

use crate::expand::{quote_ident, quote_stored_ident, ExpandError};
use crate::expr_tokens::{
    call_end, call_is_windowed, find_parameters, has_statement_separator, scan_function_heads,
    scan_references, subquery_spans, IdentRef,
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
    /// Nothing declared. Left as written when it is not a column reference
    /// ([`is_not_a_column`]: a keyword, a number, …),
    /// written as a call when it is a SQL value function, and refused
    /// otherwise ([`Lowering::refuse_undeclared`]).
    Unknown,
}

impl<'d> Lowering<'d, '_> {
    /// Add the member `text` (an item of `group`) stands for, returning its
    /// name.
    fn lower(&mut self, group: Group, text: &str) -> Result<String, ItemError> {
        check_one_expression(text)?;
        check_calls(text)?;
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
        let lambdas = lambda_scopes(text);
        for r in scan_references(text) {
            while let Some((span, base)) = next_aggregate.next_if(|(s, _)| s.start <= r.start) {
                out.replace(span.clone(), &quote_ident(base));
            }
            if r.start < out.copied || is_local_name(text, &r, &lambdas) {
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
                Resolved::Unknown => {
                    if let Some(call) = self.refuse_undeclared(text, &r, text)? {
                        out.replace(r.start..r.end, call);
                    }
                }
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
        let mut out = Splice::new(part);
        let mut table: Option<(String, String)> = None;
        let lambdas = lambda_scopes(part);
        for r in scan_references(part) {
            if is_local_name(part, &r, &lambdas) {
                continue;
            }
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
                Resolved::Unknown => {
                    if let Some(call) = self.refuse_undeclared(part, &r, text)? {
                        out.replace(r.start..r.end, call);
                    }
                    continue;
                }
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
                        Some(t) => format!("{}.{}", quote_stored_ident(t), quote_ident(&f.name)),
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
            out.replace(r.start..r.end, &(replacement + &r.raw[field_at..]));
        }
        let Some((table, _)) = table else {
            return Err(invalid(format!(
                "'{text}' refers to no dimension or fact; {what} must use at least one"
            )));
        };
        Ok((out.finish(), table))
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

    /// Refuse `r`, a name in `scanned` (part of the item `item`) that is no
    /// member of the view, unless it is not a column reference at all
    /// ([`is_not_a_column`]), or is a SQL value function, when it returns the
    /// call to write in its place ([`sql_value_function`]). An expression item
    /// reads only the view's members: any other name would read a column of a
    /// table the query joins that the view does not expose, such as the column
    /// under a private fact.
    fn refuse_undeclared(
        &self,
        scanned: &str,
        r: &IdentRef<'_>,
        item: &str,
    ) -> Result<Option<&'static str>, ItemError> {
        if is_not_a_column(scanned, r) {
            return Ok(None);
        }
        if let Some(call) = sql_value_function(r) {
            return Ok(Some(call));
        }
        let hash = if scanned[..r.start].ends_with('#') {
            "#"
        } else {
            ""
        };
        Err(invalid(format!(
            "'{hash}{}' in '{item}' is not a dimension, fact or metric of semantic view '{}'; \
             an expression item can only read the view's members",
            r.raw, self.view_name
        )))
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

/// The SQL value functions `DuckDB` calls without parentheses, and the call
/// its binder makes of each, sorted. A bare `current_date` reads a column of
/// that name when the query joins one and calls the function only when none
/// does, so an expression item writes the call itself.
const SQL_VALUE_FUNCTIONS: &[(&str, &str)] = &[
    ("current_catalog", "current_catalog()"),
    ("current_date", "current_date()"),
    ("current_role", "current_role()"),
    ("current_schema", "current_schema()"),
    ("current_time", "get_current_time()"),
    ("current_timestamp", "get_current_timestamp()"),
    ("current_user", "current_user()"),
    ("localtime", "current_localtime()"),
    ("localtimestamp", "current_localtimestamp()"),
    ("session_user", "session_user()"),
    // `user()` is a macro over a bare `current_user`, which a column would
    // capture in the same way.
    ("user", "current_user()"),
];

/// The call that a bare, unquoted `r` stands for when it names a SQL value
/// function.
fn sql_value_function(r: &IdentRef<'_>) -> Option<&'static str> {
    if !r.is_bare() || r.raw.starts_with('"') {
        return None;
    }
    let word = r.raw.to_ascii_lowercase();
    SQL_VALUE_FUNCTIONS
        .binary_search_by(|(name, _)| name.cmp(&word.as_str()))
        .ok()
        .map(|i| SQL_VALUE_FUNCTIONS[i].1)
}

/// A `lambda x, y: body` in an expression: its parameters' keys, and the span
/// from the `lambda` to the end of its body, where they name the parameters.
/// `DuckDB` refuses the deprecated arrow spelling (`x -> body`) unless a
/// setting allows it, and reads `->` as the JSON operator otherwise, so a name
/// beside `->` is never taken for a parameter.
struct LambdaScope {
    params: Vec<String>,
    span: std::ops::Range<usize>,
}

/// The lambdas in `text`, which has no comments.
fn lambda_scopes(text: &str) -> Vec<LambdaScope> {
    let bytes = text.as_bytes();
    let refs = scan_references(text);
    let mut out = Vec::new();
    for r in &refs {
        if !r.raw.eq_ignore_ascii_case("lambda") {
            continue;
        }
        let mut params = Vec::new();
        let mut i = r.end;
        loop {
            i = skip_spaces(bytes, i);
            let Some(param) = refs.iter().find(|p| p.start == i && p.is_bare()) else {
                break;
            };
            params.push(param.key());
            i = skip_spaces(bytes, param.end);
            match bytes.get(i) {
                Some(b',') => i += 1,
                Some(b':') if bytes.get(i + 1) != Some(&b':') => {
                    out.push(LambdaScope {
                        params,
                        span: r.start..body_end(bytes, i + 1),
                    });
                    break;
                }
                _ => break,
            }
        }
    }
    out
}

/// Whether the name or call head `r` starts with a parameter of a lambda it
/// is inside: `x`, `"x"`, `s.a`, `x.lower()`.
fn is_lambda_param(r: &IdentRef<'_>, lambdas: &[LambdaScope]) -> bool {
    let first = r.first_part_key();
    lambdas
        .iter()
        .any(|l| l.span.contains(&r.start) && l.params.contains(&first))
}

fn skip_spaces(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// Where a lambda body starting at `from` ends: at the bracket that closes
/// the list it is in, or at a `,` beside it.
fn body_end(bytes: &[u8], from: usize) -> usize {
    let mut quotes = crate::util::QuoteState::default();
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        let (next, live) = quotes.step(bytes, i);
        if live {
            match bytes[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' | b',' if depth == 0 => return i,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }
        }
        i = next;
    }
    bytes.len()
}

/// The word that ends just before `at` in `text`, skipping whitespace.
fn word_before(text: &str, at: usize) -> &str {
    let head = text[..at].trim_end();
    let start = head
        .bytes()
        .rposition(|b| !crate::util::is_ident_byte(b))
        .map_or(0, |p| p + 1);
    &head[start..]
}

/// The word that starts just after `at` in `text`, skipping whitespace.
fn word_after(text: &str, at: usize) -> &str {
    let tail = text[at..].trim_start();
    let end = tail
        .bytes()
        .position(|b| !crate::util::is_ident_byte(b))
        .unwrap_or(tail.len());
    &tail[..end]
}

/// Whether the text before `at` ends with an operand, so that a `BETWEEN` at
/// `at` is the operator rather than a column of that name: a name, a literal,
/// a bracket's close, `NULL`, `TRUE`, `FALSE` or a `CASE`'s `END`, and the
/// operand before a `NOT`. After an operator, a keyword that takes an
/// expression (`WHEN`, `AND`, `AT TIME ZONE`, a `BETWEEN`) or nothing, the
/// word is a column.
fn follows_an_operand(text: &str, at: usize) -> bool {
    let head = text[..at].trim_end();
    match head.bytes().last() {
        Some(b')' | b']' | b'}' | b'\'' | b'"') => true,
        Some(b) if crate::util::is_ident_byte(b) => {
            let word = word_before(text, at).to_ascii_lowercase();
            match word.as_str() {
                "not" => follows_an_operand(text, head.len() - word.len()),
                "null" | "true" | "false" | "end" => true,
                "between" | "zone" => false,
                w => !crate::ident::is_refused_as_name(w),
            }
        }
        _ => false,
    }
}

/// Whether `r`, a name in `text` (which has no comments), is a name of the
/// expression's own rather than a reference to resolve: a collation, or a
/// parameter of a lambda it is inside. It is left as written, even when a
/// member has its name. (A field of an expression's value and a named argument
/// are not scanned as references at all: [`scan_references`].)
fn is_local_name(text: &str, r: &IdentRef<'_>, lambdas: &[LambdaScope]) -> bool {
    word_before(text, r.start).eq_ignore_ascii_case("collate") || is_lambda_param(r, lambdas)
}

/// Whether `r`, a name in `text` that is no member of the view and no local
/// name ([`is_local_name`]), is not a column reference: a number, or a keyword
/// or another word of the expression grammar where the grammar reads it as
/// one.
fn is_not_a_column(text: &str, r: &IdentRef<'_>) -> bool {
    if r.raw.as_bytes()[0].is_ascii_digit() {
        // `#3` is the third column of the query's tables.
        return !text[..r.start].ends_with('#');
    }
    if !r.is_bare() || r.raw.starts_with('"') {
        return false;
    }
    let word = r.raw.to_ascii_lowercase();
    crate::ident::is_refused_as_name(&word)
        || (word == "map" && text[r.end..].trim_start().starts_with('{'))
        || (word == "between" && follows_an_operand(text, r.start))
        || (word == "nulls" && {
            let next = word_after(text, r.end);
            next.eq_ignore_ascii_case("first") || next.eq_ignore_ascii_case("last")
        })
        || ((word == "first" || word == "last")
            && word_before(text, r.start).eq_ignore_ascii_case("nulls"))
}

/// Refuse the calls in `text` that read columns other than through the names
/// the scan checks: `COLUMNS(...)`, which selects columns by name, and a call
/// by a qualified name, which `DuckDB` reads as a method call on a column
/// (`x.abs()` is `abs(x)`) when the qualifier names one. A schema-qualified
/// call is refused with it; a method call on a lambda's parameter is kept.
fn check_calls(text: &str) -> Result<(), ItemError> {
    let lambdas = lambda_scopes(text);
    for head in scan_function_heads(text) {
        let last = head.last_part_key();
        if last == "columns" {
            return Err(invalid(format!(
                "'{text}' holds {}(...), which selects columns by name; an expression item can \
                 only read the view's members",
                head.raw
            )));
        }
        if !head.is_bare() && !is_lambda_param(&head, &lambdas) {
            return Err(invalid(format!(
                "'{}(...)' in '{text}' calls a function by a qualified name; an expression \
                 item can only call a function by its own name, as in {last}(...)",
                head.raw
            )));
        }
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

    /// The message refusing `name`, a column the view does not declare.
    fn undeclared(name: &str, msg: &str) -> bool {
        msg.contains(&format!("'{name}' in '"))
            && msg.contains("is not a dimension, fact or metric of semantic view 'v'")
    }

    #[test]
    fn an_undeclared_column_in_a_dimension_expression_is_refused() {
        let msg = refused(&["region || tier"], &[], &[]);
        assert!(undeclared("tier", &msg), "{msg}");
    }

    #[test]
    fn an_undeclared_column_in_a_facts_expression_is_refused() {
        let msg = refused(&[], &[], &["amount + salt"]);
        assert!(undeclared("salt", &msg), "{msg}");
    }

    #[test]
    fn an_undeclared_column_beside_a_fact_in_an_aggregate_is_refused() {
        // The column under the private fact `secret`, read through a public
        // fact's aggregate.
        let msg = refused(&[], &["sum(amount * 0 + secret_col)"], &[]);
        assert!(undeclared("secret_col", &msg), "{msg}");
    }

    #[test]
    fn an_undeclared_column_outside_an_aggregate_is_refused() {
        let msg = refused(&[], &["revenue + tier"], &[]);
        assert!(undeclared("tier", &msg), "{msg}");
    }

    #[test]
    fn a_quoted_undeclared_column_is_refused() {
        let msg = refused(&["region || \"tier\""], &[], &[]);
        assert!(undeclared("\"tier\"", &msg), "{msg}");
    }

    #[test]
    fn a_field_of_an_undeclared_column_is_refused() {
        let msg = refused(&["region || info.tier"], &[], &[]);
        assert!(undeclared("info.tier", &msg), "{msg}");
    }

    #[test]
    fn an_undeclared_column_read_as_json_is_refused() {
        let msg = refused(&["region || (tier->'a')"], &[], &[]);
        assert!(undeclared("tier", &msg), "{msg}");
    }

    /// Each case is lowered on its own, and every one that is accepted is
    /// reported, so that no case hides behind another.
    fn assert_each_refused(cases: &[(&str, &str, &str)]) {
        let mut accepted = Vec::new();
        for &(group, text, want) in cases {
            let one = [text];
            let (d, m, f): (&[&str], &[&str], &[&str]) = match group {
                "dimension" => (&one, &[], &[]),
                "metric" => (&[], &one, &[]),
                _ => (&[], &[], &one),
            };
            match lower_expression_items("v", &def(), items(d, m, f)) {
                Err(ItemError::Invalid(msg)) if msg.contains(want) => {}
                Err(e) => accepted.push(format!("{group} {text}: {e:?}")),
                Ok(_) => accepted.push(format!("{group} {text}: accepted")),
            }
        }
        assert!(accepted.is_empty(), "not refused:\n{}", accepted.join("\n"));
    }

    #[test]
    fn an_undeclared_column_beside_a_json_arrow_is_refused() {
        // DuckDB reads `->` as the JSON operator unless it is the deprecated
        // lambda arrow, which it refuses by default; either way a name beside
        // it is not a lambda's parameter here.
        let tier = "'tier' in '";
        assert_each_refused(&[
            ("dimension", "region || (tier -> ('$'))", tier),
            ("dimension", "region || ((tier::VARCHAR) -> ('$'))", tier),
            ("dimension", "(region || tier) -> ('a')", tier),
            ("dimension", "region || (tier -> -1)", tier),
            ("dimension", "region || (tier -> E'$.a')", tier),
            ("dimension", "region || (tier -> $$a$$)", tier),
            ("dimension", "region || (o -> ('$'))", "'o' in '"),
            (
                "dimension",
                "list_transform([amount], x -> x + 1)[1]",
                "'x' in '",
            ),
        ]);
    }

    #[test]
    fn a_method_call_or_a_columns_expression_is_refused() {
        let method = "calls a function by a qualified name";
        assert_each_refused(&[
            ("metric", "sum(amount * 0 + tier.abs())", method),
            ("dimension", "region || tier.lower()", method),
            ("dimension", "region || region.lower()", method),
            ("dimension", "COLUMNS('^tier$')", "selects columns by name"),
            (
                "metric",
                "sum(columns('amount'))",
                "selects columns by name",
            ),
        ]);
    }

    #[test]
    fn a_positional_reference_is_refused() {
        assert_each_refused(&[
            ("metric", "sum(amount * 0 + #3)", "'#3'"),
            ("dimension", "region || #1", "'#1'"),
        ]);
    }

    #[test]
    fn a_column_named_like_a_word_of_the_grammar_is_refused() {
        assert_each_refused(&[
            ("dimension", "region || map", "'map' in '"),
            ("dimension", "region || between", "'between' in '"),
            ("dimension", "NOT between OR amount > 0", "'between' in '"),
            (
                "dimension",
                "CASE WHEN between THEN amount END",
                "'between' in '",
            ),
            (
                "dimension",
                "order_date AT TIME ZONE between",
                "'between' in '",
            ),
            (
                "dimension",
                "amount BETWEEN between AND 3",
                "'between' in '",
            ),
            (
                "dimension",
                "amount BETWEEN 1 AND between",
                "'between' in '",
            ),
        ]);
    }

    #[test]
    fn a_lambda_parameter_is_not_a_column_quoted_or_with_a_field() {
        let refused: Vec<String> = [
            "list_transform([struct_pack(a := amount)], lambda s: s.a)[1]",
            "list_transform([amount], lambda \"x\": \"x\" + 1)[1]",
            "list_transform([amount], lambda X: x + 1)[1]",
            "list_transform([region], lambda x: x.lower())[1]",
        ]
        .into_iter()
        .filter_map(|d| {
            lower_expression_items("v", &def(), items(&[d], &[], &[]))
                .err()
                .map(|e| format!("{d}: {e:?}"))
        })
        .collect();
        assert!(refused.is_empty(), "refused:\n{}", refused.join("\n"));
    }

    /// The expression each dimension item lowers to, one item at a time, or
    /// its error.
    fn lowered_dimensions(texts: &[&str]) -> Vec<Result<String, String>> {
        texts
            .iter()
            .map(|t| {
                lower_expression_items("v", &def(), items(&[t], &[], &[]))
                    .map(|l| added_dimension(&l).expr.clone())
                    .map_err(|e| format!("{e:?}"))
            })
            .collect()
    }

    #[test]
    fn a_field_argument_or_lambda_name_is_left_as_written() {
        // A field, a named argument and a lambda's parameter are names of
        // their own, even when a member or a SQL value function has the name.
        assert_eq!(
            lowered_dimensions(&[
                "struct_pack(user := region).user",
                "current_date - struct_pack(current_date := order_date).current_date",
                "list_transform([order_date], lambda current_date: current_date)[1]",
                "struct_pack(region := order_date).region",
                "list_transform([order_date], lambda region: region)[1]",
            ]),
            [
                Ok("(struct_pack(user := (upper(o.region))).user)".to_string()),
                Ok(
                    "(current_date() - struct_pack(current_date := (o.order_date)).current_date)"
                        .to_string()
                ),
                Ok(
                    "(list_transform([(o.order_date)], lambda current_date: current_date)[1])"
                        .to_string()
                ),
                Ok("(struct_pack(region := (o.order_date)).region)".to_string()),
                Ok("(list_transform([(o.order_date)], lambda region: region)[1])".to_string()),
            ]
        );
    }

    #[test]
    fn a_field_or_argument_named_like_a_metric_is_left_as_written() {
        let lowered = lower(&[], &["revenue + struct_pack(revenue := 1).revenue"], &[]);
        let metrics = added_metrics(&lowered);
        assert_eq!(
            metrics.last().map(|(_, expr, _)| expr.as_str()),
            Some("\"revenue\" + struct_pack(revenue := 1).revenue")
        );
    }

    #[test]
    fn a_sql_value_function_is_called_rather_than_read_as_a_column() {
        // A bare `current_date` reads a column of that name when the query
        // joins one, so it is written as the function DuckDB would call.
        let lowered = lower(
            &[
                "current_date - order_date",
                "order_date < current_timestamp",
            ],
            &[],
            &[],
        );
        let dims = &lowered.def.as_ref().unwrap().dimensions;
        let exprs: Vec<_> = dims[dims.len() - 2..]
            .iter()
            .map(|d| d.expr.as_str())
            .collect();
        assert_eq!(
            exprs,
            [
                "(current_date() - (o.order_date))",
                "((o.order_date) < get_current_timestamp())"
            ]
        );
    }

    #[test]
    fn an_undeclared_column_named_like_a_lambda_parameter_elsewhere_is_refused() {
        // `x` is a parameter inside the lambda only.
        let msg = refused(
            &["list_transform([amount], lambda x: x + 1)[1] + x"],
            &[],
            &[],
        );
        assert!(undeclared("x", &msg), "{msg}");
    }

    #[test]
    fn syntax_words_numbers_and_local_names_are_not_columns() {
        for dimension in [
            "CASE WHEN region = 'EU' THEN 1 ELSE 0 END",
            "region IS NOT NULL AND NOT (region IS DISTINCT FROM 'x') OR TRUE",
            "amount BETWEEN 1 AND 1.5e3",
            "CAST(amount AS INTEGER) + amount::DOUBLE",
            "order_date + INTERVAL 1 DAY + INTERVAL '2' HOUR",
            "EXTRACT(year FROM order_date)",
            "region COLLATE nocase",
            "region LIKE 'E%' ESCAPE '!' OR region ILIKE 'e%'",
            "current_date - order_date",
            "list_transform([amount], lambda x: x + 1)",
            "amount NOT BETWEEN 1 AND 2 OR (amount) BETWEEN 1 AND 2",
            "amount::INTEGER BETWEEN 1 AND 2 AND [amount] BETWEEN [1] AND [2]",
            "list_reduce([amount], lambda a, b: a + b)",
            "struct_pack(k := region)",
            "struct_pack(k := region).k || (struct_pack(k := region)).k",
            "region IN ('EU', 'US')",
            "MAP {'a': amount}",
            "amount * 2 /* tier */",
            "amount * 2 -- tier\n",
        ] {
            lower_expression_items("v", &def(), items(&[dimension], &[], &[]))
                .unwrap_or_else(|e| panic!("{dimension}: {e:?}"));
        }
        for metric in [
            "string_agg(region, ',' ORDER BY region DESC NULLS LAST)",
            "count(DISTINCT region)",
            "count(amount) FILTER (WHERE region = 'EU')",
        ] {
            lower_expression_items("v", &def(), items(&[], &[metric], &[]))
                .unwrap_or_else(|e| panic!("{metric}: {e:?}"));
        }
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
