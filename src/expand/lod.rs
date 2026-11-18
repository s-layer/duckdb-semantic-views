//! Level-of-detail expressions (Phase 8.5a: `FIXED DIMENSIONS`; Phase 8.5b:
//! `INCLUDE` and `EXCLUDE DIMENSIONS`).
//!
//! Snowflake computes `COUNT(orders.order_id) FIXED DIMENSIONS
//! (customers.customer_id)` once per customer over every row, whatever the
//! query groups by or filters on, and a metric over it (`AVG(...)`) reads each
//! customer's value once per group the customer's rows fall in (fixture 11
//! L1-L18, 11b E1-E8). That is the once-per-group count the expansion already
//! makes for a metric on a table that the query reaches across a fan-out
//! (Phase 7 PR 4c), so the expansion's working copy reads each LOD expression
//! as a table of its own:
//!
//! - a hidden table `__sv_lod_<n>`, an inline query over the expression's
//!   home table (the table its aggregate reads) joined to the tables its
//!   fixed dimensions are on, grouped by them: one row per fixed-grain value,
//!   keyed by a column `__sv_lodk_<n>` holding them as one `ROW(...)`, and the
//!   value in `__sv_lod`;
//! - the home table, now a derived table that adds the same key column, and
//!   a relationship from it to the hidden table.
//!
//! The key is one `ROW(...)` because a row compares equal to a row with the
//! same NULL fields, so rows whose fixed dimension is NULL (an order with no
//! customer) find the value computed for them (E1, E3: the NULL group of 2).
//! `FIXED DIMENSIONS ()` is keyed by a constant.
//!
//! A named LOD fact or dimension becomes that table's fact or dimension.
//! A metric over LOD expressions becomes a derived metric over hidden private
//! metrics `__sv_lodm_<n>`, one per aggregate call: one that reads an LOD
//! expression is a metric on its table, any other is a metric on the table
//! whose members it reads. Every later step then sees ordinary tables,
//! relationships and members: the hidden table's value is computed over all
//! rows before the query's `WHERE`, which only decides which groups and rows
//! remain, as Snowflake applies a filter after a FIXED calculation.
//!
//! `INCLUDE` and `EXCLUDE DIMENSIONS` have no grain of their own: their grain
//! is the query's dimensions plus (`INCLUDE`) or minus (`EXCLUDE`) the listed
//! ones, and Snowflake computes them over the rows the query's `WHERE` keeps
//! (fixture 11 L6-L11, 11b E5, E6). `query_grain` reads that grain and the
//! filter from the request, refusing what Snowflake refuses (010969, 010970),
//! and `lower_lod_at` builds their hidden tables at it, the filter inside
//! each one's query. An `INCLUDE` one is joined to its home table, as a
//! `FIXED` one is. An `EXCLUDE` one is joined to the table a dimensions-only
//! query of the grain lists from (`sql_gen::member_anchor`), so a group with no
//! row of the home table (a customer with no orders) gets its value too, and
//! to its home table through a twin, so a group of home rows with no row there
//! (orders with no customer row) does as well; the metric reads the first
//! value that is not `NULL`.
//!
//! When the query's dimensions and filter are listed from a table below the
//! home table (returns below orders), an `INCLUDE` or `EXCLUDE` hidden table
//! is computed over that table's rows instead, each joined up to the home,
//! and joined to that table alone (`child_root`, TECH-DEBT #115): Snowflake's
//! plans for such a query (fixture 14c) count an order once per return in a
//! group, and list the groups from the returns.
//!
//! With no query (`CREATE` checks, `SHOW`), an `INCLUDE` one is lowered at
//! its listed dimensions and an `EXCLUDE` one at none.
//!
//! The lowering runs after the bare-column, dimension-reference and window
//! lowerings, so the member expressions it splices are already qualified, and
//! those steps leave LOD members alone (they have no table of their own).
//! Only the working copy changes: `DESCRIBE`, `GET_DDL` and the YAML export
//! show each LOD expression as written.

use std::borrow::Cow;
use std::fmt::Write as _;

use crate::expr_tokens::scan_references;
use crate::graph::lod::{
    collect_lods, has_lod, is_lod_fact, member_table, resolve, resolve_lod, LodKind, Origin,
    PartLod, Resolution, Resolved,
};
use crate::ident::{ident_matches, normalize_ident_part};
use crate::model::{
    AccessModifier, Cardinality, Fact, Join, Metric, SemanticViewDefinition, TableRef,
};

use super::join_resolver::synthesize_on_clause;
use super::resolution::{quote_ident, quote_stored_ident, table_source};

/// The column of a hidden LOD table that holds the value.
pub(crate) const VALUE_COLUMN: &str = "__sv_lod";

/// The prefix of a hidden LOD table's alias.
pub(crate) const TABLE_PREFIX: &str = "__sv_lod_";

/// `FROM <home> LEFT JOIN ...`: `home`'s source with every table of `tables`,
/// and every table the qualified names and facts of `texts` read, joined
/// along its one path from `home`.
fn joined_from(
    def: &SemanticViewDefinition,
    home: &TableRef,
    tables: &[String],
    texts: &[&str],
) -> String {
    let base = def.tables.first().map_or("", |b| b.alias.as_str());
    let mut joins: Vec<usize> = Vec::new();
    let mut add_path = |from: &str, to: &str| {
        if let Some(path) = super::window_members::paths(def, from, to)
            .into_iter()
            .next()
        {
            for j in path {
                if !joins.contains(&j) {
                    joins.push(j);
                }
            }
        }
    };
    for t in tables {
        add_path(&home.alias, t);
    }
    for text in texts {
        for (from, other) in super::window_members::hops(def, base, &home.alias, text) {
            add_path(&from, &other);
        }
    }
    let alias = quote_stored_ident(&home.alias);
    let mut sql = format!("FROM {} AS {alias}", table_source(home, def));
    // Walked from the home table, each join adds the table at its far end.
    let mut present = vec![normalize_ident_part(&home.alias)];
    for j in joins {
        let join = &def.joins[j];
        let forward = present.contains(&normalize_ident_part(&join.from_alias));
        let added = if forward {
            &join.table
        } else {
            &join.from_alias
        };
        let Some(other) = def.tables.iter().find(|o| ident_matches(&o.alias, added)) else {
            continue;
        };
        let kind = if forward && join.asof.is_some() {
            "ASOF LEFT JOIN"
        } else {
            "LEFT JOIN"
        };
        let _ = write!(
            sql,
            "\n{kind} {} AS {} ON {}",
            table_source(other, def),
            quote_stored_ident(&other.alias),
            synthesize_on_clause(join, &def.tables)
        );
        present.push(normalize_ident_part(added));
    }
    sql
}

/// The table an `INCLUDE` or `EXCLUDE` expression whose home table is `home`
/// is computed from when a query lists its rows from a table below that home
/// (TECH-DEBT #115): the table a dimensions-only query of `grain_tables` (the
/// query's dimension and filter tables) lists from, when it is not `home` and
/// references `home` along foreign keys by one path. `None` when the query's
/// rows are listed from the home or a table above it, or from no one table.
///
/// Snowflake computes such an expression over that table's rows, each joined
/// up to the home table, and lists the query's groups from the same rows
/// (fixture 14c's plans P1-P7): an order with two returns in one group counts
/// twice, and orders with no return make no group.
fn child_root<'d>(
    def: &'d SemanticViewDefinition,
    grain_tables: &[String],
    home: &str,
) -> Option<&'d TableRef> {
    if grain_tables.is_empty() {
        return None;
    }
    let super::sql_gen::Anchor::At(anchor) = super::sql_gen::member_anchor(def, grain_tables)
    else {
        return None;
    };
    if ident_matches(&anchor, home)
        || crate::graph::chain_paths(def, &anchor, home, &|_| true).len() != 1
    {
        return None;
    }
    def.tables.iter().find(|t| ident_matches(&t.alias, &anchor))
}

/// Whether `from` is `to` or references it along foreign keys by one path.
fn reached_by_one_path(def: &SemanticViewDefinition, from: &str, to: &str) -> bool {
    ident_matches(from, to) || crate::graph::chain_paths(def, from, to, &|_| true).len() == 1
}

/// The tables an `INCLUDE` or `EXCLUDE` expression read from a table below
/// its home must reach (#115): the tables its aggregate reads, and for
/// `INCLUDE` those of its own dimensions, which are in its grain (an
/// `EXCLUDE` one's are not). `query_grain` and `lod_grain_allows` both check
/// them, so a dimension `SHOW ... FOR METRIC` lists is one a query answers.
fn own_tables<'a>(
    def: &'a SemanticViewDefinition,
    kind: LodKind,
    res: &'a Resolution,
) -> impl Iterator<Item = String> + 'a {
    res.tables.iter().cloned().chain(
        res.keys
            .iter()
            .filter(move |_| kind == LodKind::Include)
            .filter_map(|&k| member_table(def, k).map(str::to_string)),
    )
}

/// `name`, with `_` appended while `taken` says it is in use.
fn unique(mut name: String, taken: impl Fn(&str) -> bool) -> String {
    while taken(&name) {
        name.push('_');
    }
    name
}

/// `def` with every LOD expression read through a hidden table of its own
/// (see the module docs); borrowed unchanged when there is none, or when one
/// breaks a rule `CREATE` checks (`graph::validate_lod`), which only a
/// definition that skipped `CREATE` can.
///
/// With no query, an `INCLUDE` expression is read at its listed dimensions
/// and an `EXCLUDE` one over every row, the grains a query with no
/// dimensions gives them: what `CREATE`'s checks and `SHOW SEMANTIC
/// DIMENSIONS ... FOR METRIC` see. A query lowers them at its own grain
/// ([`lower_lod_at`]).
#[must_use]
pub fn lower_lod(def: &SemanticViewDefinition) -> Cow<'_, SemanticViewDefinition> {
    lower_lod_at(def, &QueryGrain::default())
}

/// `def` as a query with no dimensions reads its LOD expressions, or `None`
/// when it has none: the steps `expand` takes ahead of [`lower_lod_at`]
/// (many-to-many relationships bridged, bare columns qualified, dimension
/// references and window members lowered, each window member read as a query
/// naming every fact would read it), then [`lower_lod`]. Used to type and
/// bind an `INCLUDE` or `EXCLUDE` fact, which no query can name in `FACTS`
/// (v0.20.0 review F2).
#[must_use]
pub fn lod_lowered_for_probe(def: &SemanticViewDefinition) -> Option<SemanticViewDefinition> {
    if !has_lod(def) {
        return None;
    }
    let bridged = super::many_to_many::lower_many_to_many(def);
    let qualified = super::bare_columns::qualify_bare_columns(&bridged);
    let refs = super::dim_refs::lower_dimension_references(&qualified);
    let every_fact = super::types::QueryRequest {
        facts: refs.facts.iter().map(|f| f.name.as_str().into()).collect(),
        ..super::types::QueryRequest::default()
    };
    let windowed = super::window_members::lower_window_members(&refs, &every_fact);
    Some(lower_lod(&windowed).into_owned())
}

/// What an `INCLUDE` or `EXCLUDE` expression's grain depends on in a query
/// (Phase 8.5b): the dimensions it groups by and the filter it applies
/// first. Snowflake computes `INCLUDE DIMENSIONS (d)` at the query's
/// dimensions plus `d`, and `EXCLUDE DIMENSIONS (d)` at them minus `d`, over
/// the rows the query's `WHERE` keeps (fixture 11 L5-L11, L17, 11b E1-E6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct QueryGrain {
    /// The query's dimensions, as resolved against the definition lowered.
    pub(super) dims: Vec<Resolved>,
    /// The query's `WHERE`, as the expansion reads it.
    pub(super) filter: Option<String>,
    /// The tables that `WHERE` reads.
    pub(super) filter_tables: Vec<String>,
}

/// [`lower_lod`] at a query's grain: each `INCLUDE` expression is computed
/// per tuple of the query's dimensions and its own, each `EXCLUDE` one per
/// tuple of the query's dimensions but its own, both over the rows `grain`'s
/// filter keeps. An `INCLUDE` table is read from its home table, as a `FIXED`
/// one is: each finer tuple once per group of the rows it covers. An
/// `EXCLUDE` one is read from the table a query of the same dimensions and
/// filter with no metrics lists its rows from (`sql_gen::member_anchor`), so
/// each such group, rows or not, finds the value of the coarser tuple it
/// falls in (11b E5: a customer with no orders shows the region's total).
#[must_use]
#[allow(clippy::too_many_lines)]
pub(super) fn lower_lod_at<'d>(
    def: &'d SemanticViewDefinition,
    grain: &QueryGrain,
) -> Cow<'d, SemanticViewDefinition> {
    if !has_lod(def) {
        return Cow::Borrowed(def);
    }
    let Ok((lods, metric_parts)) = collect_lods(def) else {
        return Cow::Borrowed(def);
    };
    let plain_facts: Vec<Fact> = def
        .facts
        .iter()
        .filter(|f| !is_lod_fact(f))
        .cloned()
        .collect();
    // A fact or dimension as the SQL it computes, facts inlined.
    let member_sql = |r: Resolved| -> Option<String> {
        let expr = match r {
            Resolved::Fact(i) => &def.facts[i].expr,
            Resolved::Dimension(i) => &def.dimensions[i].expr,
            _ => return None,
        };
        Some(format!(
            "({})",
            super::facts::inline_dimension_facts(expr, &plain_facts)
        ))
    };
    // An LOD expression's body with each member it names spliced in.
    let body_sql = |body: &str| -> String {
        let mut out = String::with_capacity(body.len());
        let mut copied = 0;
        for r in scan_references(body) {
            if let Some(sql) = member_sql(resolve(def, &r)) {
                out.push_str(&body[copied..r.start]);
                out.push_str(&sql);
                copied = r.end;
            }
        }
        out.push_str(&body[copied..]);
        out
    };
    let mut out = def.clone();
    let member_taken = |n: &str, d: &SemanticViewDefinition| {
        d.facts
            .iter()
            .map(|f| &f.name)
            .chain(d.dimensions.iter().map(|x| &x.name))
            .chain(d.metrics.iter().map(|m| &m.name))
            .any(|m| ident_matches(m, n))
    };
    // Each LOD expression's hidden table, by index into `lods`.
    let mut tables: Vec<Option<String>> = vec![None; lods.len()];
    // An EXCLUDE expression's twin table, read from its home table.
    let mut twins: Vec<Option<String>> = vec![None; lods.len()];
    // The key columns each table gains: (table alias, column, SQL, the
    // tables the SQL reads).
    let mut keys: Vec<(String, String, String, Vec<String>)> = Vec::new();
    // The query's filter, with each member it names spliced in.
    let filter_sql = grain.filter.as_deref().map(body_sql);
    // The table an EXCLUDE expression is read from: the one the query's
    // dimensions are listed from.
    let grain_tables: Vec<String> = grain
        .dims
        .iter()
        .filter_map(|&d| member_table(def, d).map(str::to_ascii_lowercase))
        .chain(grain.filter_tables.iter().map(|t| t.to_ascii_lowercase()))
        .collect();
    // With no dimensions, every row is the one group, read from the home.
    let anchor = if grain.dims.is_empty() {
        super::sql_gen::Anchor::Undecided
    } else {
        super::sql_gen::member_anchor(def, &grain_tables)
    };
    let listed_from = match anchor {
        super::sql_gen::Anchor::At(t) => def
            .tables
            .iter()
            .find(|x| ident_matches(&x.alias, &t))
            .map(|x| x.alias.clone()),
        _ => None,
    };
    for (n, lod) in lods.iter().enumerate() {
        let Ok(res) = resolve_lod(def, lod) else {
            return Cow::Borrowed(def);
        };
        let Some(home) = def
            .tables
            .iter()
            .find(|t| ident_matches(&t.alias, &res.home))
        else {
            return Cow::Borrowed(def);
        };
        // Read from a table below its home, an INCLUDE or EXCLUDE expression
        // is computed over that table's rows and read from it (#115).
        let below = match lod.kind {
            LodKind::Fixed => None,
            LodKind::Include | LodKind::Exclude => child_root(def, &grain_tables, &home.alias),
        };
        // Its grain: its own dimensions, or the query's with them or without.
        let mut grain_keys: Vec<Resolved> = Vec::new();
        let (attach, filter) = match lod.kind {
            LodKind::Fixed => {
                grain_keys.clone_from(&res.keys);
                (home, None)
            }
            LodKind::Include => {
                grain_keys.clone_from(&grain.dims);
                for k in &res.keys {
                    if !grain_keys.contains(k) {
                        grain_keys.push(*k);
                    }
                }
                (below.unwrap_or(home), filter_sql.as_deref())
            }
            LodKind::Exclude => {
                grain_keys = grain
                    .dims
                    .iter()
                    .filter(|d| !res.keys.contains(d))
                    .copied()
                    .collect();
                // Below its home, the groups are the child table's rows even
                // when only the filter names it (no dimensions).
                let at = below.unwrap_or_else(|| {
                    listed_from
                        .as_deref()
                        .and_then(|a| def.tables.iter().find(|t| ident_matches(&t.alias, a)))
                        .unwrap_or(home)
                });
                (at, filter_sql.as_deref())
            }
        };
        let key_tables: Vec<String> = grain_keys
            .iter()
            .filter_map(|&k| member_table(def, k).map(str::to_string))
            .collect();
        let parts: Vec<String> = grain_keys.iter().filter_map(|&k| member_sql(k)).collect();
        let key_sql = if parts.is_empty() {
            "0".to_string()
        } else {
            format!("ROW({})", parts.join(", "))
        };
        let body = body_sql(&lod.body);
        let mut read: Vec<String> = res.tables.clone();
        read.extend(key_tables.iter().cloned());
        let mut texts: Vec<&str> = vec![&key_sql, &body];
        texts.extend(filter);
        // Computed over the home table's rows, or over the rows of the table
        // below it that the query lists from, each joined up to the home.
        let source = match below {
            Some(b) => {
                read.push(home.alias.clone());
                b
            }
            None => home,
        };
        let from = joined_from(def, source, &read, &texts);
        let filter_clause = filter.map(|f| format!("\nWHERE {f}")).unwrap_or_default();
        let name = match lod.origin {
            Origin::Fact(i) => def.facts[i].name.clone(),
            Origin::Dimension(i) => def.dimensions[i].name.clone(),
            Origin::Metric { metric, .. } => def.metrics[metric].name.clone(),
        };
        // An EXCLUDE expression read from another table than its home is
        // read from its home too, through a twin of its table, so that the
        // groups of rows with no row there (orders with no customer row)
        // find their value as well (11b E5).
        // Read from a table below its home, the groups are that table's rows,
        // so no twin is needed.
        let mut attachments = vec![attach];
        if below.is_none() && !ident_matches(&attach.alias, &home.alias) {
            attachments.push(home);
        }
        let mut aliases: Vec<String> = Vec::new();
        for (t, at) in attachments.into_iter().enumerate() {
            let suffix = if t == 0 {
                String::new()
            } else {
                format!("_{t}")
            };
            let alias = unique(format!("{TABLE_PREFIX}{n}{suffix}"), |a| {
                out.tables.iter().any(|t| ident_matches(&t.alias, a))
            });
            let key = unique(format!("__sv_lodk_{n}{suffix}"), |c| {
                def.table_columns
                    .iter()
                    .find(|(t, _)| ident_matches(t, &at.alias))
                    .is_some_and(|(_, cols)| cols.iter().any(|x| ident_matches(x, c)))
            });
            let query = format!(
                "SELECT {key_sql} AS {}, {body} AS {}\n{from}{filter_clause}\nGROUP BY 1",
                quote_ident(&key),
                quote_ident(VALUE_COLUMN)
            );
            out.tables.push(TableRef {
                alias: alias.clone(),
                table: alias.clone(),
                definition: Some(query),
                pk_columns: vec![key.clone()],
                ..TableRef::default()
            });
            if !out.table_columns.is_empty() {
                out.table_columns
                    .insert(alias.clone(), vec![key.clone(), VALUE_COLUMN.to_string()]);
            }
            // One relationship name per hidden table, also for a metric with
            // several inline LOD expressions, unique beside the view's own
            // names.
            let mut rel = format!("{name} (level of detail)");
            let mut nth = 1;
            while out
                .joins
                .iter()
                .any(|j| j.name.as_deref().is_some_and(|n| ident_matches(n, &rel)))
            {
                nth += 1;
                rel = format!("{name} (level of detail {nth})");
            }
            out.joins.push(Join {
                name: Some(rel),
                from_alias: at.alias.clone(),
                table: alias.clone(),
                fk_columns: vec![key.clone()],
                ref_columns: vec![key.clone()],
                cardinality: Cardinality::ManyToOne,
                generated_name: true,
                ..Join::default()
            });
            keys.push((at.alias.clone(), key, key_sql.clone(), key_tables.clone()));
            aliases.push(alias);
        }
        let alias = aliases.remove(0);
        twins[n] = aliases.pop();
        let column = format!("{}.{}", quote_ident(&alias), quote_ident(VALUE_COLUMN));
        match lod.origin {
            Origin::Fact(i) => {
                out.facts[i].expr = column;
                out.facts[i].source_table = Some(alias.clone());
            }
            Origin::Dimension(i) => {
                out.dimensions[i].expr = column;
                out.dimensions[i].source_table = Some(alias.clone());
            }
            Origin::Metric { .. } => {}
        }
        tables[n] = Some(alias);
    }
    // Each table a hidden table is read from adds its key columns,
    // computed over its rows.
    let mut homes: Vec<String> = Vec::new();
    for (home, _, _, _) in &keys {
        if !homes.iter().any(|h| ident_matches(h, home)) {
            homes.push(home.clone());
        }
    }
    for home in homes {
        let Some(t) = def.tables.iter().find(|t| ident_matches(&t.alias, &home)) else {
            continue;
        };
        let columns: Vec<(&str, &str)> = keys
            .iter()
            .filter(|(h, _, _, _)| ident_matches(h, &home))
            .map(|(_, c, s, _)| (c.as_str(), s.as_str()))
            .collect();
        let texts: Vec<&str> = columns.iter().map(|(_, s)| *s).collect();
        let key_tables: Vec<String> = keys
            .iter()
            .filter(|(h, _, _, _)| ident_matches(h, &home))
            .flat_map(|(_, _, _, t)| t.iter().cloned())
            .collect();
        let alias = quote_stored_ident(&t.alias);
        let mut sql = format!("SELECT {alias}.*");
        for (c, s) in &columns {
            let _ = write!(sql, ",\n    {s} AS {}", quote_ident(c));
        }
        let _ = write!(sql, "\n{}", joined_from(def, t, &key_tables, &texts));
        if let Some(t) = out
            .tables
            .iter_mut()
            .find(|x| ident_matches(&x.alias, &home))
        {
            t.definition = Some(sql);
        }
        if let Some(cols) = out
            .table_columns
            .iter_mut()
            .find(|(a, _)| ident_matches(a, &home))
            .map(|(_, c)| c)
        {
            cols.extend(columns.iter().map(|(c, _)| (*c).to_string()));
        }
    }
    // Each LOD metric: a derived metric over one hidden metric per aggregate
    // call.
    let mut next = 0;
    for (mi, parts) in &metric_parts {
        let m = &def.metrics[*mi];
        let mut rewritten = String::with_capacity(m.expr.len());
        let mut copied = 0;
        let mut hidden: Vec<Metric> = Vec::new();
        for (p, part) in parts.iter().enumerate() {
            let call = &m.expr[part.start..part.end];
            // The hidden metric's expression and table, and its twin's table
            // for an EXCLUDE expression read from two tables.
            let (expr, table, twin) = match &part.lod {
                Some(PartLod::Inline { start, end, .. }) => {
                    let Some(n) = lods.iter().position(|l| {
                        l.origin
                            == Origin::Metric {
                                metric: *mi,
                                part: p,
                            }
                    }) else {
                        return Cow::Borrowed(def);
                    };
                    let Some(alias) = tables[n].clone() else {
                        return Cow::Borrowed(def);
                    };
                    let column = format!("{}.{}", quote_ident(&alias), quote_ident(VALUE_COLUMN));
                    let expr = format!(
                        "{}{column}{}",
                        &m.expr[part.start..*start],
                        &m.expr[*end..part.end]
                    );
                    (expr, alias, twins[n].clone())
                }
                Some(PartLod::Named { start, end, key }) => {
                    let Some(n) = lods.iter().position(|l| {
                        let name = match l.origin {
                            Origin::Fact(i) => &def.facts[i].name,
                            Origin::Dimension(i) => &def.dimensions[i].name,
                            Origin::Metric { .. } => return false,
                        };
                        normalize_ident_part(name) == *key
                    }) else {
                        return Cow::Borrowed(def);
                    };
                    let Some(alias) = tables[n].clone() else {
                        return Cow::Borrowed(def);
                    };
                    let column = format!("{}.{}", quote_ident(&alias), quote_ident(VALUE_COLUMN));
                    // Every read of the LOD expression in the call, the first at
                    // `start..end`.
                    let mut spans: Vec<(usize, usize)> = crate::graph::lod::names(&m.expr)
                        .into_iter()
                        .filter(|r| {
                            part.start <= r.start
                                && r.end <= part.end
                                && r.key() == *key
                                && resolve(def, r) == Resolved::Lod
                        })
                        .map(|r| (r.start, r.end))
                        .collect();
                    if !spans.contains(&(*start, *end)) {
                        spans.push((*start, *end));
                    }
                    spans.sort_unstable();
                    let mut expr = String::new();
                    let mut at = part.start;
                    for (s, e) in spans {
                        expr.push_str(&m.expr[at..s]);
                        expr.push_str(&column);
                        at = e;
                    }
                    expr.push_str(&m.expr[at..part.end]);
                    (expr, alias, twins[n].clone())
                }
                None => {
                    let Some(table) = scan_references(call)
                        .iter()
                        .find_map(|r| member_table(def, resolve(def, r)).map(str::to_string))
                    else {
                        return Cow::Borrowed(def);
                    };
                    (call.to_string(), table, None)
                }
            };
            let mut names: Vec<String> = Vec::new();
            let twin_expr = twin.as_ref().map(|t| {
                let column = |a: &str| format!("{}.{}", quote_ident(a), quote_ident(VALUE_COLUMN));
                (expr.replace(&column(&table), &column(t)), t.clone())
            });
            for (expr, table) in std::iter::once((expr, table)).chain(twin_expr) {
                let name = unique(format!("__sv_lodm_{next}"), |n| {
                    member_taken(n, &out) || hidden.iter().any(|h| ident_matches(&h.name, n))
                });
                next += 1;
                names.push(quote_ident(&name));
                hidden.push(Metric {
                    name,
                    expr,
                    source_table: Some(table),
                    access: AccessModifier::Private,
                    ..Metric::default()
                });
            }
            rewritten.push_str(&m.expr[copied..part.start]);
            // Read from both tables, a group finds the value either gives:
            // the same value, where both list the group.
            if let [one] = names.as_slice() {
                rewritten.push_str(one);
            } else {
                let _ = write!(rewritten, "COALESCE({})", names.join(", "));
            }
            copied = part.end;
        }
        rewritten.push_str(&m.expr[copied..]);
        out.metrics[*mi].expr = rewritten;
        out.metrics.extend(hidden);
    }
    Cow::Owned(out)
}

/// The grain a query gives the `INCLUDE` and `EXCLUDE` expressions it reads
/// (see [`QueryGrain`]), after the rules Snowflake checks for them in a query.
/// `def` is the definition [`lower_lod_at`] lowers, and `req` the query as
/// the expansion reads it.
///
/// # Errors
///
/// [`ExpandError::LodRule`](super::types::ExpandError::LodRule) when the
/// query reads such an expression:
///
/// - named in `DIMENSIONS`, `FACTS` or `WHERE`, where Snowflake reads it only
///   in `METRICS`, inside an aggregate (fixture 11 L26: 010969);
/// - beside an expression item in `DIMENSIONS` (L28: 010970);
/// - grouped or filtered by an LOD expression, which is computed over rows
///   of its own;
/// - grouped or filtered by a table its aggregated table does not reach along
///   foreign keys by one path, so its rows can't be grouped by it.
#[allow(clippy::too_many_lines)]
pub(super) fn query_grain(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &super::types::QueryRequest,
) -> Result<QueryGrain, super::types::ExpandError> {
    let refuse = |reason: String| super::types::ExpandError::LodRule {
        view_name: view_name.to_string(),
        reason,
    };
    if !has_lod(def) {
        return Ok(QueryGrain::default());
    }
    let Ok((lods, _)) = collect_lods(def) else {
        return Ok(QueryGrain::default());
    };
    let needed = super::window_members::needed(def, req);
    let name_of = |lod: &crate::graph::lod::Lod| match lod.origin {
        Origin::Fact(i) => &def.facts[i].name,
        Origin::Dimension(i) => &def.dimensions[i].name,
        Origin::Metric { metric, .. } => &def.metrics[metric].name,
    };
    let read: Vec<&crate::graph::lod::Lod> = lods
        .iter()
        .filter(|l| l.kind != LodKind::Fixed && needed.contains(&normalize_ident_part(name_of(l))))
        .collect();
    let Some(first) = read.first() else {
        return Ok(QueryGrain::default());
    };
    let first_what = first.describe(def);
    let first_kind = first.kind.keyword();
    // Named in DIMENSIONS, FACTS or WHERE.
    let named = |text: &str| -> Option<&crate::graph::lod::Lod> {
        scan_references(text).iter().find_map(|r| {
            read.iter().copied().find(|l| {
                !matches!(l.origin, Origin::Metric { .. })
                    && r.is_bare()
                    && r.key() == normalize_ident_part(name_of(l))
            })
        })
    };
    let clauses = [
        (
            "DIMENSIONS",
            req.dimensions
                .iter()
                .map(super::types::CiName::as_str)
                .collect::<Vec<_>>(),
        ),
        (
            "FACTS",
            req.facts.iter().map(super::types::CiName::as_str).collect(),
        ),
        (
            "WHERE",
            req.where_clause.iter().map(String::as_str).collect(),
        ),
    ];
    for (clause, texts) in &clauses {
        for text in texts {
            if let Some(l) = named(text) {
                return Err(refuse(format!(
                    "{} uses {} DIMENSIONS, so it is computed at the query's grain, and can be \
                     read only in METRICS, as the argument of an aggregate function (such as \
                     AVG({})), not in {clause}.",
                    l.describe(def),
                    l.kind.keyword(),
                    name_of(l)
                )));
            }
        }
    }
    // The query's dimensions.
    let mut dims: Vec<Resolved> = Vec::new();
    for d in &req.dimensions {
        let refs = scan_references(d.as_str());
        let [r] = refs.as_slice() else {
            continue;
        };
        match resolve(def, r) {
            Resolved::Dimension(i) => {
                if def.dimensions[i].name.starts_with("__sv_expr_") {
                    return Err(refuse(format!(
                        "{first_what} uses {first_kind} DIMENSIONS, so it is computed at the \
                         query's dimensions, and can't be beside an expression in DIMENSIONS. \
                         Group by dimensions the view defines, or define the expression as a \
                         dimension."
                    )));
                }
                let k = Resolved::Dimension(i);
                if !dims.contains(&k) {
                    dims.push(k);
                }
            }
            Resolved::Lod => {
                return Err(refuse(format!(
                    "{first_what} uses {first_kind} DIMENSIONS, so it is computed at the \
                     query's dimensions, and can't be grouped by LOD dimension '{}', which is \
                     computed at a grain of its own.",
                    d.as_str()
                )));
            }
            _ => {}
        }
    }
    // The tables the filter reads, and an LOD expression it reads.
    let mut filter_tables: Vec<String> = Vec::new();
    if let Some(w) = req.where_clause.as_deref() {
        for r in scan_references(w) {
            match resolve(def, &r) {
                Resolved::Lod => {
                    return Err(refuse(format!(
                        "{first_what} uses {first_kind} DIMENSIONS, so it is computed over the \
                         rows the query's WHERE keeps, and that WHERE reads LOD expression '{}', \
                         which is computed at a grain of its own.",
                        r.raw
                    )));
                }
                resolved @ (Resolved::Fact(_) | Resolved::Dimension(_)) => {
                    if let Some(t) = member_table(def, resolved) {
                        filter_tables.push(t.to_string());
                    }
                }
                _ => {
                    if r.is_bare() {
                        continue;
                    }
                    let q = r.first_part_key();
                    if let Some(t) = def
                        .tables
                        .iter()
                        .find(|t| normalize_ident_part(&t.alias) == q)
                    {
                        filter_tables.push(t.alias.clone());
                    }
                }
            }
        }
    }
    // The tables the query lists its rows from.
    let grain_tables: Vec<String> = dims
        .iter()
        .filter_map(|&d| member_table(def, d).map(str::to_ascii_lowercase))
        .chain(filter_tables.iter().map(|t| t.to_ascii_lowercase()))
        .collect();
    // Every table its grain and filter read is reached from its home by one
    // path, or, when the query lists its rows from a table below the home,
    // from that table, which also reaches the home and the expression's own
    // tables (#115).
    for lod in &read {
        let Ok(res) = resolve_lod(def, lod) else {
            continue;
        };
        let below = child_root(def, &grain_tables, &res.home);
        let root = below.map_or(res.home.as_str(), |b| b.alias.as_str());
        let grouped = dims.iter().filter(|d| match lod.kind {
            LodKind::Exclude => !res.keys.contains(d),
            _ => true,
        });
        let own = below.map(|_| own_tables(def, lod.kind, &res));
        let tables = grouped
            .filter_map(|&d| member_table(def, d).map(str::to_string))
            .chain(filter_tables.iter().cloned())
            .chain(own.into_iter().flatten());
        for t in tables {
            if !reached_by_one_path(def, root, &t) {
                return Err(refuse(format!(
                    "{} uses {} DIMENSIONS, so it aggregates the rows of table '{}' at the \
                     query's grain, but the query groups or filters by table '{t}', which \
                     '{}' does not reference along foreign keys by one path.",
                    lod.describe(def),
                    lod.kind.keyword(),
                    res.home,
                    root
                )));
            }
        }
    }
    Ok(QueryGrain {
        dims,
        filter: req.where_clause.clone().filter(|w| !w.trim().is_empty()),
        filter_tables,
    })
}

/// Whether a query of `metric` can be grouped by `dim` as far as the
/// `INCLUDE` and `EXCLUDE` expressions it reads go ([`query_grain`]'s rules):
/// `dim` is no LOD dimension, and every such expression's aggregated table
/// reaches its table along foreign keys by one path, unless an `EXCLUDE`
/// expression leaves it out of its grain. `def` is the definition as
/// `SHOW SEMANTIC DIMENSIONS ... FOR METRIC` lowers it, before [`lower_lod`].
#[must_use]
pub fn lod_grain_allows(def: &SemanticViewDefinition, metric: &str, dim: &str) -> bool {
    if !has_lod(def) {
        return true;
    }
    let Ok((lods, _)) = collect_lods(def) else {
        return true;
    };
    let req = super::types::QueryRequest {
        metrics: vec![super::types::CiName::new(metric)],
        ..super::types::QueryRequest::default()
    };
    let needed = super::window_members::needed(def, &req);
    let Some(d) = def
        .dimensions
        .iter()
        .position(|x| ident_matches(&x.name, dim))
    else {
        return true;
    };
    for lod in &lods {
        let name = match lod.origin {
            Origin::Fact(i) => &def.facts[i].name,
            Origin::Dimension(i) => &def.dimensions[i].name,
            Origin::Metric { metric, .. } => &def.metrics[metric].name,
        };
        if lod.kind == LodKind::Fixed || !needed.contains(&normalize_ident_part(name)) {
            continue;
        }
        if crate::graph::lod::is_lod_dimension(&def.dimensions[d]) {
            return false;
        }
        let Ok(res) = resolve_lod(def, lod) else {
            continue;
        };
        if lod.kind == LodKind::Exclude && res.keys.contains(&Resolved::Dimension(d)) {
            continue;
        }
        let Some(t) = def.dimensions[d].source_table.as_deref() else {
            continue;
        };
        if reached_by_one_path(def, &res.home, t) {
            continue;
        }
        // A table below the home: grouped by it alone, the expression is
        // read from its rows (#115), so they reach the home and the
        // expression's own tables.
        let grain_tables = [t.to_ascii_lowercase()];
        let Some(below) = child_root(def, &grain_tables, &res.home) else {
            return false;
        };
        if !own_tables(def, lod.kind, &res).all(|o| reached_by_one_path(def, &below.alias, &o)) {
            return false;
        }
    }
    true
}

/// Refuse a query that reads an LOD expression and reaches across a
/// many-to-many relationship, as Snowflake does (010959, fixture 11b M16q,
/// M16c and M16d): it reads members, or an LOD expression's tables, on both
/// sides of one. `def` is the definition as stored, before
/// `lower_many_to_many`.
///
/// # Errors
///
/// [`ExpandError::LodAcrossManyToMany`], naming the relationship.
pub(super) fn check_lod_paths(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &super::types::QueryRequest,
) -> Result<(), super::types::ExpandError> {
    let many: Vec<&Join> = def
        .joins
        .iter()
        .filter(|j| j.cardinality == Cardinality::ManyToMany)
        .collect();
    if many.is_empty() || !has_lod(def) {
        return Ok(());
    }
    let needed = super::window_members::needed(def, req);
    let lod_keys = crate::graph::lod::lod_member_keys(def);
    let reads_lod = needed.iter().any(|n| lod_keys.contains(n))
        || def.metrics.iter().any(|m| {
            needed.contains(&normalize_ident_part(&m.name))
                && crate::graph::lod::is_lod_metric(m, &lod_keys)
        });
    if !reads_lod {
        return Ok(());
    }
    let mut tables: Vec<String> = Vec::new();
    let mut add = |t: &str| {
        if !tables.iter().any(|x| ident_matches(x, t)) {
            tables.push(t.to_string());
        }
    };
    let members = def
        .facts
        .iter()
        .map(|f| (&f.name, &f.source_table))
        .chain(def.dimensions.iter().map(|d| (&d.name, &d.source_table)))
        .chain(def.metrics.iter().map(|m| (&m.name, &m.source_table)));
    for (name, table) in members {
        if let Some(t) = table.as_deref() {
            if needed.contains(&normalize_ident_part(name)) {
                add(t);
            }
        }
    }
    let texts = req
        .dimensions
        .iter()
        .map(super::types::CiName::as_str)
        .chain(req.metrics.iter().map(super::types::CiName::as_str))
        .chain(req.facts.iter().map(super::types::CiName::as_str))
        .chain(req.where_clause.as_deref());
    for text in texts {
        for r in scan_references(text) {
            if r.is_bare() {
                continue;
            }
            let q = r.first_part_key();
            if let Some(t) = def
                .tables
                .iter()
                .find(|t| normalize_ident_part(&t.alias) == q)
            {
                add(&t.alias.clone());
            }
        }
    }
    for j in many {
        let has = |a: &str| tables.iter().any(|t| ident_matches(t, a));
        if has(&j.from_alias) && has(&j.table) {
            return Err(super::types::ExpandError::LodAcrossManyToMany {
                view_name: view_name.to_string(),
                relationship: j.name.clone().unwrap_or_else(|| j.table.clone()),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dimension, Metric};

    fn def() -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![
                TableRef {
                    alias: "customers".into(),
                    table: "customers".into(),
                    pk_columns: vec!["customer_id".into()],
                    ..Default::default()
                },
                TableRef {
                    alias: "orders".into(),
                    table: "orders".into(),
                    pk_columns: vec!["order_id".into()],
                    ..Default::default()
                },
            ],
            joins: vec![Join {
                from_alias: "orders".into(),
                table: "customers".into(),
                fk_columns: vec!["customer_id".into()],
                ref_columns: vec!["customer_id".into()],
                name: Some("oc".into()),
                ..Default::default()
            }],
            facts: vec![
                Fact {
                    name: "order_amount".into(),
                    expr: "orders.amount".into(),
                    source_table: Some("orders".into()),
                    ..Default::default()
                },
                Fact {
                    name: "opc".into(),
                    expr: "COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id)".into(),
                    ..Default::default()
                },
            ],
            dimensions: vec![
                Dimension {
                    name: "customer_id".into(),
                    expr: "customers.customer_id".into(),
                    source_table: Some("customers".into()),
                    ..Default::default()
                },
                Dimension {
                    name: "order_id".into(),
                    expr: "orders.order_id".into(),
                    source_table: Some("orders".into()),
                    ..Default::default()
                },
            ],
            metrics: vec![Metric {
                name: "share".into(),
                expr:
                    "SUM(orders.order_amount) / AVG(SUM(orders.order_amount) FIXED DIMENSIONS ())"
                        .into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn include_def() -> SemanticViewDefinition {
        let mut d = def();
        d.metrics[0].expr =
            "AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id))".into();
        d
    }

    /// A blank `where_clause` is no filter: the hidden table of an INCLUDE
    /// expression gets no bare `WHERE` (review of #369; `expand_unhoisted`
    /// already skipped a blank predicate, as `fuzz_where_predicate` found).
    #[test]
    fn a_blank_where_clause_adds_no_filter_to_an_include_table() {
        let d = include_def();
        for blank in ["", "  ", "\n\t"] {
            let req = super::super::types::QueryRequest {
                where_clause: Some(blank.to_string()),
                facts: vec![],
                dimensions: vec![super::super::types::DimensionName::new("customer_id")],
                metrics: vec![super::super::types::MetricName::new("share")],
            };
            let sql = crate::expand::expand("v", &d, &req).unwrap();
            let no_space: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                !no_space.contains("WHERE GROUP BY") && !no_space.contains("WHERE )"),
                "{blank:?} gave a bare WHERE:\n{sql}"
            );
        }
    }

    /// `SHOW SEMANTIC DIMENSIONS ... FOR METRIC` lists a dimension with no
    /// `source_table` for an INCLUDE metric, as a query grouped by it is not
    /// refused for its table either (review of #369).
    #[test]
    fn a_dimension_with_no_table_is_listed_for_an_include_metric() {
        let mut d = include_def();
        d.dimensions.push(Dimension {
            name: "seg".into(),
            expr: "customers.segment".into(),
            ..Default::default()
        });
        assert!(lod_grain_allows(&d, "share", "seg"));
        assert!(lod_grain_allows(&d, "share", "customer_id"));
    }

    /// Two inline LOD expressions in one metric each get a relationship of
    /// their own name, so the lowered copy passes `validate_graph` (review of
    /// #368: both were named `<metric> (level of detail)`).
    #[test]
    fn two_inline_lods_in_one_metric_get_distinct_relationships() {
        let mut d = def();
        d.metrics[0].expr = "AVG(COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id)) \
             / AVG(SUM(orders.order_amount) FIXED DIMENSIONS ())"
            .into();
        let out = lower_lod(&d);
        let names: Vec<&str> = out.joins.iter().filter_map(|j| j.name.as_deref()).collect();
        assert_eq!(
            names,
            vec![
                "oc",
                "opc (level of detail)",
                "share (level of detail)",
                "share (level of detail 2)"
            ]
        );
        crate::graph::validate_graph(&out).unwrap();
    }

    /// Each LOD expression gets a hidden table keyed on its fixed dimensions,
    /// referenced by its home table, and an LOD metric becomes a derived
    /// metric over one hidden metric per aggregate call.
    #[test]
    fn lod_expressions_are_read_through_hidden_tables() {
        let d = def();
        let out = lower_lod(&d);
        let aliases: Vec<&str> = out.tables.iter().map(|t| t.alias.as_str()).collect();
        assert_eq!(
            aliases,
            vec!["customers", "orders", "__sv_lod_0", "__sv_lod_1"]
        );
        let lod0 = out.tables[2].definition.as_deref().unwrap();
        assert!(
            lod0.contains("SELECT ROW((customers.customer_id)) AS \"__sv_lodk_0\", COUNT((orders.order_id)) AS \"__sv_lod\""),
            "{lod0}"
        );
        assert!(
            lod0.contains("LEFT JOIN \"customers\" AS \"customers\""),
            "{lod0}"
        );
        assert!(lod0.ends_with("GROUP BY 1"), "{lod0}");
        let lod1 = out.tables[3].definition.as_deref().unwrap();
        assert!(
            lod1.starts_with("SELECT 0 AS \"__sv_lodk_1\", SUM((orders.amount))"),
            "{lod1}"
        );
        let home = out.tables[1].definition.as_deref().unwrap();
        assert!(
            home.contains("ROW((customers.customer_id)) AS \"__sv_lodk_0\""),
            "{home}"
        );
        assert!(home.contains("0 AS \"__sv_lodk_1\""), "{home}");
        assert_eq!(out.facts[1].source_table.as_deref(), Some("__sv_lod_0"));
        assert_eq!(out.facts[1].expr, "\"__sv_lod_0\".\"__sv_lod\"");
        let joins: Vec<(&str, &str)> = out
            .joins
            .iter()
            .map(|j| (j.from_alias.as_str(), j.table.as_str()))
            .collect();
        assert_eq!(
            joins,
            vec![
                ("orders", "customers"),
                ("orders", "__sv_lod_0"),
                ("orders", "__sv_lod_1")
            ]
        );
        assert_eq!(out.metrics[0].expr, "\"__sv_lodm_0\" / \"__sv_lodm_1\"");
        assert_eq!(out.metrics[1].expr, "SUM(orders.order_amount)");
        assert_eq!(out.metrics[1].source_table.as_deref(), Some("orders"));
        assert_eq!(out.metrics[2].expr, "AVG(\"__sv_lod_1\".\"__sv_lod\")");
        assert_eq!(out.metrics[2].source_table.as_deref(), Some("__sv_lod_1"));
        assert_eq!(out.metrics[2].access, AccessModifier::Private);
    }

    #[test]
    fn a_view_without_lod_is_borrowed() {
        let mut d = def();
        d.facts.truncate(1);
        d.metrics.clear();
        assert!(matches!(lower_lod(&d), Cow::Borrowed(_)));
    }
}
