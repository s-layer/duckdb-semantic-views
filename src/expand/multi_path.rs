//! Tables a query reaches by more than one path (a diamond).
//!
//! A view can reach one table from another along two chains of relationships
//! through different tables: orders reach regions through their customer and
//! through their store. A query that groups or filters orders by region has to
//! pick one, and the two give different numbers. Snowflake refuses such a query
//! ("Multi-path relationship between the dimension entity 'R' and the base
//! metric or dimension entity 'O' is not supported", 010246, fixture 3 of
//! 2026-11-10) unless the metric's `USING` names the first relationship of one
//! path, which it then follows. We used to follow the first-declared path
//! without saying so.
//!
//! A path here is a chain of tables, each referencing the next (many-to-one,
//! from the referencing table to the referenced one). Two relationships between
//! the same two tables are one step: that is a role-played table, which
//! `role_playing` handles, and its relationships are the roles.

use std::collections::{HashMap, HashSet};

use crate::model::{Dimension, Metric, SemanticViewDefinition};

use super::types::{ExpandError, MultiPathError};
use super::where_clause::WhereMember;

/// How many paths are collected from each first step. Two settle every
/// question asked here (one path, or more than one); the rest only lengthen
/// the error message.
const PATHS_PER_STEP: usize = 3;

/// The tables `table` references, each once, in declaration order.
fn referenced(def: &SemanticViewDefinition, table: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for j in &def.joins {
        if j.fk_columns.is_empty() || !j.from_alias.eq_ignore_ascii_case(table) {
            continue;
        }
        let to = j.table.to_ascii_lowercase();
        if !out.contains(&to) {
            out.push(to);
        }
    }
    // Sorted, so a walk over references does not depend on declaration order.
    out.sort_unstable();
    out
}

/// The tables `to` can be reached from by following references, `to`
/// included: a walk toward `to` only enters these, so it never explores a
/// branch with no path at its end.
fn reaching(def: &SemanticViewDefinition, to: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::from([to.to_string()]);
    let mut queue: Vec<String> = vec![to.to_string()];
    while let Some(current) = queue.pop() {
        for j in &def.joins {
            if j.fk_columns.is_empty() || !j.table.eq_ignore_ascii_case(&current) {
                continue;
            }
            let from = j.from_alias.to_ascii_lowercase();
            if seen.insert(from.clone()) {
                queue.push(from);
            }
        }
    }
    seen
}

/// Up to `cap` paths from `from` to `to`, each `[from, …, to]`, depth first in
/// declaration order. Only tables that reach `to` are entered, so every step
/// leads to a path and the walk stops after `cap` of them; a table already on
/// the path is not entered again, so a cycle (refused at CREATE) cannot loop.
fn paths_from(def: &SemanticViewDefinition, from: &str, to: &str, cap: usize) -> Vec<Vec<String>> {
    fn walk(
        def: &SemanticViewDefinition,
        path: &mut Vec<String>,
        to: &str,
        reach: &HashSet<String>,
        cap: usize,
        out: &mut Vec<Vec<String>>,
    ) {
        let current = path.last().cloned().unwrap_or_default();
        if current == to {
            out.push(path.clone());
            return;
        }
        for next in referenced(def, &current) {
            if out.len() >= cap {
                return;
            }
            if path.contains(&next) || !reach.contains(&next) {
                continue;
            }
            path.push(next);
            walk(def, path, to, reach, cap, out);
            path.pop();
        }
    }
    let to = to.to_ascii_lowercase();
    let reach = reaching(def, &to);
    let mut out = Vec::new();
    if reach.contains(&from.to_ascii_lowercase()) {
        walk(
            def,
            &mut vec![from.to_ascii_lowercase()],
            &to,
            &reach,
            cap,
            &mut out,
        );
    }
    out
}

/// The paths from `from` up to `to` a metric with `using` may follow.
///
/// Every path when the metric's `USING` names no relationship that starts one
/// of them; otherwise only the paths whose first step is a relationship it
/// names. A `USING` relationship starts at the metric's own table (checked at
/// CREATE), so it chooses the first step.
fn admitted_paths(
    def: &SemanticViewDefinition,
    from: &str,
    to: &str,
    using: &[String],
) -> Vec<Vec<String>> {
    let from = from.to_ascii_lowercase();
    let to = to.to_ascii_lowercase();
    if from == to {
        return Vec::new();
    }
    let by_step: Vec<(String, Vec<Vec<String>>)> = referenced(def, &from)
        .into_iter()
        .map(|step| {
            let rest = paths_from(def, &step, &to, PATHS_PER_STEP);
            (step, rest)
        })
        .filter(|(_, rest)| !rest.is_empty())
        .collect();
    let named: HashSet<String> = using
        .iter()
        .filter_map(|rel| crate::graph::find_relationship(&def.joins, rel))
        .filter(|j| j.from_alias.eq_ignore_ascii_case(&from))
        .map(|j| j.table.to_ascii_lowercase())
        .collect();
    let narrowed = by_step.iter().any(|(step, _)| named.contains(step));
    by_step
        .into_iter()
        .filter(|(step, _)| !narrowed || named.contains(step))
        .flat_map(|(_, rest)| rest)
        .map(|rest| {
            let mut path = vec![from.clone()];
            path.extend(rest);
            path
        })
        .collect()
}

/// The path a metric on `home` with `using` follows to `to` when `to` is
/// reached by several paths and the `USING` picks exactly one of them, as
/// `[home, …, to]`. `None` when `to` is reached one way or none (the usual
/// join walk finds it) or several ways the `USING` does not choose between
/// (refused by [`check_metric_paths`]).
pub(super) fn pinned_path(
    def: &SemanticViewDefinition,
    home: &str,
    to: &str,
    using: &[String],
) -> Option<Vec<String>> {
    if admitted_paths(def, home, to, &[]).len() < 2 {
        return None;
    }
    match admitted_paths(def, home, to, using).as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// The metrics `met` is computed from, each with the table its rows are read
/// at: `met` itself when it is declared on a table, a window metric's inner
/// metric, and a derived metric's base metrics. A base metric declared on no
/// table aggregates at `root`.
pub(super) fn base_components<'a>(
    def: &'a SemanticViewDefinition,
    met: &'a Metric,
    root: &str,
) -> Vec<(&'a Metric, String)> {
    let mut out: Vec<(&Metric, String)> = Vec::new();
    let names = super::facts::collect_transitive_metric_names(met, &def.metrics);
    let reached = std::iter::once(met).chain(
        def.metrics
            .iter()
            .filter(|m| names.contains(&crate::ident::normalize_ident_part(&m.name))),
    );
    for m in reached {
        if m.is_window() || out.iter().any(|(o, _)| std::ptr::eq(*o, m)) {
            continue;
        }
        let home = match &m.source_table {
            Some(t) => t.to_ascii_lowercase(),
            None if crate::graph::contains_aggregate_function(&m.expr).is_some() => {
                root.to_string()
            }
            None => continue,
        };
        out.push((m, home));
    }
    out
}

/// A member whose table a query has to reach: what to call it in an error, and
/// its table.
pub(super) struct Member {
    pub(super) label: String,
    pub(super) table: String,
}

/// The members a query with metrics reaches from each metric's table: every
/// dimension's table and the tables of the facts it is written over, and every
/// `where_clause` member's table and fact tables.
pub(super) fn query_members(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    where_members: &[WhereMember],
) -> Vec<Member> {
    let mut out: Vec<Member> = Vec::new();
    for dim in resolved_dims {
        let label = format!("dimension '{}'", dim.name);
        let tables =
            dim.source_table
                .iter()
                .cloned()
                .chain(super::facts::collect_referenced_fact_tables(
                    &dim.expr, &def.facts,
                ));
        for table in tables {
            out.push(Member {
                label: label.clone(),
                table: table.to_ascii_lowercase(),
            });
        }
    }
    for member in where_members {
        let label = format!("filter member '{}'", member.name);
        for table in member.table.iter().chain(member.fact_tables.iter()) {
            out.push(Member {
                label: label.clone(),
                table: table.to_ascii_lowercase(),
            });
        }
    }
    out
}

/// The paths each multi-path member is reached by for one metric component:
/// member table -> `[home, …, table]`, for every member a [`pinned_path`]
/// exists for. Members the metric's inlined facts reach are included.
pub(super) fn component_paths(
    def: &SemanticViewDefinition,
    component: &Metric,
    home: &str,
    member_tables: &[String],
) -> HashMap<String, Vec<String>> {
    let fact_tables = super::facts::collect_referenced_fact_tables(&component.expr, &def.facts);
    member_tables
        .iter()
        .cloned()
        .chain(fact_tables)
        .filter_map(|t| {
            let t = t.to_ascii_lowercase();
            pinned_path(def, home, &t, &component.using_relationships).map(|p| (t, p))
        })
        .collect()
}

/// Each path as `o -> c -> r`, sorted, so a message does not depend on the
/// order relationships are declared in.
fn render(paths: &[Vec<String>]) -> Vec<String> {
    let mut out: Vec<String> = paths.iter().map(|p| p.join(" -> ")).collect();
    out.sort();
    out
}

/// The tables `table` shares a relationship with, in either direction, each
/// once, sorted: so a walk over them does not depend on declaration order.
fn neighbours(
    def: &SemanticViewDefinition,
    table: &str,
    keep: &dyn Fn(&crate::model::Join) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for j in def
        .joins
        .iter()
        .filter(|j| !j.fk_columns.is_empty() && keep(j))
    {
        let other = if j.from_alias.eq_ignore_ascii_case(table) {
            &j.table
        } else if j.table.eq_ignore_ascii_case(table) {
            &j.from_alias
        } else {
            continue;
        };
        let other = other.to_ascii_lowercase();
        if other != table && !out.contains(&other) {
            out.push(other);
        }
    }
    out.sort();
    out
}

/// A shortest path from `from` to `to` over relationships in either direction,
/// as `[from, …, to]`, never stepping between `skip`'s two tables (in either
/// direction). `None` when there is none.
pub(super) fn undirected_path(
    def: &SemanticViewDefinition,
    from: &str,
    to: &str,
    skip: Option<(&str, &str)>,
) -> Option<Vec<String>> {
    undirected_path_over(def, from, to, skip, &|_| true)
}

/// [`undirected_path`] over only the relationships `keep` accepts.
pub(super) fn undirected_path_over(
    def: &SemanticViewDefinition,
    from: &str,
    to: &str,
    skip: Option<(&str, &str)>,
    keep: &dyn Fn(&crate::model::Join) -> bool,
) -> Option<Vec<String>> {
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut queue = std::collections::VecDeque::from([from.to_string()]);
    let mut seen: HashSet<String> = HashSet::from([from.to_string()]);
    while let Some(current) = queue.pop_front() {
        if current == to {
            let mut path = vec![current];
            while let Some(p) = parent.get(path.last()?) {
                path.push(p.clone());
            }
            path.reverse();
            return Some(path);
        }
        for next in neighbours(def, &current, keep) {
            let skipped = skip
                .is_some_and(|(a, b)| (current == a && next == b) || (current == b && next == a));
            if !skipped && seen.insert(next.clone()) {
                parent.insert(next.clone(), current.clone());
                queue.push_back(next);
            }
        }
    }
    None
}

/// Two of the paths between `from` and `to` over relationships in either
/// direction, each `[from, …, to]`, or `None` when there is one path or none.
///
/// There is a second path exactly when some step of one path lies on a cycle
/// of the (undirected) graph: a path that avoids that step then exists, and a
/// path made only of steps that disconnect the graph is the only one. So a
/// shortest path is taken, and for its first step that can be avoided, a
/// shortest path that avoids it.
pub(super) fn two_undirected_paths(
    def: &SemanticViewDefinition,
    from: &str,
    to: &str,
) -> Option<[Vec<String>; 2]> {
    two_undirected_paths_over(def, from, to, &|_| true)
}

/// [`two_undirected_paths`] over only the relationships `keep` accepts.
pub(super) fn two_undirected_paths_over(
    def: &SemanticViewDefinition,
    from: &str,
    to: &str,
    keep: &dyn Fn(&crate::model::Join) -> bool,
) -> Option<[Vec<String>; 2]> {
    let first = undirected_path_over(def, from, to, None, keep)?;
    first.windows(2).find_map(|step| {
        let second = undirected_path_over(def, from, to, Some((&step[0], &step[1])), keep)?;
        Some([first.clone(), second])
    })
}

/// A path over relationships in either direction, as `c <- o -> s`: `->` where
/// a table references the next, `<-` where the next references it.
fn render_undirected(def: &SemanticViewDefinition, path: &[String]) -> String {
    let mut s = path.first().cloned().unwrap_or_default();
    for step in path.windows(2) {
        let forward = referenced(def, &step[0]).contains(&step[1]);
        s.push_str(if forward { " -> " } else { " <- " });
        s.push_str(&step[1]);
    }
    s
}

/// Whether a metric on `home` by a member on `to` is refused as reaching it
/// by no chain and two ways ([`many_to_many_paths`]), for `SHOW SEMANTIC
/// DIMENSIONS ... FOR METRIC`, which leaves such a member out.
#[cfg_attr(not(feature = "extension"), allow(dead_code))]
pub(crate) fn refused_as_many_to_many(def: &SemanticViewDefinition, home: &str, to: &str) -> bool {
    !home.eq_ignore_ascii_case(to)
        && admitted_paths(def, home, to, &[]).is_empty()
        && many_to_many_paths(def, home, to).is_some()
}

/// When a query reaches `to` from `home` by no chain of foreign keys: two of
/// the paths it could take instead, across a many-to-many relationship and in
/// either direction (shorter first), or `None` when there is one path or none.
///
/// With no chain to follow, the only way from `home` to `to` crosses a
/// many-to-many relationship (a bridge, or the orders between customers and
/// stores) or steps back along a foreign key. Snowflake answers that when it
/// is the only path, and refuses a second one ("Unsupported feature
/// 'Multi-path in many-to-many relationship'", 000002): customers reach
/// stores through orders and through regions (fixture 5, X1), books reach
/// authors through either of two bridges (fixture 3, Q2), and a book reaches
/// `book_authors` directly and through its lead author (fixture 5, P3). A
/// `USING` cannot choose between them.
pub(super) fn many_to_many_paths(
    def: &SemanticViewDefinition,
    home: &str,
    to: &str,
) -> Option<Vec<String>> {
    let home = home.to_ascii_lowercase();
    let to = to.to_ascii_lowercase();
    let mut paths: Vec<String> = two_undirected_paths(def, &home, &to)?
        .iter()
        .map(|p| render_undirected(def, p))
        .collect();
    paths.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    Some(paths)
}

/// For a query with no metrics whose members several tables pair equally
/// well (two bridges between books and authors, Q8): the routes between the
/// first two members through each such table, as `a <- ba -> b`.
pub(super) fn paired_routes(
    def: &SemanticViewDefinition,
    pairing: &[&String],
    first: &str,
    second: &str,
) -> Vec<String> {
    pairing
        .iter()
        .filter_map(|via| {
            let up = paths_from(def, via, first, 1).into_iter().next()?;
            let down = paths_from(def, via, second, 1).into_iter().next()?;
            let mut s = up.iter().rev().cloned().collect::<Vec<_>>().join(" <- ");
            for t in &down[1..] {
                s.push_str(" -> ");
                s.push_str(t);
            }
            Some(s)
        })
        .collect()
}

/// Refuse a query that reaches a member's table from a metric's table by more
/// than one path the metric's `USING` does not choose between.
///
/// Asked of every metric each queried metric is computed from (a derived
/// metric's base metrics, a window metric's inner metric), since each is
/// computed through its own joins.
pub(super) fn check_metric_paths(
    view_name: &str,
    def: &SemanticViewDefinition,
    root: &str,
    resolved_mets: &[&Metric],
    members: &[Member],
) -> Result<(), ExpandError> {
    for met in resolved_mets {
        for (component, home) in base_components(def, met, root) {
            let fact_tables =
                super::facts::collect_referenced_fact_tables(&component.expr, &def.facts);
            let fact_members = fact_tables.into_iter().map(|t| Member {
                label: format!("a fact metric '{}' is written over", component.name),
                table: t.to_ascii_lowercase(),
            });
            let reached: Vec<Member> = members
                .iter()
                .map(|m| Member {
                    label: m.label.clone(),
                    table: m.table.clone(),
                })
                .chain(fact_members)
                .collect();
            for member in &reached {
                let paths =
                    admitted_paths(def, &home, &member.table, &component.using_relationships);
                if paths.len() > 1 {
                    return Err(ExpandError::MultiPath {
                        detail: Box::new(MultiPathError {
                            view_name: view_name.to_string(),
                            member: member.label.clone(),
                            member_table: member.table.clone(),
                            from_table: home,
                            reader: format!("metric '{}'", met.name),
                            paths: render(&paths),
                            many_to_many: false,
                        }),
                    });
                }
                if !paths.is_empty() || member.table == home {
                    continue;
                }
                if let Some(routes) = many_to_many_paths(def, &home, &member.table) {
                    return Err(ExpandError::MultiPath {
                        detail: Box::new(MultiPathError {
                            view_name: view_name.to_string(),
                            member: member.label.clone(),
                            member_table: member.table.clone(),
                            from_table: home,
                            reader: format!("metric '{}'", met.name),
                            paths: routes,
                            many_to_many: true,
                        }),
                    });
                }
            }
        }
    }
    Ok(())
}

/// The first metric component (in query order) that follows a pinned path to
/// one of `members`' tables or its own fact tables, as (queried metric, table,
/// path). Only the per-grain planner follows a pinned path, so a query it
/// declines that has one is refused rather than joined along another path.
pub(super) fn first_pinned(
    def: &SemanticViewDefinition,
    root: &str,
    resolved_mets: &[&Metric],
    members: &[Member],
) -> Option<(String, String, String)> {
    let tables: Vec<String> = members.iter().map(|m| m.table.clone()).collect();
    for met in resolved_mets {
        for (component, home) in base_components(def, met, root) {
            let mut paths: Vec<(String, Vec<String>)> =
                component_paths(def, component, &home, &tables)
                    .into_iter()
                    .collect();
            paths.sort();
            if let Some((table, path)) = paths.into_iter().next() {
                return Some((met.name.clone(), table, path.join(" -> ")));
            }
        }
    }
    None
}

/// The paths by which `anchor`, the table a query with no metrics reads rows
/// from, reaches `member` when there is more than one: such a query has no
/// `USING` to choose with. `None` when there is one path or none.
pub(super) fn anchor_paths(
    def: &SemanticViewDefinition,
    anchor: &str,
    member: &str,
) -> Option<Vec<String>> {
    let paths = admitted_paths(def, anchor, member, &[]);
    (paths.len() > 1).then(|| render(&paths))
}

/// For a query with no metrics whose rows cannot be placed (a member is
/// declared on no table, so [`member_anchor`](super::sql_gen::member_anchor)
/// decides nothing): a member table that the base table or another member's
/// table reaches by more than one path, with that table and the paths. The
/// base-anchored emission such a query falls to would join along one of them.
pub(super) fn between_members(
    def: &SemanticViewDefinition,
    members: &[String],
) -> Option<(String, String, Vec<String>)> {
    let mut tables: Vec<String> = members.iter().map(|m| m.to_ascii_lowercase()).collect();
    tables.sort();
    tables.dedup();
    let root = crate::graph::RelationshipGraph::from_definition(def)
        .map(|g| g.root.to_ascii_lowercase())
        .ok();
    for from in root.iter().chain(&tables) {
        for to in tables.iter().filter(|t| *t != from) {
            if let Some(paths) = anchor_paths(def, from, to) {
                return Some((from.clone(), to.clone(), paths));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Join, TableRef};

    fn def(edges: &[(&str, &str, &str)]) -> SemanticViewDefinition {
        let mut tables: Vec<String> = Vec::new();
        for (_, from, to) in edges {
            for t in [from, to] {
                if !tables.iter().any(|x| x == t) {
                    tables.push((*t).to_string());
                }
            }
        }
        SemanticViewDefinition {
            tables: tables
                .iter()
                .map(|t| TableRef {
                    alias: t.clone(),
                    table: t.clone(),
                    pk_columns: vec!["id".to_string()],
                    ..Default::default()
                })
                .collect(),
            joins: edges
                .iter()
                .map(|(name, from, to)| Join {
                    table: (*to).to_string(),
                    from_alias: (*from).to_string(),
                    fk_columns: vec![format!("{to}_id")],
                    ref_columns: vec!["id".to_string()],
                    name: Some((*name).to_string()),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn diamond() -> SemanticViewDefinition {
        def(&[
            ("o_cust", "o", "c"),
            ("o_store", "o", "s"),
            ("c_region", "c", "r"),
            ("s_region", "s", "r"),
            ("r_country", "r", "k"),
        ])
    }

    #[test]
    fn a_diamond_has_two_paths_to_its_shared_table_and_past_it() {
        let d = diamond();
        assert_eq!(
            render(&admitted_paths(&d, "o", "r", &[])),
            ["o -> c -> r", "o -> s -> r"]
        );
        assert_eq!(admitted_paths(&d, "o", "k", &[]).len(), 2);
        assert_eq!(admitted_paths(&d, "c", "r", &[]).len(), 1);
        assert_eq!(admitted_paths(&d, "r", "o", &[]).len(), 0);
    }

    #[test]
    fn using_picks_the_path_its_relationship_starts() {
        let d = diamond();
        let using = ["o_store".to_string()];
        assert_eq!(
            pinned_path(&d, "o", "k", &using),
            Some(
                vec!["o", "s", "r", "k"]
                    .into_iter()
                    .map(String::from)
                    .collect()
            )
        );
        // A table one path reaches needs no pin, `USING` or not.
        assert_eq!(pinned_path(&d, "o", "c", &using), None);
        assert_eq!(pinned_path(&d, "o", "s", &using), None);
    }

    #[test]
    fn using_on_both_first_steps_chooses_neither() {
        let d = diamond();
        let using = ["o_cust".to_string(), "o_store".to_string()];
        assert_eq!(admitted_paths(&d, "o", "r", &using).len(), 2);
        assert_eq!(pinned_path(&d, "o", "r", &using), None);
    }

    /// Fifteen layers of three tables, each referencing all three in the next
    /// layer, have 3^15 paths to the top. The walk stops after a few of them,
    /// and never enters a table that cannot reach its target.
    #[test]
    fn a_dense_graph_is_walked_only_as_far_as_the_answer_needs() {
        let names: Vec<(String, String, String)> = (0..15)
            .flat_map(|layer| {
                (0..3).flat_map(move |a| {
                    (0..3).map(move |b| {
                        let to = if layer == 14 {
                            "top".to_string()
                        } else {
                            format!("t{}_{b}", layer + 1)
                        };
                        (format!("r{layer}_{a}_{b}"), format!("t{layer}_{a}"), to)
                    })
                })
            })
            .collect();
        let edges: Vec<(&str, &str, &str)> = names
            .iter()
            .map(|(n, f, t)| (n.as_str(), f.as_str(), t.as_str()))
            .chain([("dead", "t0_0", "nowhere")])
            .collect();
        let d = def(&edges);
        assert!(admitted_paths(&d, "t0_0", "top", &[]).len() >= 2);
        assert!(admitted_paths(&d, "t0_0", "nowhere", &[]).len() == 1);
        assert!(admitted_paths(&d, "t5_0", "t0_0", &[]).is_empty());
    }

    #[test]
    fn two_relationships_between_two_tables_are_one_step() {
        // A role-played table: `role_playing` decides which relationship.
        let d = def(&[("dep", "f", "a"), ("arr", "f", "a"), ("a_region", "a", "r")]);
        assert_eq!(admitted_paths(&d, "f", "r", &[]).len(), 1);
    }

    #[test]
    fn a_member_reached_two_ways_by_no_chain_has_two_many_to_many_paths() {
        let d = diamond();
        assert_eq!(
            many_to_many_paths(&d, "c", "s"),
            Some(vec!["c -> r <- s".to_string(), "c <- o -> s".to_string()])
        );
        assert_eq!(
            many_to_many_paths(&d, "r", "o"),
            Some(vec!["r <- c <- o".to_string(), "r <- s <- o".to_string()])
        );
        // Past the diamond's top, `k` still reaches the customers two ways.
        assert_eq!(many_to_many_paths(&d, "k", "c").map(|p| p.len()), Some(2));
    }

    #[test]
    fn a_tree_has_one_path_between_any_two_tables() {
        // One bridge: books reach authors only across `ba`, and orders' child
        // is reached only one way.
        let d = def(&[
            ("ba_b", "ba", "b"),
            ("ba_a", "ba", "a"),
            ("a_k", "a", "k"),
            ("rv_b", "rv", "b"),
        ]);
        for (from, to) in [("b", "a"), ("b", "k"), ("a", "rv"), ("k", "ba"), ("b", "b")] {
            assert_eq!(many_to_many_paths(&d, from, to), None, "{from} -> {to}");
        }
        assert_eq!(many_to_many_paths(&d, "b", "nowhere"), None);
    }

    #[test]
    fn two_relationships_between_two_tables_are_one_step_either_way() {
        // Role-playing: two FKs from flights to airports are one step, so
        // airports reach flights one way.
        let d = def(&[("dep", "f", "a"), ("arr", "f", "a")]);
        assert_eq!(many_to_many_paths(&d, "a", "f"), None);
    }

    #[test]
    fn the_paths_do_not_depend_on_declaration_order() {
        let mut d = diamond();
        let forward = many_to_many_paths(&d, "c", "s");
        d.joins.reverse();
        assert_eq!(many_to_many_paths(&d, "c", "s"), forward);
    }
}
