use crate::model::{AccessModifier, Dimension, Fact, Metric, SemanticViewDefinition};
use crate::util::suggest_closest;

use super::facts::{
    collect_transitive_metric_names, inline_derived_metrics_with_joins, inline_facts,
    toposort_facts,
};
use super::fan_trap::{check_fan_traps, validate_fact_table_path};
use super::join_resolver::resolve_joins_pkfk;
use super::resolution::{find_dimension, find_fact, find_metric, quote_stored_ident};
use super::role_playing::{
    check_fact_role_playing_path, check_where_clause_role_playing_path, find_using_context,
};
use super::select_spec::{FromSource, GroupBy, SelectItem, SelectSpec};
use super::types::{ExpandError, QueryRequest, ResolvedDim};

/// An entity kind resolvable by name against a [`SemanticViewDefinition`]
/// (dimensions, metrics, facts). Encapsulates lookup, the PRIVATE-access
/// policy, and the three error variants so [`resolve_names`] takes the
/// definition plus the requested names — not nine positional closures (R-5).
///
/// Modelling the error variants per kind is what makes a slot transposition
/// unrepresentable: the old positional API let the dimension call sites pass
/// `DuplicateDimension` in the private-error slot (harmless only because
/// dimensions are never private), a mistake the compiler could not catch.
trait Resolvable: Sized {
    /// Find this entity by (possibly qualified) name in the definition.
    fn find<'a>(def: &'a SemanticViewDefinition, name: &str) -> Option<&'a Self>;
    /// Is this resolved entity PRIVATE — barred from direct querying?
    fn is_private(&self) -> bool;
    /// All declared names of this kind, for the not-found error + suggestion.
    fn available(def: &SemanticViewDefinition) -> Vec<String>;
    /// Error: the same entity was requested twice (keyed on resolved identity).
    fn duplicate_err(view_name: String, name: String) -> ExpandError;
    /// Error: no entity of this kind by that name.
    fn unknown_err(
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    ) -> ExpandError;
    /// Error: the entity is PRIVATE. Never called for kinds whose
    /// [`is_private`](Self::is_private) is always `false`.
    fn private_err(view_name: String, name: String) -> ExpandError;
}

impl Resolvable for Fact {
    fn find<'a>(def: &'a SemanticViewDefinition, name: &str) -> Option<&'a Self> {
        find_fact(def, name)
    }
    fn is_private(&self) -> bool {
        self.access == AccessModifier::Private
    }
    fn available(def: &SemanticViewDefinition) -> Vec<String> {
        def.facts.iter().map(|f| f.name.clone()).collect()
    }
    fn duplicate_err(view_name: String, name: String) -> ExpandError {
        ExpandError::DuplicateFact { view_name, name }
    }
    fn unknown_err(
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    ) -> ExpandError {
        ExpandError::UnknownFact {
            view_name,
            name,
            available,
            suggestion,
        }
    }
    fn private_err(view_name: String, name: String) -> ExpandError {
        ExpandError::PrivateFact { view_name, name }
    }
}

impl Resolvable for Dimension {
    fn find<'a>(def: &'a SemanticViewDefinition, name: &str) -> Option<&'a Self> {
        find_dimension(def, name)
    }
    fn is_private(&self) -> bool {
        // Dimensions carry no access modifier — never private.
        false
    }
    fn available(def: &SemanticViewDefinition) -> Vec<String> {
        def.dimensions.iter().map(|d| d.name.clone()).collect()
    }
    fn duplicate_err(view_name: String, name: String) -> ExpandError {
        ExpandError::DuplicateDimension { view_name, name }
    }
    fn unknown_err(
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    ) -> ExpandError {
        ExpandError::UnknownDimension {
            view_name,
            name,
            available,
            suggestion,
        }
    }
    fn private_err(_view_name: String, _name: String) -> ExpandError {
        // `is_private` is always false for dimensions, so `resolve_names`
        // never reaches this. There is no `PrivateDimension` variant; the old
        // positional API filled this slot with `DuplicateDimension` (dead but
        // misleading) — the trait removes the footgun entirely.
        unreachable!("dimensions cannot be private")
    }
}

impl Resolvable for Metric {
    fn find<'a>(def: &'a SemanticViewDefinition, name: &str) -> Option<&'a Self> {
        find_metric(def, name)
    }
    fn is_private(&self) -> bool {
        self.access == AccessModifier::Private
    }
    fn available(def: &SemanticViewDefinition) -> Vec<String> {
        def.metrics.iter().map(|m| m.name.clone()).collect()
    }
    fn duplicate_err(view_name: String, name: String) -> ExpandError {
        ExpandError::DuplicateMetric { view_name, name }
    }
    fn unknown_err(
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    ) -> ExpandError {
        ExpandError::UnknownMetric {
            view_name,
            name,
            available,
            suggestion,
        }
    }
    fn private_err(view_name: String, name: String) -> ExpandError {
        ExpandError::PrivateMetric { view_name, name }
    }
}

/// Resolve a list of requested names to their [`Resolvable`] definitions,
/// checking for unknown names, duplicates, and PRIVATE access.
///
/// Duplicate detection keys on the RESOLVED item's identity, not the raw
/// request string (SG-14): `region` and `o.region` resolve to the same
/// dimension and are rejected as duplicates instead of emitting the same
/// column twice.
fn resolve_names<'a, T: Resolvable, N: AsRef<str>>(
    names: &[N],
    view_name: &str,
    def: &'a SemanticViewDefinition,
) -> Result<Vec<&'a T>, ExpandError> {
    let mut resolved = Vec::with_capacity(names.len());
    let mut seen: std::collections::HashSet<*const T> = std::collections::HashSet::new();
    for name in names {
        let name_str = name.as_ref();
        let item = T::find(def, name_str).ok_or_else(|| {
            let available = T::available(def);
            let suggestion = suggest_closest(name_str, &available);
            T::unknown_err(
                view_name.to_string(),
                name_str.to_string(),
                available,
                suggestion,
            )
        })?;
        if !seen.insert(std::ptr::from_ref(item)) {
            return Err(T::duplicate_err(
                view_name.to_string(),
                name_str.to_string(),
            ));
        }
        if item.is_private() {
            return Err(T::private_err(view_name.to_string(), name_str.to_string()));
        }
        resolved.push(item);
    }
    Ok(resolved)
}

/// The `<child pk> IS NOT NULL` predicate that removes phantom (NULL-extended)
/// rows from a ROW-LEVEL query, or `None` when there is nothing to remove
/// (EXP-28, EXP-29 — code-review 2026-08-08).
///
/// Every synthesized join is a LEFT JOIN anchored at the base table, so a base
/// row with no matching child row survives as one all-NULL row. An AGGREGATE
/// query fences that row inside each aggregate (SG-8 + `guard_aggregate_args`),
/// but a fact query and a dimensions-only `SELECT DISTINCT` emit rows
/// directly — the phantom becomes a result row that does not exist in the data,
/// and its NULLs are indistinguishable from genuine ones.
///
/// `member_tables` are the `source_table`s of the queried members (`None` means
/// the base table, which is never NULL-extended). The guard applies only when
/// they all name ONE table that is **below** the base — reached by following
/// foreign keys INTO the base, so it sits at a finer grain — and that table
/// declares a PRIMARY KEY. That table is then the query's grain, and filtering
/// to its real rows is exactly "anchor at the common grain table, joining up
/// for the rest", which stays fan-free because the walk from a child to its
/// parents is many-to-one.
///
/// The direction is load-bearing. A member on a table ABOVE the base (a parent,
/// reached by the base's own foreign key) is an ATTRIBUTE of each base row: a
/// base row whose foreign key is NULL or dangling is still a row of the view,
/// and its NULL attribute is part of the answer, not a join artifact. Filtering
/// there would delete real rows — `multi_hop_join_proptest` catches it
/// immediately. Below the base the situation is reversed: the base row has no
/// counterpart at the finer grain at all, so the NULL-extended row is
/// manufactured.
///
/// When the members span several tables the natural grain is ambiguous (a base
/// member legitimately keeps its row even with no child), and when the table
/// declares no PRIMARY KEY there is no column that distinguishes a phantom;
/// both keep today's behaviour. See TECH-DEBT #67.
fn phantom_row_filter(
    def: &SemanticViewDefinition,
    member_tables: &[Option<String>],
) -> Option<String> {
    let base = def.tables.first()?.alias.to_ascii_lowercase();
    let mut grain: Option<String> = None;
    for member in member_tables {
        let alias = member
            .as_ref()
            .map_or_else(|| base.clone(), |source| source.to_ascii_lowercase());
        match &grain {
            None => grain = Some(alias),
            Some(seen) if *seen == alias => {}
            Some(_) => return None, // members span several tables
        }
    }
    let alias = grain?;
    if alias == base || !reaches_base_by_foreign_key(def, &alias, &base) {
        return None;
    }
    let pk = def
        .tables
        .iter()
        .find(|t| t.alias.to_ascii_lowercase() == alias)?
        .pk_columns
        .first()?;
    Some(format!(
        "{}.{} IS NOT NULL",
        quote_stored_ident(&alias),
        quote_stored_ident(pk)
    ))
}

/// Which table a query with no metrics reads from (see [`member_anchor`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Anchor {
    /// Read the rows of this table and join up from it.
    At(String),
    /// No table reaches every member, but their paths meet at this table,
    /// which has a metric: list the distinct member tuples it reaches, every
    /// hop an inner join (Phase 7 PR 4d, see [`meeting_table`]).
    Meet(String),
    /// No table reaches both of these members, so no rows hold them together.
    Unrelated(String, String),
    /// The anchor reaches a member's table by more than one path (a diamond),
    /// and a query with no metrics has no `USING` to choose one: Snowflake
    /// refuses it (010246). Holds the anchor, the member's table, and the paths.
    MultiPath(String, String, Vec<String>),
    /// Several tables reach every member equally well, none above another
    /// (two bridges between books and authors pair each book with its authors
    /// twice over): Snowflake refuses it (000002, multi-path in a many-to-many
    /// relationship). Holds the first member's table, the second's, and the
    /// route through each such table.
    Paired(String, String, Vec<String>),
    /// Not decided here: the base-anchored emission answers, or refuses.
    Undecided,
}

/// The tables a dimensions-only query's members live on: each dimension's
/// table, the tables of the facts it is written over, and every filtered
/// table. `None` when a dimension or a filter member names no table, so its
/// rows cannot be placed.
pub(super) fn dims_only_members(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    where_tables: &[String],
    where_has_unqualified_member: bool,
) -> Option<Vec<String>> {
    if where_has_unqualified_member {
        return None;
    }
    let mut members: Vec<String> = Vec::new();
    for dim in resolved_dims {
        members.push(dim.source_table.clone()?);
        members.extend(super::facts::collect_referenced_fact_tables(
            &dim.expr, &def.facts,
        ));
    }
    members.extend(where_tables.iter().cloned());
    Some(members)
}

/// The table a query with no metrics reads from.
///
/// Snowflake lists the rows of the one table that every member's table (each
/// queried dimension or fact, each fact one is written over, each filtered
/// table) can be reached from by following foreign keys (many-to-one), and
/// joins up from it (the 2026-11-09 dims-only fixtures). So one table lists its
/// own values, a parent and a child list the child's rows, and the two sides of
/// a bridge list the bridge's rows, whichever table is declared first. Reading
/// from the base table instead made the answer depend on declaration order.
///
/// Of the tables that reach every member, the one closest to them is chosen:
/// the candidate no other candidate sits above.
///
/// - When no table reaches every member (two children of one parent), but
///   their paths meet at a table that a table with a metric reaches them
///   through, the answer is
///   [`Anchor::Meet`] (see [`meeting_table`]). Otherwise it is
///   [`Anchor::Unrelated`]: Snowflake refuses such a query ("the entities are
///   not related").
/// - When a hop on the way from the anchor to a member has more than one
///   relationship (a role-played table, queried from below), the answer is
///   [`Anchor::Undecided`]: no relationship picks the role, and the
///   base-anchored path refuses it as ambiguous, as Snowflake does. A
///   role-played table queried from itself or from above crosses no such hop,
///   and is answered.
/// - When the anchor reaches a member's table by two paths through different
///   tables (a diamond), the answer is [`Anchor::MultiPath`]: Snowflake refuses
///   it, and nothing in a query with no metrics can choose a path.
/// - When two tables do equally well (two bridges between the members' tables),
///   the answer is [`Anchor::Paired`]: Snowflake refuses it, and which table
///   pairs the rows would depend on declaration order.
/// - When the graph cannot be built, the answer is [`Anchor::Undecided`].
pub(super) fn member_anchor(def: &SemanticViewDefinition, members: &[String]) -> Anchor {
    let mut members: Vec<String> = members.iter().map(|m| m.to_ascii_lowercase()).collect();
    members.sort();
    members.dedup();
    if members.is_empty() {
        return Anchor::Undecided;
    }
    let reaches = |from: &str, to: &str| from == to || reaches_base_by_foreign_key(def, from, to);
    let tables: Vec<String> = def
        .tables
        .iter()
        .map(|t| t.alias.to_ascii_lowercase())
        .collect();
    let candidates: Vec<&String> = tables
        .iter()
        .filter(|t| members.iter().all(|m| reaches(t, m)))
        .collect();
    if candidates.is_empty() {
        if let Some(meet) = meeting_table(def, &members) {
            return Anchor::Meet(meet);
        }
        for (i, a) in members.iter().enumerate() {
            for b in &members[i + 1..] {
                if !tables.iter().any(|t| reaches(t, a) && reaches(t, b)) {
                    return Anchor::Unrelated(a.clone(), b.clone());
                }
            }
        }
        return Anchor::Undecided;
    }
    let closest: Vec<&&String> = candidates
        .iter()
        .filter(|t| !candidates.iter().any(|o| o != *t && reaches(t, o)))
        .collect();
    let [anchor] = closest.as_slice() else {
        // Sorted, so the message does not depend on declaration order.
        let mut pairing: Vec<&String> = closest.iter().map(|t| **t).collect();
        pairing.sort();
        let (first, second) = (&members[0], &members[members.len() - 1]);
        if first == second {
            return Anchor::Undecided;
        }
        let routes = super::multi_path::paired_routes(def, &pairing, first, second);
        return Anchor::Paired(first.clone(), second.clone(), routes);
    };
    for member in &members {
        if let Some(paths) = super::multi_path::anchor_paths(def, anchor, member) {
            return Anchor::MultiPath((**anchor).clone(), member.clone(), paths);
        }
    }
    let Some(graph) = super::fan_trap::GrainGraph::build(def) else {
        return Anchor::Undecided;
    };
    for member in &members {
        let Some(path) = graph.path(anchor, member) else {
            return Anchor::Undecided;
        };
        for pair in path.windows(2) {
            let edges = def
                .joins
                .iter()
                .filter(|j| {
                    let (f, t) = (
                        j.from_alias.to_ascii_lowercase(),
                        j.table.to_ascii_lowercase(),
                    );
                    (f == pair[0] && t == pair[1]) || (f == pair[1] && t == pair[0])
                })
                .count();
            if edges != 1 {
                return Anchor::Undecided;
            }
        }
    }
    Anchor::At((**anchor).clone())
}

/// The table a dimensions-only query's members meet at when no table reaches
/// them all by foreign keys, if Snowflake answers the query from it (Phase 7
/// PR 4d and 4e; fixture 7's D1-D10 and N8, fixture 2's W1-W4, fixture 8's
/// M1-M4).
///
/// The meeting table is the table on the paths between the members from which
/// each member is reached by fanning out and then climbing foreign keys,
/// never fanning out again. Of several (a member above it), the lowest. The
/// rows are the distinct member tuples it reaches, every hop inner-joined, so
/// no tuple is padded with NULLs for a member it reaches no row of: a book
/// with no author lists none of its reviews, an order with no notes none of
/// its items, an order with no region none of its items beside a region.
///
/// Snowflake answers such a query only when a table from which every member
/// is reached that way has a metric: the meeting table, or one above it (a
/// regions metric answers an item's qty with a note's kind, fixture 8's M2b,
/// though regions is not on their paths and is not joined). Without one it
/// refuses ("not related"), as for two children of a customer table with no
/// metric (W1-W4) or a metric only on the members' own tables (D1).
///
/// `None` when there is no meeting table or no such metric, when two members
/// are joined by more than one path, or when a hop on the way is a
/// role-played table's or an ASOF or range relationship's (fixture 8's T1-T3
/// refuse these).
fn meeting_table(def: &SemanticViewDefinition, members: &[String]) -> Option<String> {
    let mut between: Vec<String> = Vec::new();
    for (i, a) in members.iter().enumerate() {
        for b in &members[i + 1..] {
            if super::multi_path::two_undirected_paths(def, a, b).is_some() {
                return None;
            }
            for t in super::multi_path::undirected_path(def, a, b, None)? {
                if !between.contains(&t) {
                    between.push(t);
                }
            }
        }
    }
    between.sort();
    // `Some(true)` when `from` references `to` (a hop up), `Some(false)` when
    // `to` references `from` (a fan-out), `None` when the hop is not one plain
    // relationship.
    let climbs = |from: &str, to: &str| -> Option<bool> {
        let hops: Vec<&crate::model::Join> = def
            .joins
            .iter()
            .filter(|j| !j.fk_columns.is_empty())
            .filter(|j| {
                let (f, t) = (&j.from_alias, &j.table);
                (f.eq_ignore_ascii_case(from) && t.eq_ignore_ascii_case(to))
                    || (f.eq_ignore_ascii_case(to) && t.eq_ignore_ascii_case(from))
            })
            .collect();
        let [hop] = hops.as_slice() else {
            return None;
        };
        if hop.asof.is_some() || hop.range_end.is_some() {
            return None;
        }
        Some(hop.from_alias.eq_ignore_ascii_case(from))
    };
    // The hops from `from` to `to`, each `true` for a climb: `None` when one
    // is not a plain relationship.
    let hops = |from: &str, to: &str| -> Option<Vec<bool>> {
        let path = super::multi_path::undirected_path(def, from, to, None)?;
        path.windows(2).map(|w| climbs(&w[0], &w[1])).collect()
    };
    let fans_then_climbs = |from: &str, to: &str| {
        hops(from, to).is_some_and(|h| h.iter().skip_while(|up| !**up).all(|up| *up))
    };
    let only_fans = |from: &str, to: &str| hops(from, to).is_some_and(|h| h.iter().all(|up| !*up));
    let meeting: Vec<&String> = between
        .iter()
        .filter(|t| members.iter().all(|m| fans_then_climbs(t, m)))
        .collect();
    let lowest: Vec<&&String> = meeting
        .iter()
        .filter(|t| !meeting.iter().any(|o| o != *t && only_fans(t, o)))
        .collect();
    let [meet] = lowest.as_slice() else {
        return None;
    };
    // The metric may sit on any table every member is reached from that way,
    // on the paths or above them, each reached along one undirected path.
    let qualifies = |t: &str| {
        members.iter().all(|m| {
            super::multi_path::two_undirected_paths(def, t, m).is_none() && fans_then_climbs(t, m)
        })
    };
    def.metrics
        .iter()
        .filter_map(|m| m.source_table.as_deref().map(str::to_ascii_lowercase))
        .any(|t| qualifies(&t))
        .then(|| (**meet).clone())
}

/// Whether `alias` sits BELOW `base` in the relationship graph — i.e. following
/// its declared foreign keys upwards (`from_alias` -> `table`, the referencing
/// side to the referenced side) reaches the base table.
///
/// Breadth-first with a visited set, so a diamond costs no more than a chain and
/// a mis-declared cycle terminates instead of looping.
fn reaches_base_by_foreign_key(def: &SemanticViewDefinition, alias: &str, base: &str) -> bool {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::from([alias.to_ascii_lowercase()]);
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        for join in &def.joins {
            if join.from_alias.to_ascii_lowercase() != current {
                continue;
            }
            let referenced = join.table.to_ascii_lowercase();
            if referenced == base {
                return true;
            }
            queue.push_back(referenced);
        }
    }
    false
}

/// `AND` the phantom-row guard onto the query's own predicate.
///
/// The user's predicate is parenthesized so an `OR` at its top level cannot
/// swallow the guard. With no guard the predicate is passed through byte for
/// byte — the emitted SQL of every query that has no phantom to remove is
/// unchanged.
fn and_phantom_filter(predicate: Option<String>, guard: Option<String>) -> Option<String> {
    match (predicate, guard) {
        (Some(p), Some(g)) => Some(format!("({p}) AND {g}")),
        (Some(p), None) => Some(p),
        (None, g) => g,
    }
}

/// The refusal of a query with no metrics whose anchor reaches `member_table`
/// by several paths, naming a queried dimension on that table when there is
/// one (otherwise the member is a fact or a filter).
fn multi_path_error(
    view_name: &str,
    resolved_dims: &[&Dimension],
    anchor: String,
    member_table: String,
    paths: Vec<String>,
    many_to_many: bool,
) -> ExpandError {
    let member = resolved_dims
        .iter()
        .find(|d| {
            d.source_table
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case(&member_table))
        })
        .map_or_else(
            || "a queried member".to_string(),
            |d| format!("dimension '{}'", d.name),
        );
    ExpandError::MultiPath {
        detail: Box::new(super::types::MultiPathError {
            view_name: view_name.to_string(),
            member,
            from_table: anchor,
            reader: "the query (with no metrics)".to_string(),
            many_to_many,
            member_table,
            paths,
        }),
    }
}

/// One member of a FACTS list: a fact, or a dimension read row by row.
enum FactsItem<'a> {
    Dimension(&'a Dimension),
    Fact(&'a Fact),
}

/// Whether a FACTS-list name is a dimension's. Snowflake reads a dimension
/// listed in FACTS row by row (its `2026_07` behaviour change). A name that is a
/// fact's, or no member's, is taken as a fact's, so its errors are a fact's.
fn is_dimension_in_facts(def: &SemanticViewDefinition, name: &str) -> bool {
    Fact::find(def, name).is_none() && Dimension::find(def, name).is_some()
}

/// Resolve a FACTS list, in its order: each name to its fact, or to its
/// dimension (see [`is_dimension_in_facts`]).
fn resolve_facts_list<'a, N: AsRef<str>>(
    names: &[N],
    view_name: &str,
    def: &'a SemanticViewDefinition,
) -> Result<Vec<FactsItem<'a>>, ExpandError> {
    let is_dim: Vec<bool> = names
        .iter()
        .map(|n| is_dimension_in_facts(def, n.as_ref()))
        .collect();
    let mut dim_names: Vec<&str> = Vec::new();
    let mut fact_names: Vec<&str> = Vec::new();
    for (name, &d) in names.iter().zip(&is_dim) {
        if d { &mut dim_names } else { &mut fact_names }.push(name.as_ref());
    }
    // A name that is no member is reported as an unknown fact, with the
    // dimensions among the names it may have meant: FACTS takes those too.
    let mut facts = resolve_names::<Fact, _>(&fact_names, view_name, def)
        .map_err(|e| match e {
            ExpandError::UnknownFact {
                view_name,
                name,
                mut available,
                ..
            } => {
                available.extend(def.dimensions.iter().map(|d| d.name.clone()));
                let suggestion = suggest_closest(&name, &available);
                ExpandError::UnknownFact {
                    view_name,
                    name,
                    available,
                    suggestion,
                }
            }
            other => other,
        })?
        .into_iter();
    let mut dims = resolve_names::<Dimension, _>(&dim_names, view_name, def)?.into_iter();
    Ok(is_dim
        .into_iter()
        .filter_map(|d| {
            if d {
                dims.next().map(FactsItem::Dimension)
            } else {
                facts.next().map(FactsItem::Fact)
            }
        })
        .collect())
}

/// Expand a fact query into unaggregated SQL.
///
/// Facts are row-level expressions — the generated SQL has no GROUP BY and no
/// aggregation. Fact expressions are resolved via `inline_facts` (DAG resolution)
/// just like metric expansion inlines facts into aggregate expressions.
///
/// A dimension listed in FACTS is read row by row too, as Snowflake reads it:
/// it adds a column but does NOT trigger GROUP BY (unlike a metric query, where
/// dims + metrics => GROUP BY). The columns come in the list's order.
#[allow(clippy::too_many_lines)]
fn expand_facts(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &QueryRequest,
) -> Result<String, ExpandError> {
    // 1-2. Validate + resolve the FACTS list: facts, and dimensions read row
    // by row.
    let listed = resolve_facts_list(&req.facts, view_name, def)?;
    let resolved_facts: Vec<&Fact> = listed
        .iter()
        .filter_map(|i| match i {
            FactsItem::Fact(f) => Some(*f),
            FactsItem::Dimension(_) => None,
        })
        .collect();
    let resolved_dims: Vec<&Dimension> = listed
        .iter()
        .filter_map(|i| match i {
            FactsItem::Dimension(d) => Some(*d),
            FactsItem::Fact(_) => None,
        })
        .collect();

    // 2b. Resolve the pre-aggregation predicate, if any. Done before the path
    // check so the members it references take part in it: Snowflake's
    // same-logical-table rule counts WHERE-clause members explicitly ("all facts
    // and dimensions used in the query, INCLUDING those specified in the WHERE
    // clause"). A filter on a table that fans out against the queried facts is
    // the same row-multiplication hazard as selecting from it.
    let resolved_where = req
        .where_clause
        .as_deref()
        // A blank predicate is absent, not an empty condition: `Some("")` would
        // otherwise emit a bare `WHERE ` with nothing after it. The FFI maps an
        // empty parameter to None, but `expand()` is public and reachable
        // directly (found by `fuzz_where_predicate`).
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| super::where_clause::resolve_where_clause(view_name, def, raw))
        .transpose()?;

    // 3. Validate table path constraint (FACT-04).
    let fact_tables: Vec<String> = resolved_facts
        .iter()
        .filter_map(|f| f.source_table.clone())
        .collect();
    let mut dim_tables: Vec<String> = resolved_dims
        .iter()
        .filter_map(|d| d.source_table.clone())
        .collect();
    if let Some(rw) = &resolved_where {
        dim_tables.extend(rw.source_tables.iter().cloned());
    }
    validate_fact_table_path(view_name, def, &fact_tables, &dim_tables)?;

    // A fact query reads the rows of the table under all its members, as a
    // dimensions-only query does (`member_anchor`), but every row, not each
    // distinct one. The path check above has made sure one of the members is
    // that table. Anchored there, every hop is many-to-one with a single
    // relationship, so the role-playing checks below have nothing to catch.
    let where_has_unqualified_member = resolved_where
        .as_ref()
        .is_some_and(|w| w.members.iter().any(|m| m.table.is_none()));
    let mut members: Option<Vec<String>> =
        (!where_has_unqualified_member).then(|| dim_tables.clone());
    for fact in &resolved_facts {
        if let Some(m) = members.as_mut() {
            match &fact.source_table {
                Some(t) => {
                    m.push(t.clone());
                    m.extend(super::facts::collect_referenced_fact_tables(
                        &fact.expr, &def.facts,
                    ));
                }
                None => members = None,
            }
        }
    }
    for dim in &resolved_dims {
        if let Some(m) = members.as_mut() {
            if dim.source_table.is_none() {
                members = None;
            } else {
                m.extend(super::facts::collect_referenced_fact_tables(
                    &dim.expr, &def.facts,
                ));
            }
        }
    }
    let anchored = match members.map(|m| member_anchor(def, &m)) {
        Some(Anchor::At(anchor)) => Some(anchor),
        Some(Anchor::MultiPath(anchor, member_table, paths)) => {
            return Err(multi_path_error(
                view_name,
                &resolved_dims,
                anchor,
                member_table,
                paths,
                false,
            ));
        }
        Some(Anchor::Paired(first, second, routes)) => {
            return Err(multi_path_error(
                view_name,
                &resolved_dims,
                first,
                second,
                routes,
                true,
            ));
        }
        _ => {
            // Unplaced rows (a member declared on no table): two placed
            // members joined by two paths are still refused, as for a
            // dimensions-only query.
            let mut placed = dim_tables.clone();
            for fact in &resolved_facts {
                placed.extend(fact.source_table.clone());
                placed.extend(super::facts::collect_referenced_fact_tables(
                    &fact.expr, &def.facts,
                ));
            }
            for dim in &resolved_dims {
                placed.extend(super::facts::collect_referenced_fact_tables(
                    &dim.expr, &def.facts,
                ));
            }
            if let Some((from, member_table, paths)) =
                super::multi_path::between_members(def, &placed)
            {
                return Err(multi_path_error(
                    view_name,
                    &resolved_dims,
                    from,
                    member_table,
                    paths,
                    false,
                ));
            }
            None
        }
    };

    // 3b. Role-playing ambiguity detection (SG-17), mirroring the metrics
    // path in expand(). Fact queries carry no metrics, so there is never a
    // USING context to disambiguate: a dimension on a table reached by
    // multiple named relationships always raises AmbiguousPath here — the
    // same error the metrics path raises when no co-queried metric supplies
    // USING. Previously the facts path skipped this check and silently bound
    // the dimension to an arbitrary relationship edge.
    if anchored.is_none() {
        for dim in &resolved_dims {
            let _ = find_using_context(view_name, def, dim, &[])?;
        }
    }

    // 3c. EXP-5: a fact sourced on (or reached only through) a role-playing
    // table has no USING context to pick a role — reject rather than silently
    // binding to the first-declared relationship, mirroring the dimension
    // check above.
    if anchored.is_none() {
        for fact in &resolved_facts {
            check_fact_role_playing_path(
                view_name,
                def,
                &super::role_playing::query_starts(def, &[]),
                fact,
            )?;
        }
        // TECH-DEBT #69: a queried dimension's expression may reach a fact on a
        // role-playing path too (#54 inlines it), and a fact query has no USING.
        super::role_playing::check_member_fact_role_playing_path(
            view_name,
            def,
            &resolved_dims,
            &[],
            &[],
        )?;
    }

    // 3d. EXP-10: a `where_clause` member on (or reached only through) a
    // role-playing table has no way to name its role either — the predicate is
    // spliced as the member's own expression, never rewritten to a scoped
    // alias. Same reasoning as 3b/3c.
    if let Some(rw) = resolved_where.as_ref().filter(|_| anchored.is_none()) {
        check_where_clause_role_playing_path(view_name, def, &rw.members, &[], &[])?;
    }

    // 4. Resolve fact expressions via DAG inlining (fact-to-fact dependencies).
    let topo_order = toposort_facts(&def.facts).map_err(|e| ExpandError::CycleDetected {
        view_name: view_name.to_string(),
        cycle_description: e,
    })?;

    // 5. Build the SELECT list (no DISTINCT, no aggregation), in the FACTS
    // list's order: facts inlined, dimensions with the facts they name inlined.
    let items: Vec<SelectItem> = listed
        .iter()
        .map(|item| match item {
            FactsItem::Dimension(dim) => SelectItem::new(
                super::facts::inline_dimension_facts(&dim.expr, &def.facts),
                dim.output_type.clone(),
                quote_stored_ident(&dim.name),
            ),
            FactsItem::Fact(fact) => SelectItem::new(
                inline_facts(&fact.expr, &def.facts, &topo_order),
                fact.output_type.clone(),
                quote_stored_ident(&fact.name),
            ),
        })
        .collect();

    // 6. JOIN clauses — resolve required joins for dim + fact source tables.
    // Fact queries have no metrics; fact source tables are resolved through
    // the same path walk as dimensions (SG-10) and their joins are appended
    // after the dimension-driven joins.
    // Tables named only by the predicate still have to be joined, or the
    // filter would reference an alias that is not in the FROM.
    let mut fact_sources: Vec<String> = resolved_facts
        .iter()
        .filter_map(|f| f.source_table.clone())
        .collect();
    // PAR-6, the facts-path sibling: a queried fact's own expression may
    // reference a fact on a THIRD table, which `inline_facts` splices in here
    // exactly as it does inside a metric. `source_table` names only the fact's
    // own table, so the chain has to be walked for the rest.
    for fact in &resolved_facts {
        fact_sources.extend(super::facts::collect_referenced_fact_tables(
            &fact.expr, &def.facts,
        ));
    }
    if let Some(rw) = &resolved_where {
        fact_sources.extend(rw.source_tables.iter().cloned());
    }
    if let Some(anchor) = anchored {
        let mut extra = fact_sources;
        for dim in &resolved_dims {
            extra.extend(super::facts::collect_referenced_fact_tables(
                &dim.expr, &def.facts,
            ));
        }
        let joins = super::per_grain::anchor_joins(
            def,
            &anchor,
            &resolved_dims,
            &extra,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &[],
        );
        return Ok(SelectSpec {
            where_clause: resolved_where.map(|rw| rw.sql),
            distinct: false,
            items,
            from: FromSource::AnchorTable {
                def,
                anchor,
                joins,
                inner: false,
            },
            group_by: GroupBy::None,
        }
        .render());
    }
    let joins = resolve_joins_pkfk(def, &resolved_dims, &[], &fact_sources);

    // 7. A fact query is an unaggregated top-level SELECT over the base table
    //    (+ joins): no DISTINCT, no GROUP BY. The predicate is a plain WHERE —
    //    nothing is aggregated, so there is no "before" to be careful about.
    //    EXP-28: when every queried fact lives on one LEFT-JOINed table, that
    //    table is the query's grain and its phantom rows are filtered out.
    //    A list of dimensions only is at the grain of its dimensions, as the
    //    dimensions-only path is (EXP-29). With facts, the facts set the grain:
    //    a base dimension beside a child fact rides along on its rows.
    let fact_member_tables: Vec<Option<String>> = if resolved_facts.is_empty() {
        resolved_dims
            .iter()
            .map(|d| d.source_table.clone())
            .collect()
    } else {
        resolved_facts
            .iter()
            .map(|f| f.source_table.clone())
            .collect()
    };
    let phantom_guard = phantom_row_filter(def, &fact_member_tables);
    Ok(SelectSpec {
        where_clause: and_phantom_filter(resolved_where.map(|rw| rw.sql), phantom_guard),
        distinct: false,
        items,
        from: FromSource::BaseTable { def, joins },
        group_by: GroupBy::None,
    }
    .render())
}

/// The name of each column [`expand`] returns for `req`, in its order: the
/// requested dimensions, then metrics, then facts, each named after its stored
/// name with the quotes taken off (`"Order Date"` is the column `Order Date`).
///
/// # Errors
///
/// The resolution errors [`expand`] reports for the same request: an unknown,
/// duplicated or PRIVATE member.
pub fn output_column_names(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &QueryRequest,
) -> Result<Vec<String>, ExpandError> {
    let unquoted = |stored: &str| match crate::ident::parse_qualified_identifier(stored) {
        Ok(mut parts) if parts.len() == 1 => parts.pop().unwrap_or_default(),
        _ => stored.to_string(),
    };
    let mut names = Vec::new();
    for d in resolve_names::<Dimension, _>(&req.dimensions, view_name, def)? {
        names.push(unquoted(&d.name));
    }
    for m in resolve_names::<Metric, _>(&req.metrics, view_name, def)? {
        names.push(unquoted(&m.name));
    }
    for item in resolve_facts_list(&req.facts, view_name, def)? {
        names.push(unquoted(match item {
            FactsItem::Dimension(d) => &d.name,
            FactsItem::Fact(f) => &f.name,
        }));
    }
    Ok(names)
}

/// Expand a semantic view definition into a SQL query string.
///
/// Takes a view name (for error messages), its definition, and a query request
/// specifying which dimensions and metrics to include. Returns the generated SQL
/// or an `ExpandError` if the request is invalid.
///
/// # Errors
///
/// Returns `ExpandError` if:
/// - Neither dimensions nor metrics are requested (`EmptyRequest`)
/// - A requested dimension or metric name is not found (`UnknownDimension`, `UnknownMetric`)
/// - A dimension or metric name is duplicated (`DuplicateDimension`, `DuplicateMetric`)
pub fn expand(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &QueryRequest,
) -> Result<String, ExpandError> {
    // Phase 6.3: a metric written over a same-table metric carries that
    // metric's aggregate from here on, so every later step sees what it
    // computes (`graph::same_table_metrics`).
    let inlined = crate::graph::inline_same_table_metrics(def);
    let def = &*inlined;
    // The predicate's subqueries become CTEs ahead of the expansion, so that
    // they are uncorrelated (`where_clause::hoist_subqueries`). What is left in
    // the predicate is resolved as before: a `(SELECT * FROM __sv_sqN)` is
    // still a subquery to the member scan.
    let renamed;
    let req = match req.where_clause.as_deref() {
        Some(raw) => {
            super::where_clause::refuse_outside_one_predicate(view_name, raw)?;
            renamed = QueryRequest {
                where_clause: Some(super::where_clause::rename_params(raw)),
                ..req.clone()
            };
            &renamed
        }
        None => req,
    };
    let Some(hoisted) = req
        .where_clause
        .as_deref()
        .map(super::where_clause::hoist_subqueries)
        .filter(|h| !h.ctes.is_empty())
    else {
        return expand_unhoisted(view_name, def, req);
    };
    let req = QueryRequest {
        where_clause: Some(hoisted.predicate),
        ..req.clone()
    };
    let sql = expand_unhoisted(view_name, def, &req)?;
    Ok(super::where_clause::with_ctes(&sql, &hoisted.ctes))
}

/// [`expand`], for a predicate whose subqueries are already hoisted.
#[allow(clippy::too_many_lines)]
fn expand_unhoisted(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &QueryRequest,
) -> Result<String, ExpandError> {
    // 0. Facts and metrics are mutually exclusive.
    if !req.facts.is_empty() && !req.metrics.is_empty() {
        return Err(ExpandError::FactsMetricsMutualExclusion {
            view_name: view_name.to_string(),
        });
    }

    // 0b. So are facts and dimensions, as in Snowflake since its `2026_07`
    // behaviour change. A dimension is read row by row in FACTS and a fact is
    // grouped on in DIMENSIONS instead (`query::expr_items` lowers each to a
    // member of the clause's kind), so the clause says what the query is.
    if !req.facts.is_empty() && !req.dimensions.is_empty() {
        return Err(ExpandError::FactsWithDimensions {
            view_name: view_name.to_string(),
        });
    }

    // 0c. A stored ASOF marker must name one of its relationship's key
    // columns, or the join would lose its ASOF condition.
    super::join_resolver::check_asof_markers(view_name, def)?;

    // 1. Validate: at least one dimension, metric, or fact is required.
    if req.dimensions.is_empty() && req.metrics.is_empty() && req.facts.is_empty() {
        return Err(ExpandError::EmptyRequest {
            view_name: view_name.to_string(),
        });
    }

    // Dispatch to fact expansion path when facts are requested.
    if !req.facts.is_empty() {
        return expand_facts(view_name, def, req);
    }

    // 2. Resolve requested dimensions to their definitions.
    let resolved_dims = resolve_names::<Dimension, _>(&req.dimensions, view_name, def)?;

    // 3. Resolve requested metrics to their definitions.
    // Phase 43: PRIVATE access check -- private metrics cannot be queried directly.
    // Derived metrics that reference private bases still work because
    // inline_derived_metrics resolves expressions, not access modifiers.
    let resolved_mets = resolve_names::<Metric, _>(&req.metrics, view_name, def)?;

    // Phase 48 / Phase 7 PR 5: a window metric runs over the rows the rest of
    // the query gives, so the query with each window replaced by its inner
    // metric is expanded on its own first, through every check below.
    if resolved_mets.iter().any(|m| m.is_window()) {
        return super::window::expand_over_result(
            view_name,
            def,
            req,
            &resolved_dims,
            &resolved_mets,
            |inner_def, inner_req| expand_unhoisted(view_name, inner_def, inner_req),
        );
    }

    // Resolve the pre-aggregation predicate up front: it decides materialization
    // routing below, participates in the fan-trap checks, and is rejected by the
    // strategies that cannot yet inject it.
    let resolved_where = req
        .where_clause
        .as_deref()
        // A blank predicate is absent, not an empty condition: `Some("")` would
        // otherwise emit a bare `WHERE ` with nothing after it. The FFI maps an
        // empty parameter to None, but `expand()` is public and reachable
        // directly (found by `fuzz_where_predicate`).
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| super::where_clause::resolve_where_clause(view_name, def, raw))
        .transpose()?;

    // A member's table reached from a metric's table by two paths through
    // different tables (a diamond) is refused unless the metric's `USING`
    // picks one, as Snowflake refuses it (010246). Asked before anything is
    // routed or planned: every emitter below would follow one path silently.
    let path_members = if resolved_mets.is_empty() || def.joins.is_empty() {
        None
    } else {
        let root = crate::graph::RelationshipGraph::from_definition(def)
            .map(|g| g.root)
            .unwrap_or_default();
        let where_members = resolved_where
            .as_ref()
            .map_or(&[][..], |w| w.members.as_slice());
        let members = super::multi_path::query_members(def, &resolved_dims, where_members);
        super::multi_path::check_metric_paths(view_name, def, &root, &resolved_mets, &members)?;
        Some((root, members))
    };

    // Phase 55: Materialization routing.
    // Attempt to route to a pre-aggregated table if an exact match exists.
    // Returns None if no match, or if any metric is semi-additive / window.
    //
    // A pre-aggregated table cannot answer a predicate: its rows are already
    // aggregated, so filtering them is a post-aggregation filter over whatever
    // members it happens to carry — not the pre-aggregation filter that was
    // asked for. Skip routing entirely whenever a predicate is present and
    // compute from the base tables (correct, if slower). Routing a filtered
    // query when every referenced member is a materialized dimension is a
    // separate, later change.
    //
    // A query with no metrics is routed only once `member_anchor` has found
    // one table to read it from: a materialization must not answer what the
    // base tables refuse (two paths, unrelated tables).
    let route = || {
        if resolved_where.is_none() {
            super::materialization::try_route_materialization(def, &resolved_dims, &resolved_mets)
        } else {
            None
        }
    };
    if !resolved_mets.is_empty() {
        if let Some(routed_sql) = route() {
            return Ok(routed_sql);
        }
    }

    // A dimensions-only query reads from the table under all its members, not
    // from whichever table is declared first (see `member_anchor`). Every hop
    // from there is many-to-one with one relationship, so no fan-trap,
    // role-playing or phantom-row guard below has anything to catch on it.
    // Phase 7 PR 4d: or from the table their paths meet at, whose hops fan out
    // and are inner-joined. A query with no metrics aggregates nothing, so the
    // fan-outs are taken away by the DISTINCT and the fan-trap guard has
    // nothing to protect; each hop is still one relationship.
    if resolved_mets.is_empty() {
        let where_tables: Vec<String> = resolved_where
            .as_ref()
            .map(|w| w.source_tables.clone())
            .unwrap_or_default();
        let unqualified = resolved_where
            .as_ref()
            .is_some_and(|w| w.members.iter().any(|m| m.table.is_none()));
        let anchor = dims_only_members(def, &resolved_dims, &where_tables, unqualified)
            .map_or(Anchor::Undecided, |members| member_anchor(def, &members));
        let inner = matches!(anchor, Anchor::Meet(_));
        match anchor {
            Anchor::At(anchor) | Anchor::Meet(anchor) => {
                // A materialization's rows were grouped by whatever built it,
                // which nothing ties to the tuples a meeting table reaches
                // (a grouped metric keeps an order with no notes under a NULL
                // kind), so only a query read from below is routed.
                if !inner {
                    if let Some(routed_sql) = route() {
                        return Ok(routed_sql);
                    }
                }
                let mut extra = where_tables;
                for dim in &resolved_dims {
                    extra.extend(super::facts::collect_referenced_fact_tables(
                        &dim.expr, &def.facts,
                    ));
                }
                let joins = super::per_grain::anchor_joins(
                    def,
                    &anchor,
                    &resolved_dims,
                    &extra,
                    &std::collections::HashMap::new(),
                    &std::collections::HashMap::new(),
                    &[],
                );
                let items = resolved_dims
                    .iter()
                    .map(|dim| {
                        SelectItem::new(
                            super::facts::inline_dimension_facts(&dim.expr, &def.facts),
                            dim.output_type.clone(),
                            quote_stored_ident(&dim.name),
                        )
                    })
                    .collect();
                return Ok(SelectSpec {
                    where_clause: resolved_where.map(|rw| rw.sql),
                    distinct: true,
                    items,
                    from: FromSource::AnchorTable {
                        def,
                        anchor,
                        joins,
                        inner,
                    },
                    group_by: GroupBy::None,
                }
                .render());
            }
            Anchor::Unrelated(table_a, table_b) => {
                return Err(ExpandError::UnrelatedTables {
                    view_name: view_name.to_string(),
                    table_a,
                    table_b,
                });
            }
            Anchor::MultiPath(anchor, member_table, paths) => {
                return Err(multi_path_error(
                    view_name,
                    &resolved_dims,
                    anchor,
                    member_table,
                    paths,
                    false,
                ));
            }
            Anchor::Paired(first, second, routes) => {
                return Err(multi_path_error(
                    view_name,
                    &resolved_dims,
                    first,
                    second,
                    routes,
                    true,
                ));
            }
            Anchor::Undecided => {
                // A member declared on no table leaves the rows unplaced, but
                // two placed members joined by two paths are still refused.
                let mut placed: Vec<String> = where_tables;
                for dim in &resolved_dims {
                    placed.extend(dim.source_table.clone());
                    placed.extend(super::facts::collect_referenced_fact_tables(
                        &dim.expr, &def.facts,
                    ));
                }
                if let Some((from, member_table, paths)) =
                    super::multi_path::between_members(def, &placed)
                {
                    return Err(multi_path_error(
                        view_name,
                        &resolved_dims,
                        from,
                        member_table,
                        paths,
                        false,
                    ));
                }
                if let Some(routed_sql) = route() {
                    return Ok(routed_sql);
                }
            }
        }
    }

    // 4. Pre-compute all metric expressions: inline facts into base metrics,
    //    then inline metric references into derived metrics.
    let topo_order = toposort_facts(&def.facts).map_err(|e| ExpandError::CycleDetected {
        view_name: view_name.to_string(),
        cycle_description: e,
    })?;
    let resolved = inline_derived_metrics_with_joins(
        &def.metrics,
        &def.facts,
        &topo_order,
        &def.tables,
        &def.joins,
    )
    .map_err(|e| ExpandError::CycleDetected {
        view_name: view_name.to_string(),
        cycle_description: e,
    })?;

    // TECH-DEBT #35 / v0.12.0: decide whether this query must be computed at
    // each metric's OWN grain. `plan` returns None for everything the
    // base-anchored path already answers correctly (so single-grain SQL is
    // unchanged) and for multi-grain shapes it cannot express (so those keep
    // their fan-trap error). Decided before the checks below because both of
    // them exist only to guard the base-anchored topology.
    // A `where_clause` member's tables are joined into whichever CTE evaluates
    // the predicate, so both decisions below have to account for them alongside
    // the dimension and metric tables.
    let where_tables: Vec<String> = resolved_where
        .as_ref()
        .map(|w| w.source_tables.clone())
        .unwrap_or_default();
    // EXP-22: a member the predicate names that declares NO source table is an
    // unqualified expression, exactly like the dimensions all three deciders
    // already refuse to re-anchor — it contributes no entry to `where_tables`,
    // so without this the deciders could not see it at all.
    let where_has_unqualified_member = resolved_where
        .as_ref()
        .is_some_and(|w| w.members.iter().any(|m| m.table.is_none()));
    let grain_plan = super::per_grain::plan(
        def,
        &resolved_dims,
        &resolved_mets,
        &resolved.exprs,
        &where_tables,
        where_has_unqualified_member,
    );

    // A path a metric's `USING` picks through a diamond is followed only by
    // the per-grain plan's joins. Any other emitter would join along the
    // first-declared path, so a query the planner declined is refused.
    if grain_plan.is_none() {
        if let Some((root, members)) = &path_members {
            if let Some((metric_name, member_table, path)) =
                super::multi_path::first_pinned(def, root, &resolved_mets, members)
            {
                // A member below a metric's grain is refused on every path,
                // and that is the error to give. In per-grain mode the fence
                // asks only that, which no choice of path changes: a pinned
                // path is many-to-one at every step.
                check_fan_traps(view_name, def, &resolved_dims, &resolved_mets, true, false)?;
                if let Some(rw) = &resolved_where {
                    super::fan_trap::check_where_clause_fan_traps(
                        view_name,
                        def,
                        &rw.members,
                        &resolved_mets,
                        false,
                    )?;
                }
                return Err(ExpandError::UnfollowedUsingPath {
                    view_name: view_name.to_string(),
                    metric_name,
                    member_table,
                    path,
                });
            }
        }
    }

    // An ACTIVE semi-additive metric at a non-root grain is answered by
    // anchoring `__sv_snapshot` there instead of at the base table (TECH-DEBT
    // #36). Decided before the checks below for the same reason as
    // `grain_plan`: they are the ones this supersedes.
    //
    // `queried_dim_keys` are canonical (quote-stripped + folded) so a dotted or
    // quoted NA reference resolves against the queried dims (#30); the same set
    // is reused by the semi-additive dispatch further down.
    let queried_dim_keys: std::collections::HashSet<String> = resolved_dims
        .iter()
        .map(|d| crate::ident::normalize_ident_part(&d.name))
        .collect();
    let has_active_semi_additive = resolved_mets
        .iter()
        .any(|m| super::semi_additive::is_active_semi_additive(def, m, &queried_dim_keys));
    // EXP-19/EXP-20 (code-review 2026-08-06): `is_active_semi_additive` asks
    // only about a metric's OWN `non_additive_by`, so a metric that merely
    // DEPENDS on an active semi-additive one — a derived metric referencing
    // it — routed to the regular path and inlined the dependency's raw aggregate, evaluating it
    // over every row and silently dropping `NON ADDITIVE BY`. The result did
    // not even agree with the same snapshot queried directly (`double_balance`
    // != 2 x `balance`).
    //
    // Checked here, before any dispatch, because every emission path shares the
    // defect: the base-anchored path inlines the raw aggregate, and per-grain's
    // `is_snapshot_group` classifies by
    // the group's own metrics so the dependent metric lands in a PLAIN group.
    //
    // Composing a snapshot with an outer expression is a feature, not a patch
    // (TECH-DEBT #55); until it lands this errors, the same call SG-5 makes for
    // co-queried shapes its CTE cannot decompose. Iteration is over `def.metrics`
    // rather than the dependency `HashSet` so the metric named in the error is
    // deterministic (declaration order) when a metric reaches two of them.
    for met in &resolved_mets {
        if super::semi_additive::is_active_semi_additive(def, met, &queried_dim_keys) {
            continue; // Takes the snapshot path itself; SG-5 governs the rest.
        }
        let met_key = crate::ident::normalize_ident_part(&met.name);
        let deps = collect_transitive_metric_names(met, &def.metrics);
        for dep in &def.metrics {
            let dep_key = crate::ident::normalize_ident_part(&dep.name);
            if dep_key == met_key || !deps.contains(&dep_key) {
                continue;
            }
            if super::semi_additive::is_active_semi_additive(def, dep, &queried_dim_keys) {
                return Err(ExpandError::SemiAdditiveThroughDependency {
                    view_name: view_name.to_string(),
                    metric_name: met.name.clone(),
                    semi_metric_name: dep.name.clone(),
                    non_additive_by: dep
                        .non_additive_by
                        .iter()
                        .map(|na| na.dimension.clone())
                        .collect(),
                });
            }
        }
    }

    let snapshot_anchor = if has_active_semi_additive {
        let mut extra = super::semi_additive::na_dim_source_tables(
            view_name,
            def,
            &resolved_mets,
            &queried_dim_keys,
        );
        extra.extend(where_tables.iter().cloned());
        super::per_grain::snapshot_cte_anchor(
            def,
            &resolved_dims,
            &resolved_mets,
            &extra,
            where_has_unqualified_member,
        )
    } else {
        None
    };

    // SG-8: fail loudly when a REQUESTED metric (directly, or via a derived
    // metric) depends on a COUNT(*)
    // that could not be rewritten to COUNT(<pk>) — a non-base source table
    // with no PRIMARY KEY declared. Emitting it as-is would count
    // NULL-extended LEFT JOIN rows (one per childless base row).
    //
    // On the per-grain path there is no such row: the metric's table anchors
    // its own CTE instead of being LEFT JOINed to the base table, so a bare
    // COUNT(*) counts exactly that table's rows and needs no PRIMARY KEY.
    if grain_plan.is_none() && snapshot_anchor.is_none() && !resolved.count_star_no_pk.is_empty() {
        for met in &resolved_mets {
            for name in collect_transitive_metric_names(met, &def.metrics) {
                if let Some(table_alias) = resolved.count_star_no_pk.get(&name) {
                    let metric_name = def
                        .metrics
                        .iter()
                        .find(|m| crate::ident::ident_matches(&m.name, &name))
                        .map_or(name.clone(), |m| m.name.clone());
                    return Err(ExpandError::CountStarRequiresPrimaryKey {
                        view_name: view_name.to_string(),
                        metric_name,
                        table_alias: table_alias.clone(),
                    });
                }
            }
        }
    }
    let resolved_exprs = resolved.exprs;

    // Phase 6.3: off the per-grain path, a dimension on a role-played table
    // that some metric names no role for is refused below by
    // `find_using_context`. Asked first, so that refusal names the real reason
    // rather than a fan trap between metrics the per-grain path would have
    // separated had every metric named its role.
    if grain_plan.is_none() {
        for &dim in &resolved_dims {
            find_using_context(view_name, def, dim, &resolved_mets)?;
        }
    }

    // TECH-DEBT #111: a predicate on a role-played table, or on one reached
    // through or back across it, is read on the role every metric names
    // (Snowflake fixture 6, R5). The per-grain plan reads it so in each grain
    // CTE (`role_playing::where_role_dims`), and the base-anchored emission
    // below rewrites the predicate and joins those instances, and so does the
    // base-anchored snapshot (`SnapshotFrom::Base`). A snapshot re-anchored at
    // a metric's own table takes no scopes (`snapshot_cte_anchor` declines a
    // query whose predicate is read on a role, so this is defensive), and for
    // it the predicate stays refused below.
    let where_scopes: Vec<(String, String)> = match &resolved_where {
        Some(rw) if snapshot_anchor.is_none() => {
            super::role_playing::where_role_scopes(def, &rw.members, &resolved_mets)
        }
        _ => Vec::new(),
    };
    // EXP-10: and the role-playing seam the fan-trap check does not cover —
    // reaching the member's table is unambiguous only if no role-playing
    // table sits on the path. Runs on every emission path because it is
    // decided here, above the per-grain / anchored / base-anchored split, and
    // before the fan-trap checks, like the dimensions' role check above: off
    // the per-grain path a fan trap depends on which table is declared first.
    if let Some(rw) = &resolved_where {
        check_where_clause_role_playing_path(
            view_name,
            def,
            &rw.members,
            &resolved_mets,
            &where_scopes,
        )?;
    }

    // Phase 31: Check for fan traps before generating SQL. In per-grain mode the
    // fence keeps only the metric × dimension check — the other two guard the
    // base-anchored topology the per-grain plan replaces. An anchored snapshot
    // CTE replaces that topology in exactly the same way, so it uses the same
    // mode; the retained metric × dimension check is what still reports a
    // dimension below the metric's own grain.
    // Phase 7.3, PR 4c: only a per-grain plan that deduplicates its bridged
    // groups may let a dimension or filter that fans a metric's rows out
    // through (`Plan::crosses_bridge`).
    let bridges = grain_plan
        .as_ref()
        .is_some_and(super::per_grain::Plan::crosses_bridge);
    check_fan_traps(
        view_name,
        def,
        &resolved_dims,
        &resolved_mets,
        grain_plan.is_some() || snapshot_anchor.is_some(),
        bridges,
    )?;
    // PAR-6 (TECH-DEBT #53): a member reaching a fact on another table now
    // pulls that table's join, so the fence has to rule out a fanning one.
    // Runs on every emission path — the referenced fact is inlined inside its
    // member's expression, so per-grain aggregation does not separate them.
    super::fan_trap::check_referenced_fact_fan_traps(
        view_name,
        def,
        &resolved_dims,
        &resolved_mets,
    )?;
    // TECH-DEBT #69: and the role-playing seam beside it — a referenced fact on
    // a role-playing path binds to a role only through a base metric's single
    // `USING`; anything else would join the first-declared relationship. The
    // `where_clause` tables decide the base-table exemption.
    super::role_playing::check_member_fact_role_playing_path(
        view_name,
        def,
        &resolved_dims,
        &resolved_mets,
        &where_tables,
    )?;
    if let Some(rw) = &resolved_where {
        super::fan_trap::check_where_clause_fan_traps(
            view_name,
            def,
            &rw.members,
            &resolved_mets,
            bridges,
        )?;
    }

    if let Some(plan) = grain_plan {
        // The predicate goes inside EACH grain CTE, so every metric aggregates
        // over only the matching rows. On the outer query it would instead
        // filter the already-combined result — a post-aggregation filter
        // wearing a pre-aggregation name.
        return super::per_grain::expand_per_grain(
            view_name,
            def,
            &resolved_dims,
            &resolved_mets,
            &resolved_exprs,
            &plan,
            resolved_where.as_ref(),
        );
    }

    // Phase 32: pair each resolved dimension with its role-playing scoped alias
    // (e.g. "a__dep_airport"). R-8 (code-review 2026-07-11): zipped into
    // `ResolvedDim` so the alias travels with its dimension instead of a
    // position-indexed side array (`dim_scoped_aliases[i]`).
    let mut resolved: Vec<ResolvedDim> = Vec::with_capacity(resolved_dims.len());
    for &dim in &resolved_dims {
        let scoped_alias = find_using_context(view_name, def, dim, &resolved_mets)?;
        resolved.push(ResolvedDim { dim, scoped_alias });
    }

    // Phase 47: a semi-additive metric only needs CTE treatment when at least
    // one of its NA dims is NOT in the queried dimension set. When ALL are in
    // the query it acts as regular (Snowflake semantics). Decided above, with
    // the snapshot anchor that depends on it.
    if has_active_semi_additive {
        let no_paths = std::collections::HashMap::new();
        let from = match snapshot_anchor.as_deref() {
            Some(anchor) => super::semi_additive::SnapshotFrom::Grain {
                anchor,
                paths: &no_paths,
            },
            None => super::semi_additive::SnapshotFrom::Base {
                where_scopes: &where_scopes,
            },
        };
        return super::semi_additive::expand_semi_additive(
            view_name,
            def,
            &resolved,
            &resolved_mets,
            &resolved_exprs,
            resolved_where.as_ref(),
            &from,
        );
    }

    // 5. Build the top-level SELECT.
    //    Dimensions-only (no metrics): SELECT DISTINCT, no GROUP BY.
    //    Metrics-only (no dimensions): SELECT (global aggregate), no GROUP BY.
    //    Both: SELECT with an ordinal GROUP BY over the dimensions.
    let distinct = !resolved_dims.is_empty() && resolved_mets.is_empty();

    let mut items: Vec<SelectItem> = Vec::new();
    for rd in &resolved {
        let dim = rd.dim;
        let mut base_expr = super::facts::inline_dimension_facts(&dim.expr, &def.facts);
        // Phase 32: If this dimension has a scoped alias, rewrite the expression.
        if let Some(ref scoped) = rd.scoped_alias {
            if let Some(ref st) = dim.source_table {
                // Rewrite the source-table qualifier to the scoped alias
                // e.g., "a.city" -> "a__dep_airport.city"
                base_expr = super::join_resolver::rewrite_to_scoped_alias(&base_expr, st, scoped);
            }
        }
        items.push(SelectItem::new(
            base_expr,
            dim.output_type.clone(),
            quote_stored_ident(&dim.name),
        ));
    }
    for met in &resolved_mets {
        // Look up the pre-computed resolved expression (handles both base +
        // derived metrics) by the metric's canonical key, matching how
        // `inline_derived_metrics` keys the map (EXP-6).
        let resolved_expr = resolved_exprs
            .get(&crate::ident::normalize_ident_part(&met.name))
            .cloned()
            .unwrap_or_else(|| met.expr.clone());
        items.push(SelectItem::new(
            resolved_expr,
            met.output_type.clone(),
            quote_stored_ident(&met.name),
        ));
    }

    // 6. Join resolution via PK/FK graph.
    //    The resolver returns structured edges in emission order; role-playing
    //    scoped joins (e.g. "a__dep_airport") follow the bare joins.
    // Tables named only by the predicate must still be joined, or the filter
    // would reference an alias absent from the FROM.
    // A predicate table read on a role (`where_scopes`) is joined as a
    // dimension on it would be, under the role's scoped alias, and the
    // predicate is rewritten to name that alias.
    let where_tables: Vec<String> = resolved_where
        .as_ref()
        .map(|rw| rw.source_tables.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !where_scopes.iter().any(|(s, _)| s == t))
        .collect();
    let scoped_where_dims: Vec<Dimension> = where_scopes
        .iter()
        .map(|(table, _)| Dimension {
            source_table: Some(table.clone()),
            ..Default::default()
        })
        .collect();
    let join_dims: Vec<&Dimension> = resolved_dims
        .iter()
        .copied()
        .chain(scoped_where_dims.iter())
        .collect();
    let joins = resolve_joins_pkfk(def, &join_dims, &resolved_mets, &where_tables);
    let resolved_where = resolved_where.map(|mut rw| {
        for (table, scoped) in &where_scopes {
            rw.sql = super::join_resolver::rewrite_to_scoped_alias(&rw.sql, table, scoped);
        }
        rw
    });

    // 7. GROUP BY (only when both dimensions and metrics are present).
    //    Ordinal positions avoid ambiguity when an expression matches its alias
    //    (e.g. `status AS "status"`) — see push_group_by_ordinals (E-1).
    let group_by = if !resolved_dims.is_empty() && !resolved_mets.is_empty() {
        GroupBy::Ordinals(resolved_dims.len())
    } else {
        GroupBy::None
    };

    // EXP-29: a dimensions-only `SELECT DISTINCT` emits rows directly, so it has
    // the same phantom-row exposure as a fact query — a childless parent
    // contributes a manufactured NULL that reads as a genuine data NULL. Only
    // that branch: with metrics present the phantom is fenced inside each
    // aggregate instead. Role-played dimensions are skipped — their table is
    // joined under a scoped alias the guard does not name.
    let phantom_guard = if distinct && resolved.iter().all(|rd| rd.scoped_alias.is_none()) {
        let dim_member_tables: Vec<Option<String>> = resolved_dims
            .iter()
            .map(|d| d.source_table.clone())
            .collect();
        phantom_row_filter(def, &dim_member_tables)
    } else {
        None
    };

    Ok(SelectSpec {
        // Rendered between the joins and the GROUP BY, so rows are filtered on
        // their way INTO the aggregation — Snowflake's "applied before the
        // metrics are computed".
        where_clause: and_phantom_filter(resolved_where.map(|rw| rw.sql), phantom_guard),
        distinct,
        items,
        from: FromSource::BaseTable { def, joins },
        group_by,
    }
    .render())
}
