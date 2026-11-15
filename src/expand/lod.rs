//! Level-of-detail expressions (Phase 8.5a: `FIXED DIMENSIONS`).
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
//! The lowering runs after the bare-column, dimension-reference and window
//! lowerings, so the member expressions it splices are already qualified, and
//! those steps leave LOD members alone (they have no table of their own).
//! Only the working copy changes: `DESCRIBE`, `GET_DDL` and the YAML export
//! show each LOD expression as written.

use std::borrow::Cow;
use std::fmt::Write as _;

use crate::expr_tokens::scan_references;
use crate::graph::lod::{
    collect_lods, has_lod, is_lod_fact, member_table, resolve, resolve_lod, Origin, PartLod,
    Resolved,
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
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn lower_lod(def: &SemanticViewDefinition) -> Cow<'_, SemanticViewDefinition> {
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
    // The key columns each home table gains: (home alias, column, SQL).
    let mut keys: Vec<(String, String, String)> = Vec::new();
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
        let alias = unique(format!("{TABLE_PREFIX}{n}"), |a| {
            out.tables.iter().any(|t| ident_matches(&t.alias, a))
        });
        let key = unique(format!("__sv_lodk_{n}"), |c| {
            def.table_columns
                .iter()
                .find(|(t, _)| ident_matches(t, &home.alias))
                .is_some_and(|(_, cols)| cols.iter().any(|x| ident_matches(x, c)))
        });
        let parts: Vec<String> = res.keys.iter().filter_map(|&k| member_sql(k)).collect();
        let key_sql = if parts.is_empty() {
            "0".to_string()
        } else {
            format!("ROW({})", parts.join(", "))
        };
        let body = body_sql(&lod.body);
        let from = joined_from(def, home, &res.tables, &[&key_sql, &body]);
        let query = format!(
            "SELECT {key_sql} AS {}, {body} AS {}\n{from}\nGROUP BY 1",
            quote_ident(&key),
            quote_ident(VALUE_COLUMN)
        );
        let name = match lod.origin {
            Origin::Fact(i) => def.facts[i].name.clone(),
            Origin::Dimension(i) => def.dimensions[i].name.clone(),
            Origin::Metric { metric, .. } => def.metrics[metric].name.clone(),
        };
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
        out.joins.push(Join {
            name: Some(format!("{name} (level of detail)")),
            from_alias: home.alias.clone(),
            table: alias.clone(),
            fk_columns: vec![key.clone()],
            ref_columns: vec![key.clone()],
            cardinality: Cardinality::ManyToOne,
            generated_name: true,
            ..Join::default()
        });
        keys.push((home.alias.clone(), key, key_sql));
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
    // Each home table adds its key columns, computed over its rows.
    let mut homes: Vec<String> = Vec::new();
    for (home, _, _) in &keys {
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
            .filter(|(h, _, _)| ident_matches(h, &home))
            .map(|(_, c, s)| (c.as_str(), s.as_str()))
            .collect();
        let texts: Vec<&str> = columns.iter().map(|(_, s)| *s).collect();
        let key_tables: Vec<String> = lods
            .iter()
            .filter_map(|lod| resolve_lod(def, lod).ok())
            .filter(|r| ident_matches(&r.home, &home))
            .flat_map(|r| {
                r.keys
                    .iter()
                    .filter_map(|&k| member_table(def, k).map(str::to_string))
                    .collect::<Vec<_>>()
            })
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
            let (expr, table) = match &part.lod {
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
                    (expr, alias)
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
                    (expr, alias)
                }
                None => {
                    let Some(table) = scan_references(call)
                        .iter()
                        .find_map(|r| member_table(def, resolve(def, r)).map(str::to_string))
                    else {
                        return Cow::Borrowed(def);
                    };
                    (call.to_string(), table)
                }
            };
            let name = unique(format!("__sv_lodm_{next}"), |n| {
                member_taken(n, &out) || hidden.iter().any(|h| ident_matches(&h.name, n))
            });
            next += 1;
            rewritten.push_str(&m.expr[copied..part.start]);
            rewritten.push_str(&quote_ident(&name));
            copied = part.end;
            hidden.push(Metric {
                name,
                expr,
                source_table: Some(table),
                access: AccessModifier::Private,
                ..Metric::default()
            });
        }
        rewritten.push_str(&m.expr[copied..]);
        out.metrics[*mi].expr = rewritten;
        out.metrics.extend(hidden);
    }
    Cow::Owned(out)
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
