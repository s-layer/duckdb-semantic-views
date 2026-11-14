//! Window functions in facts and dimensions (Phase 8.3).
//!
//! Snowflake lets a fact or dimension hold a window function
//! (`sales.units_rank AS ROW_NUMBER() OVER (ORDER BY units)`), and computes it
//! over every row of the member's table before the query's `WHERE` (fixture 11
//! part W: a filter on the category leaves the ranks of the whole table). A
//! metric may aggregate a window fact, and a query may group by or filter on a
//! window member.
//!
//! Spliced into the query as written, such a member did neither: its window
//! ran over the rows left after the `WHERE` and the joins, and `DuckDB` refuses
//! a window function in a `GROUP BY`, a `WHERE` or an aggregate. So the
//! expansion's working copy reads each window member as a column of its
//! table: the table becomes a derived table that adds one column per window
//! member, computed over the table's rows, and the member's expression names
//! that column. Every later step sees a row-level column.
//!
//! A window member may read a table its own table reaches along foreign keys
//! (`PARTITION BY st.region`, W6 and W9), which joins that table inside the
//! derived table, so the window still runs over one row per row of the
//! member's table. [`validate_window_members`] refuses at CREATE one that
//! reads any other table.
//!
//! A window member may read another one (a window over a window fact, or a
//! fact over one): the members are lowered in rounds, each wrapping the
//! table's derived table of the round before, so a member's window reads the
//! columns the earlier rounds added.
//!
//! A window member that reads only its own table is always lowered: `DuckDB`
//! drops a derived table's column no query reads, so it costs nothing. One
//! that reads another table joins it inside the derived table, which `DuckDB`
//! keeps, so it is lowered only for a query that may read it ([`needed`]).
//!
//! Only the working copy changes: DESCRIBE, `GET_DDL` and the YAML export show
//! each member as written.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write as _;

use crate::errors::ParseError;
use crate::expr_tokens::{has_window_call, references_ref, scan_references};
use crate::ident::{ident_matches, normalize_ident_part};
use crate::model::{Cardinality, Fact, SemanticViewDefinition};

use super::join_resolver::synthesize_on_clause;
use super::resolution::{quote_ident, quote_stored_ident, table_source};
use super::types::QueryRequest;

/// A fact or a dimension, by its index into `def.facts` or `def.dimensions`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Member {
    Fact(usize),
    Dimension(usize),
}

impl Member {
    fn expr(self, def: &SemanticViewDefinition) -> &str {
        match self {
            Self::Fact(i) => &def.facts[i].expr,
            Self::Dimension(i) => &def.dimensions[i].expr,
        }
    }

    fn table<'a>(self, def: &'a SemanticViewDefinition, base: &'a str) -> &'a str {
        let table = match self {
            Self::Fact(i) => def.facts[i].source_table.as_deref(),
            Self::Dimension(i) => def.dimensions[i].source_table.as_deref(),
        };
        table.unwrap_or(base)
    }

    fn describe(self, def: &SemanticViewDefinition) -> (&'static str, &str) {
        match self {
            Self::Fact(i) => ("fact", &def.facts[i].name),
            Self::Dimension(i) => ("dimension", &def.dimensions[i].name),
        }
    }
}

/// The facts and dimensions of `def` that hold a window function.
fn window_members(def: &SemanticViewDefinition) -> Vec<Member> {
    let facts = (0..def.facts.len())
        .filter(|&i| has_window_call(&def.facts[i].expr))
        .map(Member::Fact);
    let dims = (0..def.dimensions.len())
        .filter(|&i| has_window_call(&def.dimensions[i].expr))
        .map(Member::Dimension);
    facts.chain(dims).collect()
}

/// The facts `expr` reads, directly or through other facts, as indices into
/// `facts`.
fn reached_facts(expr: &str, facts: &[Fact]) -> HashSet<usize> {
    let mut reached = HashSet::new();
    let mut pending = vec![expr.to_string()];
    while let Some(current) = pending.pop() {
        for (i, fact) in facts.iter().enumerate() {
            if !reached.contains(&i)
                && references_ref(&current, &fact.name, fact.source_table.as_deref())
            {
                reached.insert(i);
                pending.push(fact.expr.clone());
            }
        }
    }
    reached
}

/// The tables other than `own` that `expr` reads: those its qualified names
/// name, and those of the facts it reaches and of their qualified names. Each
/// is a declared alias, as declared, in first-reached order.
fn tables_read(def: &SemanticViewDefinition, own: &str, expr: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |alias: &str| {
        if !ident_matches(alias, own) && !out.iter().any(|a| ident_matches(a, alias)) {
            out.push(alias.to_string());
        }
    };
    let mut texts: Vec<&str> = vec![expr];
    let mut reached: Vec<usize> = reached_facts(expr, &def.facts).into_iter().collect();
    reached.sort_unstable();
    for i in reached {
        if let Some(t) = def.facts[i].source_table.as_deref() {
            add(t);
        }
        texts.push(&def.facts[i].expr);
    }
    for text in texts {
        for chain in scan_references(text) {
            if chain.is_bare() {
                continue;
            }
            let qualifier = chain.first_part_key();
            if let Some(t) = def
                .tables
                .iter()
                .find(|t| normalize_ident_part(&t.alias) == qualifier)
            {
                add(&t.alias);
            }
        }
    }
    out
}

/// The ways from `from` to `to` along foreign keys, stepping back along a
/// one-to-one relationship too, each table at most once on a way: each a list
/// of indices into `def.joins`. Collecting stops at two, as
/// `graph::window_dims::chain_count` counts.
fn paths(def: &SemanticViewDefinition, from: &str, to: &str) -> Vec<Vec<usize>> {
    fn walk(
        def: &SemanticViewDefinition,
        at: &str,
        to: &str,
        seen: &mut Vec<String>,
        path: &mut Vec<usize>,
        out: &mut Vec<Vec<usize>>,
    ) {
        if ident_matches(at, to) {
            out.push(path.clone());
            return;
        }
        for (i, j) in def.joins.iter().enumerate() {
            let next = if ident_matches(&j.from_alias, at) {
                &j.table
            } else if ident_matches(&j.table, at) && j.cardinality == Cardinality::OneToOne {
                &j.from_alias
            } else {
                continue;
            };
            let key = normalize_ident_part(next);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            path.push(i);
            walk(def, next, to, seen, path, out);
            path.pop();
            seen.pop();
            if out.len() >= 2 {
                return;
            }
        }
    }
    let mut out = Vec::new();
    walk(
        def,
        from,
        to,
        &mut vec![normalize_ident_part(from)],
        &mut Vec::new(),
        &mut out,
    );
    out
}

/// Refuse at CREATE a window fact or dimension that reads a table its own
/// table does not reach along foreign keys, such as a child table (whose rows
/// give no one value per row of the member's table), or reaches two ways, as
/// a dimension's reference to another table is refused
/// (`graph::validate_dimension_references`).
///
/// # Errors
///
/// A [`ParseError`] naming the member and the table.
pub fn validate_window_members(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    let Some(base) = def.tables.first().map(|t| t.alias.clone()) else {
        return Ok(());
    };
    let lowered = super::dim_refs::lower_dimension_references(def);
    let def = &*lowered;
    for member in window_members(def) {
        let own = member.table(def, &base);
        let (kind, name) = member.describe(def);
        for other in tables_read(def, own, member.expr(def)) {
            match paths(def, own, &other).len() {
                1 => {}
                0 => {
                    return Err(ParseError::positionless(format!(
                        "{kind} '{own}.{name}' holds a window function and reads table \
                         '{other}', but table '{own}' does not reach table '{other}' along its \
                         relationships, so '{other}' has no single value per row of '{own}'. A \
                         window {kind} can read a table its own table references, directly or \
                         through other tables, not one that references it."
                    )));
                }
                _ => {
                    return Err(ParseError::positionless(format!(
                        "{kind} '{own}.{name}' holds a window function and reads table \
                         '{other}', but table '{own}' reaches table '{other}' by more than one \
                         path (for example, as two roles), so which one is meant is not said."
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The name of the column a window member is read from: `__sv_win_<n>`, with
/// `_` appended while it would match a column of the table or a member.
fn column_name(def: &SemanticViewDefinition, table: &str, n: usize) -> String {
    let mut name = format!("__sv_win_{n}");
    let columns = def
        .table_columns
        .iter()
        .find(|(alias, _)| ident_matches(alias, table))
        .map_or(&[][..], |(_, cols)| cols.as_slice());
    let taken = |c: &str| {
        columns
            .iter()
            .chain(def.facts.iter().map(|f| &f.name))
            .chain(def.dimensions.iter().map(|d| &d.name))
            .any(|m| ident_matches(m, c))
    };
    while taken(&name) {
        name.push('_');
    }
    name
}

/// The derived table that reads `table` with `columns` added: each a column
/// name and the window expression it holds, read over the table's rows, with
/// the tables the expressions read joined along their one path.
fn derived_table(
    def: &SemanticViewDefinition,
    table: &str,
    columns: &[(String, String)],
) -> String {
    let Some(t) = def.tables.iter().find(|t| ident_matches(&t.alias, table)) else {
        return String::new();
    };
    let alias = quote_stored_ident(&t.alias);
    let mut joins: Vec<usize> = Vec::new();
    for (_, expr) in columns {
        for other in tables_read(def, &t.alias, expr) {
            if let Some(path) = paths(def, &t.alias, &other).into_iter().next() {
                for j in path {
                    if !joins.contains(&j) {
                        joins.push(j);
                    }
                }
            }
        }
    }
    let mut sql = format!("SELECT {alias}.*");
    for (name, expr) in columns {
        let _ = write!(sql, ",\n    {expr} AS {}", quote_ident(name));
    }
    let _ = write!(sql, "\nFROM {} AS {alias}", table_source(t, def));
    // Walked from the member's table, each join adds the table at its far end.
    let mut present = vec![normalize_ident_part(&t.alias)];
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

/// The names a query for `req` may read, each normalized: the members it
/// names, the names its `WHERE` holds, and, again and again, the names in the
/// expression of each member so reached (for a metric, also its window's and
/// its `NON ADDITIVE BY`'s). A name is taken bare: an over-approximation, so
/// a member it misses can only be one no query step reads.
fn needed(def: &SemanticViewDefinition, req: &QueryRequest) -> HashSet<String> {
    let last = |name: &str| match crate::ident::parse_qualified_identifier(name) {
        Ok(parts) => parts
            .last()
            .map_or_else(String::new, |p| normalize_ident_part(p)),
        Err(_) => normalize_ident_part(name),
    };
    let mut names: HashSet<String> = req
        .dimensions
        .iter()
        .map(|d| last(d.as_str()))
        .chain(req.metrics.iter().map(|m| last(m.as_str())))
        .chain(req.facts.iter().map(|f| last(f.as_str())))
        .collect();
    let mut texts: Vec<String> = req.where_clause.iter().cloned().collect();
    let mut done: HashSet<(u8, usize)> = HashSet::new();
    loop {
        for text in texts.drain(..) {
            for chain in scan_references(&text) {
                names.insert(chain.last_part_key());
            }
        }
        let mut grew = false;
        for (i, f) in def.facts.iter().enumerate() {
            if names.contains(&normalize_ident_part(&f.name)) && done.insert((0, i)) {
                texts.push(f.expr.clone());
                grew = true;
            }
        }
        for (i, d) in def.dimensions.iter().enumerate() {
            if names.contains(&normalize_ident_part(&d.name)) && done.insert((1, i)) {
                texts.push(d.expr.clone());
                grew = true;
            }
        }
        for (i, m) in def.metrics.iter().enumerate() {
            if names.contains(&normalize_ident_part(&m.name)) && done.insert((2, i)) {
                texts.push(m.expr.clone());
                texts.extend(m.non_additive_by.iter().map(|n| n.dimension.clone()));
                if let Some(ws) = &m.window_spec {
                    texts.push(ws.inner_metric.clone());
                    texts.extend(ws.extra_args.iter().cloned());
                    texts.extend(ws.excluding_dims.iter().cloned());
                    texts.extend(ws.partition_dims.iter().cloned());
                    texts.extend(ws.order_by.iter().map(|o| o.expr.clone()));
                    texts.extend(ws.frame_clause.iter().cloned());
                }
                grew = true;
            }
        }
        if !grew {
            return names;
        }
    }
}

/// `def` with every window fact and dimension that a query for `req` may read
/// read as a column its table's derived table computes (see the module docs);
/// borrowed unchanged when there is none.
#[must_use]
pub(super) fn lower_window_members<'a>(
    def: &'a SemanticViewDefinition,
    req: &QueryRequest,
) -> Cow<'a, SemanticViewDefinition> {
    let mut pending = window_members(def);
    let Some(base) = def.tables.first().map(|t| t.alias.clone()) else {
        return Cow::Borrowed(def);
    };
    if pending.is_empty() {
        return Cow::Borrowed(def);
    }
    // One that reads another table and that the query does not read is left
    // as written, and so is each that reads it.
    let needed = needed(def, req);
    let mut skipped: Vec<Member> = pending
        .iter()
        .copied()
        .filter(|&m| {
            let (_, name) = m.describe(def);
            !needed.contains(&normalize_ident_part(name))
                && !tables_read(def, m.table(def, &base), m.expr(def)).is_empty()
        })
        .collect();
    loop {
        let more: Vec<Member> = pending
            .iter()
            .copied()
            .filter(|m| !skipped.contains(m))
            .filter(|&m| {
                reached_facts(m.expr(def), &def.facts)
                    .into_iter()
                    .any(|f| skipped.contains(&Member::Fact(f)))
            })
            .collect();
        if more.is_empty() {
            break;
        }
        skipped.extend(more);
    }
    pending.retain(|m| !skipped.contains(m));
    if pending.is_empty() {
        return Cow::Borrowed(def);
    }
    let mut out = def.clone();
    let mut next = 0;
    while !pending.is_empty() {
        // A member is lowered once no window fact it reads is still pending,
        // so its expression reads those as the columns they became.
        let ready: Vec<Member> = pending
            .iter()
            .copied()
            .filter(|&m| {
                reached_facts(m.expr(&out), &out.facts)
                    .into_iter()
                    .all(|f| Member::Fact(f) == m || !pending.contains(&Member::Fact(f)))
            })
            .collect();
        if ready.is_empty() {
            // A cycle of facts, which CREATE refuses (`validate_facts`).
            break;
        }
        pending.retain(|m| !ready.contains(m));
        let mut tables: Vec<String> = Vec::new();
        for m in &ready {
            let t = m.table(&out, &base).to_string();
            if !tables.iter().any(|x| ident_matches(x, &t)) {
                tables.push(t);
            }
        }
        for table in tables {
            let mut columns: Vec<(String, String)> = Vec::new();
            let mut lowered: Vec<(Member, String)> = Vec::new();
            for &m in ready
                .iter()
                .filter(|m| ident_matches(m.table(&out, &base), &table))
            {
                let name = column_name(&out, &table, next);
                next += 1;
                let expr = super::facts::inline_dimension_facts(m.expr(&out), &out.facts);
                columns.push((name.clone(), expr));
                lowered.push((m, name));
            }
            let sql = derived_table(&out, &table, &columns);
            let alias = quote_stored_ident(&table);
            if let Some(t) = out
                .tables
                .iter_mut()
                .find(|t| ident_matches(&t.alias, &table))
            {
                t.definition = Some(sql);
            }
            for (m, name) in lowered {
                let column = format!("{alias}.{}", quote_ident(&name));
                match m {
                    Member::Fact(i) => out.facts[i].expr = column,
                    Member::Dimension(i) => out.dimensions[i].expr = column,
                }
            }
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dimension, Join, TableRef};

    fn def() -> SemanticViewDefinition {
        let t = |a: &str, pk: &str| TableRef {
            alias: a.to_string(),
            table: format!("w_{a}"),
            pk_columns: vec![pk.to_string()],
            ..Default::default()
        };
        SemanticViewDefinition {
            tables: vec![t("st", "store_id"), t("sales", "id")],
            joins: vec![Join {
                name: Some("sale_store".into()),
                from_alias: "sales".into(),
                table: "st".into(),
                fk_columns: vec!["store_id".into()],
                ref_columns: vec!["store_id".into()],
                cardinality: Cardinality::ManyToOne,
                ..Default::default()
            }],
            facts: vec![
                Fact {
                    name: "total_units".into(),
                    expr: "SUM(sales.units) OVER ()".into(),
                    source_table: Some("sales".into()),
                    ..Default::default()
                },
                Fact {
                    name: "pct".into(),
                    expr: "sales.units / total_units".into(),
                    source_table: Some("sales".into()),
                    ..Default::default()
                },
            ],
            dimensions: vec![
                Dimension {
                    name: "region".into(),
                    expr: "st.region".into(),
                    source_table: Some("st".into()),
                    ..Default::default()
                },
                Dimension {
                    name: "units_rank".into(),
                    expr: "ROW_NUMBER() OVER (PARTITION BY st.region ORDER BY sales.units)".into(),
                    source_table: Some("sales".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    /// A request naming every member of `d`.
    fn all(d: &SemanticViewDefinition) -> QueryRequest {
        QueryRequest {
            dimensions: d.dimensions.iter().map(|x| x.name.clone().into()).collect(),
            facts: d.facts.iter().map(|x| x.name.clone().into()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_window_member_reading_another_table_is_lowered_only_when_read() {
        let d = def();
        let req = QueryRequest {
            facts: vec!["total_units".to_string().into()],
            ..Default::default()
        };
        let out = lower_window_members(&d, &req);
        // `total_units` reads only its own table; `units_rank` reads `st`.
        assert_eq!(out.facts[0].expr, "\"sales\".\"__sv_win_0\"");
        assert_eq!(out.dimensions[1].expr, d.dimensions[1].expr);
        let sales = out.tables[1].definition.as_deref().expect("derived table");
        assert!(!sales.contains("JOIN"), "{sales}");
        // Named in the WHERE clause, it is read.
        let req = QueryRequest {
            where_clause: Some("units_rank = 1".into()),
            ..req
        };
        let out = lower_window_members(&d, &req);
        assert_ne!(out.dimensions[1].expr, d.dimensions[1].expr);
    }

    #[test]
    fn a_view_without_window_members_is_borrowed() {
        let mut d = def();
        d.facts.truncate(0);
        d.dimensions.truncate(1);
        assert!(matches!(
            lower_window_members(&d, &all(&d)),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_window_member_becomes_a_column_of_its_tables_derived_table() {
        let d = def();
        let out = lower_window_members(&d, &all(&d));
        assert_eq!(out.facts[0].expr, "\"sales\".\"__sv_win_0\"");
        assert_eq!(out.dimensions[1].expr, "\"sales\".\"__sv_win_1\"");
        // A row-level fact over a window fact reads its column.
        assert_eq!(out.facts[1].expr, "sales.units / total_units");
        let sales = out.tables[1].definition.as_deref().expect("derived table");
        assert!(sales.starts_with("SELECT \"sales\".*"), "{sales}");
        assert!(
            sales.contains("SUM(sales.units) OVER () AS \"__sv_win_0\""),
            "{sales}"
        );
        assert!(
            sales.contains(
                "LEFT JOIN \"w_st\" AS \"st\" ON \"sales\".\"store_id\" = \"st\".\"store_id\""
            ),
            "{sales}"
        );
        assert!(out.tables[0].definition.is_none());
    }

    #[test]
    fn a_window_over_a_window_fact_is_computed_in_a_later_round() {
        let mut d = def();
        d.dimensions.push(Dimension {
            name: "r2".into(),
            expr: "RANK() OVER (ORDER BY total_units)".into(),
            source_table: Some("sales".into()),
            ..Default::default()
        });
        let out = lower_window_members(&d, &all(&d));
        let sales = out.tables[1].definition.as_deref().expect("derived table");
        // The second round wraps the first and reads its column.
        assert!(
            sales.contains("RANK() OVER (ORDER BY (\"sales\".\"__sv_win_0\")) AS \"__sv_win_2\""),
            "{sales}"
        );
        assert!(sales.contains("FROM (\nSELECT \"sales\".*"), "{sales}");
    }

    #[test]
    fn a_column_name_already_taken_gets_an_underscore() {
        let mut d = def();
        d.table_columns
            .insert("sales".into(), vec!["__sv_win_0".into(), "units".into()]);
        let out = lower_window_members(&d, &all(&d));
        assert_eq!(out.facts[0].expr, "\"sales\".\"__sv_win_0_\"");
    }

    #[test]
    fn a_window_member_reading_a_child_table_is_refused() {
        let mut d = def();
        d.dimensions.push(Dimension {
            name: "busy".into(),
            expr: "RANK() OVER (ORDER BY sales.units)".into(),
            source_table: Some("st".into()),
            ..Default::default()
        });
        let err = validate_window_members(&d).expect_err("child table");
        assert!(
            err.message.contains("does not reach table 'sales'"),
            "{}",
            err.message
        );
        assert!(validate_window_members(&def()).is_ok());
    }

    #[test]
    fn a_window_member_reading_a_table_two_ways_is_refused() {
        let mut d = def();
        let mut second = d.joins[0].clone();
        second.name = Some("sale_store_2".into());
        second.fk_columns = vec!["other_store_id".into()];
        d.joins.push(second);
        let err = validate_window_members(&d).expect_err("two roles");
        assert!(
            err.message.contains("more than one path"),
            "{}",
            err.message
        );
    }
}
