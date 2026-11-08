use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph::RelationshipGraph;
use crate::model::{Join, SemanticViewDefinition, TableRef};

use super::facts::{
    collect_derived_metric_source_tables, collect_derived_metric_using,
    collect_referenced_fact_tables, collect_transitive_metric_names,
};
use super::resolution::{
    qualify_and_quote_table_ref, quote_ident, quote_ident_if_needed, quote_stored_ident,
    table_source,
};
use super::types::ExpandError;

/// Build a role-playing scoped alias in the documented `{table}__{rel}` format.
///
/// The result is the alias's *logical* value — never SQL text. Both parts go
/// through [`crate::ident::normalize_ident_part`], so a stored, quoted
/// relationship name (`"de p"`, embedded `""` and all) contributes its
/// unquoted, case-folded value (`de p`) rather than its delimiters (TECH-DEBT
/// #70, MM-2). Every emission site then quotes the alias exactly once: the
/// JOIN clause via `quote_ident`, a rewritten member reference via
/// [`rewrite_to_scoped_alias`]. Concatenating the raw stored text instead put
/// quote characters *inside* the alias, which the two sites then quoted
/// differently (`"ap__""de p"""` vs `ap__"de p".city`) — unparseable SQL.
///
/// This is the single source of the format so every producer — this module's
/// USING-relationship join synthesis, `role_playing::find_using_context` and
/// the per-grain emitter — can never drift (E-10, code-review 2026-07-11).
pub(super) fn scoped_join_alias(table: &str, rel: &str) -> String {
    format!(
        "{}__{}",
        crate::ident::normalize_ident_part(table),
        crate::ident::normalize_ident_part(rel)
    )
}

/// Rewrite every reference qualified by `table` in `expr` to the role-playing
/// `scoped` alias (as built by [`scoped_join_alias`]), emitting the alias as
/// one SQL identifier.
///
/// A bare-safe alias (`ap__r_dep`) is emitted unquoted, byte-identical to the
/// pre-#70 output; anything else (`ap__de p`, non-ASCII, upper case) is
/// double-quoted, matching the `quote_ident` the JOIN clause applies to the
/// same logical alias. This is the one reference-site spelling, shared by the
/// dimension emitters and the role-played fact rewrite (TECH-DEBT #69).
pub(super) fn rewrite_to_scoped_alias(expr: &str, table: &str, scoped: &str) -> String {
    crate::expr_tokens::rewrite_qualifier(expr, table, &quote_ident_if_needed(scoped))
}

/// Synthesize an ON clause from PK/FK column declarations (Phase 26).
///
/// Zips `join.fk_columns` with the referenced table's `pk_columns` to produce
/// `from_alias.fk = to_alias.pk` pairs, joined by ` AND `.
/// Uses `join.from_alias` for the FROM side and `join.table` for the TO side.
pub(super) fn synthesize_on_clause(join: &Join, tables: &[TableRef]) -> String {
    on_clause(
        join,
        tables,
        None,
        &quote_stored_ident(&join.table),
        AsofSide::Forward,
    )
}

/// Synthesize an ON clause with a potentially scoped alias on the PK (target) side.
///
/// Like `synthesize_on_clause`, but uses `to_alias` instead of `join.table` in the
/// generated SQL. This supports role-playing dimensions where the same physical table
/// appears multiple times with different scoped aliases (e.g., `a__dep_airport`).
///
/// Phase 33: Prefers `join.ref_columns` (resolved during inference) over looking up
/// `pk_columns` from the target table. Falls back to target PK for backward compat
/// (legacy joins without `ref_columns`).
pub(super) fn synthesize_on_clause_scoped(
    join: &Join,
    tables: &[TableRef],
    from_alias: Option<&str>,
    to_alias: &str,
) -> String {
    on_clause(
        join,
        tables,
        from_alias,
        &quote_ident(to_alias),
        AsofSide::Forward,
    )
}

/// How an ASOF relationship's ASOF pair is rendered (Phase 7.1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AsofSide {
    /// The referenced table is joined onto the referencing one, by an
    /// `ASOF LEFT JOIN`: the pair is its `from.fk >= to.ref` match condition.
    Forward,
    /// The referencing table is joined onto the referenced one, as the
    /// derived table [`reverse_asof_source`] builds: the pair is
    /// `from.<ASOF_MATCH_COLUMN> = to.ref`, the referenced row it matched.
    Reverse,
}

/// The column [`reverse_asof_source`] adds to the referencing table: the
/// referenced row's ASOF value that the row matched.
const ASOF_MATCH_COLUMN: &str = "__sv_asof_match";

/// The referenced columns of `join`, in `fk_columns` order.
fn ref_columns<'j>(join: &'j Join, tables: &'j [TableRef]) -> &'j [String] {
    // Phase 33: Prefer ref_columns (resolved during inference).
    // Fall back to target PK for backward compat (legacy joins without ref_columns).
    if join.ref_columns.is_empty() {
        let to_alias_lower = join.table.to_ascii_lowercase();
        tables
            .iter()
            .find(|t| t.alias.to_ascii_lowercase() == to_alias_lower)
            .map_or(&[] as &[String], |t| &t.pk_columns)
    } else {
        &join.ref_columns
    }
}

/// The ON clause of `join` with `to_sql`, an already-quoted alias, on the
/// referenced side, and `from_alias` (a scoped alias) on the referencing side
/// when the edge leaves a role-played instance rather than the declared
/// `from_alias` (Phase 6.3b). Stored aliases and key columns keep any quotes
/// they were written with, so each is stripped and quoted once
/// ([`quote_stored_ident`]); a scoped alias is a logical value and is quoted as
/// is. An ASOF relationship's ASOF pair is rendered per `asof`.
fn on_clause(
    join: &Join,
    tables: &[TableRef],
    from_alias: Option<&str>,
    to_sql: &str,
    asof: AsofSide,
) -> String {
    let from_sql = from_alias.map_or_else(|| quote_stored_ident(&join.from_alias), quote_ident);
    if let (Some(end), [fk], [start]) = (
        &join.range_end,
        join.fk_columns.as_slice(),
        join.ref_columns.as_slice(),
    ) {
        return range_condition(
            &format!("{from_sql}.{}", quote_stored_ident(fk)),
            &format!("{to_sql}.{}", quote_stored_ident(start)),
            &format!("{to_sql}.{}", quote_stored_ident(end)),
        );
    }
    let pairs: Vec<String> = join
        .fk_columns
        .iter()
        .zip(ref_columns(join, tables).iter())
        .enumerate()
        .map(|(i, (fk, pk))| {
            let (fk_sql, op) = match (join.asof == Some(i), asof) {
                (false, _) => (quote_stored_ident(fk), "="),
                (true, AsofSide::Forward) => (quote_stored_ident(fk), ">="),
                (true, AsofSide::Reverse) => (quote_ident(ASOF_MATCH_COLUMN), "="),
            };
            format!(
                "{from_sql}.{fk_sql} {op} {to_sql}.{}",
                quote_stored_ident(pk)
            )
        })
        .collect();
    pairs.join(" AND ")
}

/// Phase 7.2: the match condition of a range relationship, `value` in the
/// half-open period `[start, end)`, a NULL bound open on its side (the
/// answers Snowflake gave on the Phase 7.0 fixture, R1 and R6). A NULL value
/// is in no period. The condition names both sides, so it reads the same
/// whichever table the join walks from.
///
/// It is written as two comparisons of rows, one per bound, so that the planner
/// plans it as a range join (an inequality or piecewise merge join) rather than a
/// nested loop, which `s IS NULL OR v >= s` forces (PR #338 review: 2M events
/// against 10k periods, 34 s against 0.5 s). Each row leads with a flag: a
/// NULL start is `(-1, NULL)`, below every `(0, v)`, and a NULL end `(1,
/// NULL)`, above every one, whatever the bounds' type; otherwise the flags tie
/// and the bound decides. A NULL value gives a NULL row, which matches
/// nothing (a `(0, NULL)` would sort after every bound).
fn range_condition(value: &str, start: &str, end: &str) -> String {
    let v = format!("(CASE WHEN {value} IS NOT NULL THEN ROW(0, {value}) END)");
    format!(
        "({v} >= ROW(CASE WHEN {start} IS NULL THEN -1 ELSE 0 END, {start}) \
         AND {v} < ROW(CASE WHEN {end} IS NULL THEN 1 ELSE 0 END, {end}))"
    )
}

/// The referencing table of the ASOF relationship `join`, as a derived table
/// that carries, per row, the ASOF value of the referenced row it matches
/// ([`ASOF_MATCH_COLUMN`]).
///
/// It is what a join walked against the relationship's direction joins: from
/// the referenced table, `ON <equalities> AND from.__sv_asof_match = to.ref`
/// gives each referenced row exactly the referencing rows whose ASOF match it
/// is, which no condition over the two bare tables can say. A referencing row
/// with no match has a NULL there and joins no referenced row, as in the
/// forward join.
fn reverse_asof_source(join: &Join, def: &SemanticViewDefinition, asof: usize) -> Option<String> {
    let find = |alias: &str| {
        def.tables
            .iter()
            .find(|t| t.alias.eq_ignore_ascii_case(alias))
    };
    let (from, to) = (find(&join.from_alias)?, find(&join.table)?);
    let ref_col = ref_columns(join, &def.tables).get(asof)?;
    let inner = Join {
        from_alias: "__sv_asof_from".to_string(),
        table: "__sv_asof_to".to_string(),
        ..join.clone()
    };
    Some(format!(
        "(SELECT \"__sv_asof_from\".*, \"__sv_asof_to\".{ref_col} AS {match_col} \
         FROM {from_src} AS \"__sv_asof_from\" \
         ASOF JOIN {to_src} AS \"__sv_asof_to\" ON {on})",
        ref_col = quote_stored_ident(ref_col),
        match_col = quote_ident(ASOF_MATCH_COLUMN),
        from_src = table_source(from, def),
        to_src = table_source(to, def),
        on = on_clause(
            &inner,
            &def.tables,
            None,
            "\"__sv_asof_to\"",
            AsofSide::Forward
        ),
    ))
}

/// Refuse a stored ASOF marker, or range relationship (Phase 7.2), that
/// cannot be emitted.
///
/// CREATE only stores a marker that names one of the referenced columns of a
/// relationship between two of the view's tables, but a definition can also
/// arrive from storage or (in the fuzzers) be built directly. One whose index
/// is past the key, or whose tables are missing, would otherwise render as a
/// plain join with no ASOF condition: a silent wrong number. So expansion
/// refuses it up front, which also keeps the emission asserts in
/// [`push_join_clauses`] true.
pub(super) fn check_asof_markers(
    view_name: &str,
    def: &SemanticViewDefinition,
) -> Result<(), ExpandError> {
    let has_table = |alias: &str| {
        def.tables
            .iter()
            .any(|t| t.alias.eq_ignore_ascii_case(alias))
    };
    for join in &def.joins {
        // Phase 7.2: a range relationship renders its range only from one
        // value column and one start column (`on_clause`); any other shape
        // would render as equalities.
        if join.range_end.is_some()
            && (join.asof.is_some() || join.fk_columns.len() != 1 || join.ref_columns.len() != 1)
        {
            return Err(ExpandError::UncheckableDefinition {
                view_name: view_name.to_string(),
                reason: format!(
                    "the range relationship from '{}' to '{}' does not pair one column \
                     with one range",
                    join.from_alias, join.table
                ),
            });
        }
        let Some(asof) = join.asof else { continue };
        let fits = asof < join.fk_columns.len() && asof < ref_columns(join, &def.tables).len();
        if !fits || !has_table(&join.from_alias) || !has_table(&join.table) {
            return Err(ExpandError::UncheckableDefinition {
                view_name: view_name.to_string(),
                reason: format!(
                    "the ASOF marker of the relationship from '{}' to '{}' does not name \
                     one of its key columns",
                    join.from_alias, join.table
                ),
            });
        }
    }
    Ok(())
}

/// A join edge chosen by the resolver, in emission order.
///
/// Carries everything the emitters need to render the JOIN clause directly:
/// no re-searching `def.joins` by alias (SG-2) and no re-parsing of scoped
/// alias strings at `__` (SG-12).
pub(super) struct ResolvedJoin<'a> {
    /// SQL alias emitted after `AS` — the bare table alias (lowercased) or a
    /// role-playing scoped alias in the documented `{bare}__{rel}` format.
    pub emit_alias: String,
    /// The bare (lowercased) table alias, used to look up the physical table
    /// name in `def.tables`.
    pub bare_alias: String,
    /// The join edge connecting `emit_alias` to an already-emitted table.
    pub join: &'a Join,
    /// Whether `emit_alias` is a role-playing scoped alias. Scoped joins put
    /// the scoped alias on the PK side of the synthesized ON clause.
    pub scoped: bool,
    /// The scoped alias of the referencing side, when the edge leaves a
    /// role-played instance: `a__dep` for `regions` reached through the
    /// departure airport (Phase 6.3b). `None` uses the declared `from_alias`.
    pub from_emit: Option<String>,
}

/// Render `LEFT JOIN` clauses for resolver-selected edges onto `sql`.
///
/// `prefix` carries the newline + indentation + keyword for the emission site
/// (e.g. `"\nLEFT JOIN "` in flat queries, `"\n    LEFT JOIN "` inside CTEs).
pub(super) fn push_join_clauses(
    sql: &mut String,
    joins: &[ResolvedJoin<'_>],
    def: &SemanticViewDefinition,
    prefix: &str,
) {
    for rj in joins {
        let table_ref = def
            .tables
            .iter()
            .find(|t| t.alias.to_ascii_lowercase() == rj.bare_alias);
        // Phase 7.1: an ASOF relationship joins its referenced table with an
        // `ASOF LEFT JOIN`. Walked the other way (the referencing table joined
        // onto the referenced one), it joins the derived table
        // `reverse_asof_source` builds instead.
        let reverse_asof = rj
            .join
            .asof
            .filter(|_| !rj.scoped && !rj.join.table.eq_ignore_ascii_case(&rj.bare_alias));
        let reverse_source = reverse_asof.and_then(|i| reverse_asof_source(rj.join, def, i));
        // Every caller passes a `LEFT JOIN ` prefix, and every reverse hop is
        // a bare one whose two tables the view has. Anything else would render
        // the ASOF pair as a plain inequality join, a silent wrong number, so
        // it is a bug to catch in tests rather than SQL to emit.
        debug_assert!(
            rj.join.asof.is_none() || prefix.ends_with("LEFT JOIN "),
            "an ASOF relationship joined with the prefix {prefix:?}"
        );
        debug_assert!(
            reverse_asof.is_none() || reverse_source.is_some(),
            "an ASOF relationship walked backwards has no derived table"
        );
        debug_assert!(
            rj.join.asof.is_none()
                || !rj.scoped
                || rj.join.table.eq_ignore_ascii_case(&rj.bare_alias),
            "a role-played ASOF hop walked backwards"
        );
        match (&reverse_source, rj.join.asof) {
            (None, Some(_)) => match prefix.strip_suffix("LEFT JOIN ") {
                Some(lead) => {
                    sql.push_str(lead);
                    sql.push_str("ASOF LEFT JOIN ");
                }
                None => sql.push_str(prefix),
            },
            _ => sql.push_str(prefix),
        }
        sql.push_str(&match (&reverse_source, table_ref) {
            (Some(source), _) => source.clone(),
            (None, Some(t)) => table_source(t, def),
            (None, None) => qualify_and_quote_table_ref(&rj.bare_alias, def),
        });
        sql.push_str(" AS ");
        // A scoped alias is a logical value; a bare one is stored as written.
        sql.push_str(&if rj.scoped {
            quote_ident(&rj.emit_alias)
        } else {
            quote_stored_ident(&rj.emit_alias)
        });
        sql.push_str(" ON ");
        if rj.scoped {
            sql.push_str(&synthesize_on_clause_scoped(
                rj.join,
                &def.tables,
                rj.from_emit.as_deref(),
                &rj.emit_alias,
            ));
        } else if reverse_source.is_some() {
            sql.push_str(&on_clause(
                rj.join,
                &def.tables,
                None,
                &quote_stored_ident(&rj.join.table),
                AsofSide::Reverse,
            ));
        } else {
            sql.push_str(&synthesize_on_clause(rj.join, &def.tables));
        }
    }
}

/// Undirected BFS from the graph root over PK/FK join edges.
///
/// Returns, for each reachable non-root alias, its tree parent (the neighbor
/// on the path toward the root) and the `Join` edge connecting the two. This
/// is the edge the emitters must use for the alias's ON clause: any other join
/// mentioning the alias would reference a table that is not yet (or never)
/// joined (SG-2). Traversing edges in both directions also covers tables on
/// the FK side of the root (SG-10).
fn build_tree_parents<'a>(
    def: &'a SemanticViewDefinition,
    root: &str,
) -> HashMap<String, (String, &'a Join)> {
    let mut tree_parent: HashMap<String, (String, &'a Join)> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(root.to_string());
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(root.to_string());
    while let Some(current) = queue.pop_front() {
        // Visit edges in declaration order for determinism.
        for join in &def.joins {
            if join.fk_columns.is_empty() {
                continue; // Legacy join -- not part of the PK/FK graph
            }
            let from = join.from_alias.to_ascii_lowercase();
            let to = join.table.to_ascii_lowercase();
            let neighbor = if from == current {
                to
            } else if to == current {
                from
            } else {
                continue;
            };
            if visited.insert(neighbor.clone()) {
                tree_parent.insert(neighbor.clone(), (current.clone(), join));
                queue.push_back(neighbor.clone());
            }
        }
    }
    tree_parent
}

/// Append a bare-alias join to `result`, choosing the connecting edge.
///
/// Reachable aliases use their BFS tree edge. Aliases disconnected from the
/// root (degenerate definitions that predate stricter validation) fall back to
/// the first declared join mentioning the alias; aliases with no join at all
/// are skipped, matching the previous emitters' behavior.
fn push_bare_join<'a>(
    result: &mut Vec<ResolvedJoin<'a>>,
    def: &'a SemanticViewDefinition,
    tree_parent: &HashMap<String, (String, &'a Join)>,
    alias: String,
) {
    let join = if let Some((_, join)) = tree_parent.get(&alias) {
        *join
    } else {
        let Some(join) = def.joins.iter().find(|j| {
            j.table.to_ascii_lowercase() == alias || j.from_alias.to_ascii_lowercase() == alias
        }) else {
            return;
        };
        join
    };
    result.push(ResolvedJoin {
        emit_alias: alias.clone(),
        bare_alias: alias,
        join,
        scoped: false,
        from_emit: None,
    });
}

/// `resolved_mets` plus, transitively, every metric a window metric among them
/// names as its inner aggregate, each once, in first-reached order.
fn with_window_inner_metrics<'m>(
    resolved_mets: &[&'m crate::model::Metric],
    all_metrics: &'m [crate::model::Metric],
) -> Vec<&'m crate::model::Metric> {
    let mut out: Vec<&crate::model::Metric> = resolved_mets.to_vec();
    let mut i = 0;
    while i < out.len() {
        if let Some(ref ws) = out[i].window_spec {
            if let Some(inner) = all_metrics
                .iter()
                .find(|m| crate::ident::ident_matches(&m.name, &ws.inner_metric))
            {
                if !out.iter().any(|m| std::ptr::eq(*m, inner)) {
                    out.push(inner);
                }
            }
        }
        i += 1;
    }
    out
}

/// Resolve which joins are needed using graph-based PK/FK resolution (Phase 26+32).
///
/// Builds a `RelationshipGraph` from the definition, collects needed table
/// aliases from resolved dimensions, metrics, and fact source tables, includes
/// every table on the path between the root and a needed alias (in both edge
/// directions — SG-10), and returns structured join edges in emission order:
/// each edge's ON clause references only the new alias and already-emitted
/// tables (SG-2).
///
/// `fact_source_tables` carries fact source aliases for the facts path
/// (`expand_facts`); other callers pass `&[]`. Fact-driven joins are appended
/// after dimension-driven joins, preserving the historical clause order.
///
/// Phase 32: When metrics have `using_relationships`, generates scoped aliases
/// (`{to_alias}__{rel_name}`) instead of bare aliases. Scoped joins are placed
/// after all bare joins, sorted by alias for deterministic output.
#[allow(clippy::too_many_lines)]
pub(super) fn resolve_joins_pkfk<'a>(
    def: &'a SemanticViewDefinition,
    resolved_dims: &[&crate::model::Dimension],
    resolved_mets: &[&crate::model::Metric],
    fact_source_tables: &[String],
) -> Vec<ResolvedJoin<'a>> {
    let Ok(graph) = RelationshipGraph::from_definition(def) else {
        return Vec::new(); // Graph was validated at define time
    };

    // A window metric's inner aggregate is computed in `__sv_agg` against these
    // joins, so the inner metric needs them as much as a queried one does: its
    // `USING` scoped joins and the tables its inlined facts reach. A window
    // metric is qualified (it carries the source table of its declaration), so
    // none of the derived-metric walks below reach it (PR #247 review — the
    // inlined fact named `a__dep_airport` while no scoped join was emitted).
    let with_inner = with_window_inner_metrics(resolved_mets, &def.metrics);
    let resolved_mets: &[&crate::model::Metric] = &with_inner;

    let root = &graph.root;

    // Phase 32: Collect scoped joins from metrics with USING relationships.
    // Each entry carries the scoped alias and the resolved Join edge, so the
    // emitters never re-derive the relationship from the alias string (SG-12).
    let mut scoped_joins: Vec<(String, &Join, String)> = Vec::new();
    // Also track which bare aliases are role-playing (have multiple relationships).
    let mut role_playing_bare_aliases: HashSet<String> = HashSet::new();

    let add_scoped = |scoped_joins: &mut Vec<(String, &'a Join, String)>,
                      role_playing: &mut HashSet<String>,
                      using_rel: &str| {
        if let Some(join) = crate::graph::find_relationship(&def.joins, using_rel) {
            let to_alias = join.table.to_ascii_lowercase();
            // `scoped_join_alias` normalizes the name, so every spelling of
            // one relationship yields the same alias.
            let scoped = scoped_join_alias(&to_alias, using_rel);
            if !scoped_joins.iter().any(|(s, _, _)| *s == scoped) {
                scoped_joins.push((scoped, join, using_rel.to_string()));
            }
            role_playing.insert(to_alias);
        }
    };

    for met in resolved_mets {
        if !met.using_relationships.is_empty() {
            for using_rel in &met.using_relationships {
                add_scoped(&mut scoped_joins, &mut role_playing_bare_aliases, using_rel);
            }
        } else if met.source_table.is_none() {
            // Derived metric: walk transitive USING relationships
            let transitive_using = collect_derived_metric_using(met, &def.metrics);
            for using_rel in transitive_using {
                add_scoped(
                    &mut scoped_joins,
                    &mut role_playing_bare_aliases,
                    &using_rel,
                );
            }
        }
    }

    // Collect needed bare aliases from source_table fields (lowercased).
    let mut needed: HashSet<String> = HashSet::new();
    // Phase 6.3b: a dimension reachable only THROUGH a role-played table is
    // joined under each role's scoped chain (`r__dep` off `a__dep`), never
    // bare: a bare walk would hang it off the first-declared relationship.
    // `find_using_context` has already required a single role for it.
    let mut descendant_tables: Vec<(String, String)> = Vec::new();
    for dim in resolved_dims {
        if let Some(ref st) = dim.source_table {
            let alias = st.to_ascii_lowercase();
            if let Some(rp) = super::role_playing::role_playing_on_path("", def, &alias)
                .ok()
                .flatten()
                .filter(|rp| *rp != alias && role_playing_bare_aliases.contains(rp))
            {
                if !descendant_tables.iter().any(|(_, d)| *d == alias) {
                    descendant_tables.push((rp, alias));
                }
                continue;
            }
            if alias != *root {
                // If this is a role-playing table, the dimension alias will be resolved
                // via find_using_context() in expand(). Don't add the bare alias here;
                // the scoped aliases are already tracked.
                if !role_playing_bare_aliases.contains(&alias) {
                    needed.insert(alias);
                }
            }
        }
    }
    for met in resolved_mets {
        if met.using_relationships.is_empty() {
            if let Some(ref st) = met.source_table {
                let alias = st.to_ascii_lowercase();
                if alias != *root {
                    needed.insert(alias);
                }
            } else {
                // Derived metric without direct USING: walk dependency graph for bare tables
                let transitive_tables = collect_derived_metric_source_tables(met, &def.metrics);
                for alias in transitive_tables {
                    if alias != *root && !role_playing_bare_aliases.contains(&alias) {
                        needed.insert(alias);
                    }
                }
            }
        }
        // Metrics WITH using_relationships: their source_table is the base table
        // (e.g., "f" for flights). Only add if it's not root.
        if !met.using_relationships.is_empty() {
            if let Some(ref st) = met.source_table {
                let alias = st.to_ascii_lowercase();
                if alias != *root {
                    needed.insert(alias);
                }
            }
        }
    }

    // PAR-6 (TECH-DEBT #53): a member's *expression* may reference a fact
    // declared on another logical table — Snowflake's supported way to cross
    // tables. `inline_facts` splices that fact's expression in at the reference
    // site, so its table has to be joined too or the emitted SQL names an alias
    // that is not in scope. Collected from the expression rather than from
    // `source_table`, which by construction only ever names the member's own
    // table.
    let add_referenced_fact_tables = |expr: &str, needed: &mut HashSet<String>| {
        for alias in collect_referenced_fact_tables(expr, &def.facts) {
            if alias != *root && !role_playing_bare_aliases.contains(&alias) {
                needed.insert(alias);
            }
        }
    };
    // Dimensions as well as metrics, since TECH-DEBT #54: a dimension's
    // expression is now fact-inlined on the same footing, so a fact it reaches
    // on another table has to be joined for the spliced expression to bind.
    // (#200 collected these before the inlining existed, which was the wrong
    // order -- it joined a relation to satisfy a reference that was never
    // substituted. With inlining in place the collection is what makes the
    // reference work.)
    for dim in resolved_dims {
        add_referenced_fact_tables(&dim.expr, &mut needed);
    }
    for met in resolved_mets {
        add_referenced_fact_tables(&met.expr, &mut needed);
        // A derived metric's own expression names metrics, not facts; the fact
        // references live in the base metrics it resolves to, whose expressions
        // are inlined into it before the facts pass runs.
        if met.source_table.is_none() {
            for name in collect_transitive_metric_names(met, &def.metrics) {
                if let Some(base) = def
                    .metrics
                    .iter()
                    .find(|m| crate::ident::normalize_ident_part(&m.name) == name)
                {
                    add_referenced_fact_tables(&base.expr, &mut needed);
                }
            }
        }
    }

    // Fact source tables (facts path only), in declaration order.
    let mut fact_needed: Vec<String> = Vec::new();
    for st in fact_source_tables {
        let alias = st.to_ascii_lowercase();
        if alias != *root && !fact_needed.contains(&alias) {
            fact_needed.push(alias);
        }
    }

    // If no bare aliases, fact aliases, or scoped aliases needed, return empty
    if needed.is_empty()
        && scoped_joins.is_empty()
        && fact_needed.is_empty()
        && descendant_tables.is_empty()
    {
        return Vec::new();
    }

    // Map each reachable alias to its connecting edge on the path to the root.
    let tree_parent = build_tree_parents(def, root);

    // Include every table on the path between the root and each needed alias.
    let mut all_needed: HashSet<String> = HashSet::new();
    for alias in &needed {
        if tree_parent.contains_key(alias) {
            // Reachable: walk the parent chain to the root, adding intermediaries.
            let mut current = alias.clone();
            while current != *root && all_needed.insert(current.clone()) {
                let Some((parent, _)) = tree_parent.get(&current) else {
                    break;
                };
                current = parent.clone();
            }
        } else {
            // Disconnected from the root (degenerate definition): legacy
            // reverse-edge walk, preserving pre-existing emission behavior.
            all_needed.insert(alias.clone());
            let mut to_visit = vec![alias.clone()];
            while let Some(current) = to_visit.pop() {
                if let Some(parents) = graph.reverse.get(&current) {
                    for parent in parents {
                        if parent != root && all_needed.insert(parent.clone()) {
                            to_visit.push(parent.clone());
                        }
                    }
                }
            }
        }
    }

    // Base ordering: topological order over the PK/FK graph.
    let Ok(topo_order) = graph.toposort() else {
        return Vec::new(); // Should not happen -- validated at define time
    };
    let mut ordered: Vec<String> = topo_order
        .into_iter()
        .filter(|alias| all_needed.contains(alias))
        .collect();

    // Stable re-sort: each reachable alias's tree parent must be emitted first
    // (or be the root). Kahn order already satisfies this for root-outward
    // trees; FK-side chains below the root need the fix (SG-10).
    let mut emission: Vec<String> = Vec::with_capacity(ordered.len());
    while !ordered.is_empty() {
        let pos = ordered
            .iter()
            .position(|alias| match tree_parent.get(alias) {
                Some((parent, _)) => parent == root || !ordered.contains(parent),
                None => true, // Disconnected: no ordering constraint
            });
        // Reachable aliases always have their full parent chain included, so
        // some alias is always emittable; fall back to the front defensively.
        emission.push(ordered.remove(pos.unwrap_or(0)));
    }

    let mut result: Vec<ResolvedJoin<'a>> = Vec::new();
    for alias in emission {
        push_bare_join(&mut result, def, &tree_parent, alias);
    }

    // Fact-driven joins: appended after dimension-driven joins, each with its
    // path intermediaries in root-outward order (SG-10 on the facts path).
    for alias in fact_needed {
        if result.iter().any(|rj| rj.emit_alias == alias) {
            continue;
        }
        if tree_parent.contains_key(&alias) {
            let mut path: Vec<String> = Vec::new();
            let mut current = alias;
            while current != *root {
                path.push(current.clone());
                let Some((parent, _)) = tree_parent.get(&current) else {
                    break;
                };
                current = parent.clone();
            }
            for path_alias in path.into_iter().rev() {
                if result.iter().any(|rj| rj.emit_alias == path_alias) {
                    continue;
                }
                push_bare_join(&mut result, def, &tree_parent, path_alias);
            }
        } else {
            push_bare_join(&mut result, def, &tree_parent, alias);
        }
    }

    // Sort scoped joins by alias for deterministic output, then append.
    scoped_joins.sort_by(|a, b| a.0.cmp(&b.0));
    for (scoped_alias, join, rel) in &scoped_joins {
        result.push(ResolvedJoin {
            bare_alias: join.table.to_ascii_lowercase(),
            emit_alias: scoped_alias.clone(),
            join,
            scoped: true,
            from_emit: None,
        });
        // Each descendant dimension's chain continues from this instance.
        let target = join.table.to_ascii_lowercase();
        for (rp, table) in &descendant_tables {
            if *rp != target {
                continue;
            }
            for edge in super::role_playing::descendant_chain(def, rp, table).unwrap_or_default() {
                let emit_alias = scoped_join_alias(&edge.table, rel);
                if result.iter().any(|rj| rj.emit_alias == emit_alias) {
                    continue;
                }
                result.push(ResolvedJoin {
                    bare_alias: edge.table.to_ascii_lowercase(),
                    emit_alias,
                    join: edge,
                    scoped: true,
                    from_emit: Some(scoped_join_alias(&edge.from_alias, rel)),
                });
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expand::test_helpers::{orders_view, TestFixtureExt};
    use crate::model::{Join, SemanticViewDefinition, TableRef};

    /// Phase 7.1: the orders -> addresses ASOF relationship used below.
    fn asof_def(orders_first: bool) -> SemanticViewDefinition {
        let o = TableRef {
            alias: "o".to_string(),
            table: "orders".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        };
        let a = TableRef {
            alias: "a".to_string(),
            table: "addresses".to_string(),
            pk_columns: vec!["cust".to_string(), "start_date".to_string()],
            ..Default::default()
        };
        SemanticViewDefinition {
            tables: if orders_first { vec![o, a] } else { vec![a, o] },
            joins: vec![Join {
                from_alias: "o".to_string(),
                table: "a".to_string(),
                fk_columns: vec!["cust".to_string(), "order_date".to_string()],
                ref_columns: vec!["cust".to_string(), "start_date".to_string()],
                name: Some("order_address".to_string()),
                asof: Some(1),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn render(def: &SemanticViewDefinition, joined: &str) -> String {
        let rj = ResolvedJoin {
            emit_alias: joined.to_string(),
            bare_alias: joined.to_string(),
            join: &def.joins[0],
            scoped: false,
            from_emit: None,
        };
        let mut sql = String::new();
        push_join_clauses(&mut sql, &[rj], def, "\nLEFT JOIN ");
        sql
    }

    /// Joining the referenced table: an `ASOF LEFT JOIN` whose ASOF pair is
    /// the `>=` match condition.
    #[test]
    fn asof_relationship_joins_its_referenced_table_with_asof_left_join() {
        let def = asof_def(true);
        assert_eq!(
            render(&def, "a"),
            "\nASOF LEFT JOIN \"addresses\" AS \"a\" ON \"o\".\"cust\" = \"a\".\"cust\" \
             AND \"o\".\"order_date\" >= \"a\".\"start_date\""
        );
    }

    /// Joining the referencing table onto the referenced one: the derived
    /// table carrying each row's ASOF match, joined on equality.
    #[test]
    fn asof_relationship_walked_backwards_joins_the_matched_rows() {
        let def = asof_def(false);
        let sql = render(&def, "o");
        assert!(
            sql.starts_with("\nLEFT JOIN (SELECT \"__sv_asof_from\".*"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "ASOF JOIN \"addresses\" AS \"__sv_asof_to\" ON \"__sv_asof_from\".\"cust\" = \
                 \"__sv_asof_to\".\"cust\" AND \"__sv_asof_from\".\"order_date\" >= \
                 \"__sv_asof_to\".\"start_date\")"
            ),
            "{sql}"
        );
        assert!(
            sql.ends_with(
                " AS \"o\" ON \"o\".\"cust\" = \"a\".\"cust\" AND \
                 \"o\".\"__sv_asof_match\" = \"a\".\"start_date\""
            ),
            "{sql}"
        );
    }

    fn range_def(events_first: bool) -> SemanticViewDefinition {
        let e = TableRef {
            alias: "e".to_string(),
            table: "events".to_string(),
            pk_columns: vec!["e_id".to_string()],
            ..Default::default()
        };
        let p = TableRef {
            alias: "p".to_string(),
            table: "periods".to_string(),
            pk_columns: vec!["period_id".to_string()],
            distinct_ranges: vec![crate::model::DistinctRange {
                name: "period_range".to_string(),
                start: "start_ts".to_string(),
                end: "end_ts".to_string(),
            }],
            ..Default::default()
        };
        SemanticViewDefinition {
            tables: if events_first { vec![e, p] } else { vec![p, e] },
            joins: vec![Join {
                from_alias: "e".to_string(),
                table: "p".to_string(),
                fk_columns: vec!["e_ts".to_string()],
                ref_columns: vec!["start_ts".to_string()],
                name: Some("event_period".to_string()),
                range_end: Some("end_ts".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    const RANGE_ON: &str =
        "((CASE WHEN \"e\".\"e_ts\" IS NOT NULL THEN ROW(0, \"e\".\"e_ts\") END) \
                            >= ROW(CASE WHEN \"p\".\"start_ts\" IS NULL THEN -1 ELSE 0 END, \
                            \"p\".\"start_ts\") AND (CASE WHEN \"e\".\"e_ts\" IS NOT NULL THEN \
                            ROW(0, \"e\".\"e_ts\") END) < ROW(CASE WHEN \"p\".\"end_ts\" IS NULL \
                            THEN 1 ELSE 0 END, \"p\".\"end_ts\"))";

    /// Phase 7.2: a range relationship joins the referenced table on its
    /// half-open period, a NULL bound open.
    #[test]
    fn range_relationship_joins_on_its_half_open_period() {
        assert_eq!(
            render(&range_def(true), "p"),
            format!("\nLEFT JOIN \"periods\" AS \"p\" ON {RANGE_ON}")
        );
    }

    /// Reached through a role-played alias, the condition names that alias,
    /// not the table's own (PR #338 review).
    #[test]
    fn range_relationship_on_a_scoped_alias_names_the_alias() {
        let def = range_def(true);
        let on = synthesize_on_clause_scoped(&def.joins[0], &def.tables, None, "p__event_period");
        assert_eq!(on, RANGE_ON.replace("\"p\".", "\"p__event_period\"."));
    }

    /// Walked the other way, the same condition: it names both sides.
    #[test]
    fn range_relationship_walked_backwards_has_the_same_condition() {
        assert_eq!(
            render(&range_def(false), "e"),
            format!("\nLEFT JOIN \"events\" AS \"e\" ON {RANGE_ON}")
        );
    }

    /// A plain relationship is unchanged.
    #[test]
    fn plain_relationship_still_joins_with_left_join() {
        let mut def = asof_def(true);
        def.joins[0].asof = None;
        assert_eq!(
            render(&def, "a"),
            "\nLEFT JOIN \"addresses\" AS \"a\" ON \"o\".\"cust\" = \"a\".\"cust\" \
             AND \"o\".\"order_date\" = \"a\".\"start_date\""
        );
    }

    #[test]
    fn test_synthesize_on_clause_single_column() {
        let join = Join {
            table: "customers".to_string(),
            from_alias: "orders".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "customers".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause(&join, &tables);
        assert_eq!(result, r#""orders"."customer_id" = "customers"."id""#);
    }

    #[test]
    fn test_synthesize_on_clause_composite_keys() {
        let join = Join {
            table: "target".to_string(),
            from_alias: "source".to_string(),
            fk_columns: vec!["a".to_string(), "b".to_string()],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "target".to_string(),
            table: "target".to_string(),
            pk_columns: vec!["x".to_string(), "y".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause(&join, &tables);
        assert!(
            result.contains(r#"= "target"."x""#),
            "Should contain target.x"
        );
        assert!(
            result.contains("AND"),
            "Should contain AND for composite keys"
        );
        assert!(
            result.contains(r#"= "target"."y""#),
            "Should contain target.y"
        );
    }

    #[test]
    fn test_synthesize_on_clause_empty_fk_columns() {
        let join = Join {
            table: "customers".to_string(),
            from_alias: "orders".to_string(),
            fk_columns: vec![],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "customers".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause(&join, &tables);
        assert_eq!(
            result, "",
            "Empty fk_columns should produce empty ON clause"
        );
    }

    #[test]
    fn test_synthesize_on_clause_scoped_uses_to_alias() {
        let join = Join {
            table: "airports".to_string(),
            from_alias: "flights".to_string(),
            fk_columns: vec!["dep_airport_code".to_string()],
            ref_columns: vec!["code".to_string()],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "airports".to_string(),
            table: "airports".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause_scoped(&join, &tables, None, "airports__dep_airport");
        assert!(
            result.contains(r#""airports__dep_airport""#),
            "Should use scoped alias, got: {result}"
        );
        assert!(
            !result.contains(r#""airports"."#),
            "Should not use bare table alias on right side"
        );
    }

    #[test]
    fn test_synthesize_on_clause_scoped_prefers_ref_columns() {
        let join = Join {
            table: "airports".to_string(),
            from_alias: "flights".to_string(),
            fk_columns: vec!["dep_code".to_string()],
            ref_columns: vec!["code".to_string()],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "airports".to_string(),
            table: "airports".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause_scoped(&join, &tables, None, "airports__dep");
        assert!(
            result.contains(r#""code""#),
            "Should use ref_columns, got: {result}"
        );
        assert!(
            !result.contains(r#""id""#),
            "Should not use pk_columns when ref_columns are present"
        );
    }

    /// Key columns are stored as written, so a quoted one keeps its quotes;
    /// the ON clause must not quote it a second time.
    #[test]
    fn test_synthesize_on_clause_quoted_key_columns_are_quoted_once() {
        let join = Join {
            table: "p".to_string(),
            from_alias: "c".to_string(),
            fk_columns: vec!["\"Parent Ref\"".to_string()],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "p".to_string(),
            table: "parent".to_string(),
            pk_columns: vec!["\"Key Col\"".to_string()],
            ..Default::default()
        }];
        assert_eq!(
            synthesize_on_clause_scoped(&join, &tables, None, "p"),
            r#""c"."Parent Ref" = "p"."Key Col""#
        );
    }

    #[test]
    fn test_synthesize_on_clause_scoped_falls_back_to_pk() {
        let join = Join {
            table: "airports".to_string(),
            from_alias: "flights".to_string(),
            fk_columns: vec!["dep_code".to_string()],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "airports".to_string(),
            table: "airports".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause_scoped(&join, &tables, None, "airports__dep");
        assert!(
            result.contains(r#""id""#),
            "Should fall back to pk_columns when ref_columns are empty, got: {result}"
        );
    }

    #[test]
    fn test_resolve_joins_pkfk_no_joins() {
        let def = orders_view();
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = resolve_joins_pkfk(&def, &resolved_dims, &resolved_mets, &[]);
        assert!(result.is_empty(), "No joins should produce empty result");
    }

    #[test]
    fn test_resolve_joins_pkfk_single_join() {
        let def = orders_view()
            .with_table("orders", "orders", &["id"])
            .with_table("customers", "customers", &["id"])
            .with_dimension("cust_name", "name", Some("customers"))
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            );
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = resolve_joins_pkfk(&def, &resolved_dims, &resolved_mets, &[]);
        assert!(
            result.iter().any(|rj| rj.emit_alias == "customers"),
            "Should include customers join, got: {:?}",
            result.iter().map(|rj| &rj.emit_alias).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_resolve_joins_pkfk_with_using_relationship() {
        // Build a role-playing scenario: flights -> airports via dep_airport and arr_airport
        let def = orders_view()
            .with_table("flights", "flights", &[])
            .clear_dimensions()
            .clear_metrics()
            .with_table("flights", "flights", &["id"])
            .with_table("airports", "airports", &["code"])
            .with_dimension("airport_name", "name", Some("airports"))
            .with_metric("flight_count", "count(*)", Some("flights"))
            .with_using_relationship("flight_count", &["dep_airport"])
            .with_pkfk_join(
                "dep_airport",
                "flights",
                "airports",
                &["dep_code"],
                &["code"],
            )
            .with_pkfk_join(
                "arr_airport",
                "flights",
                "airports",
                &["arr_code"],
                &["code"],
            );
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = resolve_joins_pkfk(&def, &resolved_dims, &resolved_mets, &[]);
        assert!(
            result
                .iter()
                .any(|rj| rj.emit_alias == "airports__dep_airport" && rj.scoped),
            "Should include scoped alias airports__dep_airport, got: {:?}",
            result.iter().map(|rj| &rj.emit_alias).collect::<Vec<_>>()
        );
    }

    /// A stored ASOF marker that names no referenced column (an index past
    /// the key, or a key that is empty) cannot be emitted as an ASOF join.
    /// Expansion refuses the definition instead of rendering a plain
    /// equality join, or tripping the emission asserts (the fuzz crash on
    /// PR #336).
    fn asof_query(def: &SemanticViewDefinition, dim_table: &str) -> Result<String, ExpandError> {
        use crate::expand::{expand, DimensionName, QueryRequest};
        use crate::model::Dimension;
        let mut def = def.clone();
        def.dimensions = vec![Dimension {
            name: "d".to_string(),
            expr: format!("{dim_table}.cust"),
            source_table: Some(dim_table.to_string()),
            ..Default::default()
        }];
        let req = QueryRequest {
            dimensions: vec![DimensionName::new("d")],
            ..Default::default()
        };
        expand("v", &def, &req)
    }

    #[test]
    fn asof_index_past_the_key_is_refused_forward() {
        let mut def = asof_def(true);
        def.joins[0].asof = Some(7);
        let err = asof_query(&def, "a").expect_err("out-of-range ASOF index expanded");
        assert!(
            matches!(err, ExpandError::UncheckableDefinition { .. }),
            "{err}"
        );
    }

    #[test]
    fn asof_index_past_the_key_is_refused_backwards() {
        let mut def = asof_def(false);
        def.joins[0].asof = Some(7);
        let err = asof_query(&def, "o").expect_err("out-of-range ASOF index expanded");
        assert!(
            matches!(err, ExpandError::UncheckableDefinition { .. }),
            "{err}"
        );
    }

    #[test]
    fn range_relationship_with_two_columns_is_refused() {
        let mut def = range_def(true);
        def.joins[0].fk_columns.push("x".to_string());
        def.joins[0].ref_columns.push("y".to_string());
        let err = asof_query(&def, "e").expect_err("two-column range expanded");
        assert!(
            matches!(err, ExpandError::UncheckableDefinition { .. }),
            "{err}"
        );
    }

    #[test]
    fn asof_on_an_empty_key_is_refused() {
        let mut def = asof_def(false);
        def.joins[0].ref_columns.clear();
        def.tables[0].pk_columns.clear();
        def.joins[0].asof = Some(0);
        let err = asof_query(&def, "o").expect_err("ASOF on an empty key expanded");
        assert!(
            matches!(err, ExpandError::UncheckableDefinition { .. }),
            "{err}"
        );
    }
}
