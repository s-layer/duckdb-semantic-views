//! View variables (Snowflake's `VARIABLES` clause, Phase 6.5): what a
//! variable may be, how it is written back, and how a query's values reach
//! the member expressions that use it.
//!
//! A fact, dimension or metric expression, and a query's `where_clause`, refer
//! to a variable by its unqualified name; a qualified name is a column, as in
//! Snowflake. Before a request is expanded, [`bind`] writes each such
//! reference as the value the query set or, failing that, the variable's
//! default, so the expansion itself never sees a variable:
//!
//! - a value the query set is the `params` call `__sv_param(<n>)`, which the
//!   bind replaces with the value itself (TECH-DEBT #105), so a value is
//!   never SQL text;
//! - a default is its expression, evaluated in the query;
//! - either is cast to the variable's type when it has one.
//!
//! A query that leaves a variable with no default unset fails, as in
//! Snowflake, whether or not it uses a member that refers to it.

use std::collections::{HashMap, HashSet};

use crate::ident::normalize_ident_part;
use crate::model::{SemanticViewDefinition, Variable};

/// What a variable may be, for the DDL and YAML forms alike: one name, used
/// by no member and no other variable; a default that is one expression with
/// no subquery or parameter, as in Snowflake; and no use in a window frame
/// bound, which Snowflake refuses.
///
/// # Errors
///
/// The first problem found, as a message.
pub fn validate(def: &SemanticViewDefinition) -> Result<(), String> {
    validate_parts(&def.variables, &def.dimensions, &def.facts, &def.metrics)
}

/// [`validate`] over the parts of a definition, for the DDL parser, which has
/// them before it has a definition.
///
/// # Errors
///
/// The first problem found, as a message.
pub fn validate_parts(
    variables: &[Variable],
    dimensions: &[crate::model::Dimension],
    facts: &[crate::model::Fact],
    metrics: &[crate::model::Metric],
) -> Result<(), String> {
    let mut seen: HashSet<String> = HashSet::new();
    let members: HashMap<String, &str> = dimensions
        .iter()
        .map(|d| (normalize_ident_part(&d.name), "dimension"))
        .chain(
            facts
                .iter()
                .map(|f| (normalize_ident_part(&f.name), "fact")),
        )
        .chain(
            metrics
                .iter()
                .map(|m| (normalize_ident_part(&m.name), "metric")),
        )
        .collect();
    for v in variables {
        if v.name.trim().is_empty() {
            return Err("a variable has an empty name".to_string());
        }
        match crate::ident::parse_qualified_identifier(v.name.trim()) {
            Ok(parts) if parts.len() == 1 && v.name.trim() == v.name => {}
            _ => {
                return Err(format!(
                    "variable name '{}' is not a single identifier",
                    v.name
                ))
            }
        }
        let key = normalize_ident_part(&v.name);
        if !seen.insert(key.clone()) {
            return Err(format!("variable '{}' is declared twice", v.name));
        }
        if let Some(kind) = members.get(&key) {
            return Err(format!(
                "variable '{}' has the name of a {kind}; a variable can't share a name with \
                 a fact, dimension or metric",
                v.name
            ));
        }
        if v.data_type.as_deref().is_some_and(|t| t.trim().is_empty()) {
            return Err(format!("variable '{}' has an empty type", v.name));
        }
        if let Some(default) = &v.default {
            if let Some(reason) = default_error(default) {
                return Err(format!(
                    "variable '{}' has an invalid default: {reason}",
                    v.name
                ));
            }
        }
    }
    if variables.is_empty() {
        return Ok(());
    }
    let names: HashSet<String> = variables
        .iter()
        .map(|v| normalize_ident_part(&v.name))
        .collect();
    for m in metrics {
        let Some(frame) = m
            .window_spec
            .as_ref()
            .and_then(|w| w.frame_clause.as_deref())
        else {
            continue;
        };
        if let Some(r) = crate::expr_tokens::scan_references(frame)
            .into_iter()
            .find(|r| r.is_bare() && names.contains(&r.key()))
        {
            return Err(format!(
                "metric '{}' uses variable '{}' in its window frame; a variable can't be \
                 used in a window frame bound",
                m.name, r.raw
            ));
        }
    }
    Ok(())
}

/// Why `default` cannot be a variable's default, if it cannot.
fn default_error(default: &str) -> Option<String> {
    if default.trim().is_empty() {
        return Some("it is empty".to_string());
    }
    if crate::expr_tokens::has_statement_separator(default) {
        return Some("it holds a ';'".to_string());
    }
    if let Some((_, p)) = crate::expr_tokens::find_parameters(default).first() {
        return Some(format!(
            "it holds the prepared statement parameter {p}; a default can't use one"
        ));
    }
    if !crate::expr_tokens::subquery_spans(default).is_empty() {
        return Some("it holds a subquery; a default can't use one".to_string());
    }
    None
}

/// One `VARIABLES` entry as `GET_DDL` writes it:
/// `name [type] [DEFAULT expr] [COMMENT '...']`.
#[must_use]
pub fn render_entry(v: &Variable) -> String {
    let mut out = v.name.clone();
    if let Some(t) = &v.data_type {
        out.push(' ');
        out.push_str(t);
    }
    if let Some(d) = &v.default {
        out.push_str(" DEFAULT ");
        out.push_str(d);
    }
    if let Some(c) = &v.comment {
        out.push_str(" COMMENT '");
        out.push_str(&crate::sql_lit::SqlLit::escape(c).to_string());
        out.push('\'');
    }
    out
}

/// The values a variable takes in one expansion.
#[derive(Debug, Clone, Copy)]
pub enum Values<'a> {
    /// A query: the variables it set, by name, whose values are `params`
    /// from `first_slot` on, in this order.
    Query {
        names: &'a [String],
        first_slot: usize,
    },
    /// A probe of the members' types (CREATE, SHOW, DESCRIBE): each variable
    /// is a NULL of its type, or its default when it has no type, so no
    /// value is needed.
    Probe,
}

/// A definition with its variables written into its member expressions.
#[derive(Debug, Clone)]
pub struct Bound {
    pub def: SemanticViewDefinition,
    /// What each variable is written as, for the query's own expressions.
    pub substitution: Substitution,
}

impl Bound {
    /// `expr` (a query's `where_clause`) with its variables written in.
    #[must_use]
    pub fn apply(&self, expr: &str) -> String {
        self.substitution.apply(expr)
    }
}

/// What each variable of one expansion is written as, by normalized name.
#[derive(Debug, Clone, Default)]
pub struct Substitution(HashMap<String, String>);

impl Substitution {
    /// `expr` (a query's `where_clause` or expression item) with its
    /// variables written in: each bare name of a variable, not a qualified
    /// one, which is the column.
    #[must_use]
    pub fn apply(&self, expr: &str) -> String {
        apply(expr, &self.0)
    }
}

fn apply(expr: &str, text: &HashMap<String, String>) -> String {
    if text.is_empty() {
        return expr.to_string();
    }
    let map: HashMap<String, &str> = text.iter().map(|(k, v)| (k.clone(), v.as_str())).collect();
    let refs: Vec<_> = crate::expr_tokens::scan_references(expr)
        .into_iter()
        .filter(crate::expr_tokens::IdentRef::is_bare)
        .collect();
    crate::expr_tokens::splice_references(expr, refs, &map)
}

/// `def` with each variable written into the member expressions that use it,
/// for one expansion (see the module docs).
///
/// A materialization covering a member that uses a variable, directly or
/// through another member, is dropped: its table was computed with values
/// the query may not have.
///
/// # Errors
///
/// For a query: a name `def` declares no variable for, a name set twice, or a
/// variable with no default that the query does not set.
pub fn bind(
    view_name: &str,
    mut def: SemanticViewDefinition,
    values: Values<'_>,
) -> Result<Bound, String> {
    let mut text: HashMap<String, String> = HashMap::new();
    if let Values::Query { names, first_slot } = values {
        for (i, name) in names.iter().enumerate() {
            let key = normalize_ident_part(name);
            let Some(v) = def
                .variables
                .iter()
                .find(|v| normalize_ident_part(&v.name) == key)
            else {
                let declared: Vec<String> = def.variables.iter().map(|v| v.name.clone()).collect();
                let hint = crate::util::suggest_closest(name, &declared)
                    .map(|s| format!(" Did you mean '{s}'?"))
                    .unwrap_or_default();
                return Err(format!(
                    "semantic view '{view_name}' has no variable '{name}'.{hint}"
                ));
            };
            if text.contains_key(&key) {
                return Err(format!(
                    "semantic view '{view_name}': variable '{name}' is set twice"
                ));
            }
            let call = format!("{}({})", crate::expand::PARAM_FUNCTION, first_slot + i);
            text.insert(key, typed(&call, v.data_type.as_deref()));
        }
    }
    for v in &def.variables {
        let key = normalize_ident_part(&v.name);
        if text.contains_key(&key) {
            continue;
        }
        let value = match (values, &v.default, &v.data_type) {
            (Values::Probe, _, Some(t)) => format!("(CAST(NULL AS {t}))"),
            (_, Some(d), t) => typed(d, t.as_deref()),
            (Values::Probe, None, None) => "(NULL)".to_string(),
            (Values::Query { .. }, None, _) => {
                return Err(format!(
                    "semantic view '{view_name}': variable '{}' has no default, so the query \
                     must set it: VARIABLES {} = <value>",
                    v.name, v.name
                ));
            }
        };
        text.insert(key, value);
    }
    if text.is_empty() {
        return Ok(Bound {
            def,
            substitution: Substitution(text),
        });
    }

    drop_materializations_over(&mut def, &text);

    for d in &mut def.dimensions {
        d.expr = apply(&d.expr, &text);
    }
    for f in &mut def.facts {
        f.expr = apply(&f.expr, &text);
    }
    for m in &mut def.metrics {
        m.expr = apply(&m.expr, &text);
        if let Some(ws) = &mut m.window_spec {
            for a in &mut ws.extra_args {
                *a = apply(a, &text);
            }
            for ob in &mut ws.order_by {
                ob.expr = apply(&ob.expr, &text);
            }
        }
    }
    Ok(Bound {
        def,
        substitution: Substitution(text),
    })
}

/// Drop the materializations that cover a member using a variable, directly
/// or through another member: their tables were computed with values the
/// query may not have.
fn drop_materializations_over(def: &mut SemanticViewDefinition, text: &HashMap<String, String>) {
    let mut used: HashSet<String> = HashSet::new();
    let reads = |expr: &str, used: &HashSet<String>| {
        crate::expr_tokens::scan_references(expr)
            .into_iter()
            .any(|r| {
                (r.is_bare() && text.contains_key(&r.key())) || used.contains(&r.last_part_key())
            })
    };
    // To a fixpoint, since a metric can use a fact or a metric that uses one.
    // A window metric's expression is its whole call, arguments and ORDER BY
    // included.
    loop {
        let before = used.len();
        for (name, expr) in def
            .dimensions
            .iter()
            .map(|d| (&d.name, &d.expr))
            .chain(def.facts.iter().map(|f| (&f.name, &f.expr)))
            .chain(def.metrics.iter().map(|m| (&m.name, &m.expr)))
        {
            if reads(expr, &used) {
                used.insert(normalize_ident_part(name));
            }
        }
        if used.len() == before {
            break;
        }
    }
    def.materializations.retain(|mat| {
        !mat.dimensions
            .iter()
            .chain(&mat.metrics)
            .any(|n| used.contains(&normalize_ident_part(n)))
    });
}

/// The SQL a CREATE binds to check a variable, as Snowflake checks one at
/// CREATE: its default cast to its type, else a NULL of its type, else its
/// default; `None` for a variable with neither.
#[must_use]
pub fn create_probe(v: &Variable) -> Option<String> {
    let value = match (&v.default, &v.data_type) {
        (Some(d), t) => typed(d, t.as_deref()),
        (None, Some(t)) => format!("(CAST(NULL AS {t}))"),
        (None, None) => return None,
    };
    Some(format!("SELECT {value} AS v"))
}

/// Whether `data_type`, a type as `DuckDB` names it, is one a variable can't
/// have. Snowflake refuses `VARIANT`, `OBJECT` and `ARRAY`; their `DuckDB`
/// counterparts are the nested types (a list or array, `STRUCT`, `MAP`,
/// `UNION`) and the semi-structured `VARIANT` and `JSON`.
#[must_use]
pub fn unsupported_type(data_type: &str) -> bool {
    let t = data_type.trim().to_ascii_uppercase();
    t.ends_with(']')
        || ["STRUCT", "MAP", "UNION"]
            .iter()
            .any(|n| t.starts_with(&format!("{n}(")))
        || t == "VARIANT"
        || t == "JSON"
}

/// `def` with each variable written as it is for a probe of the members'
/// types ([`Values::Probe`]), which needs no value.
#[must_use]
pub fn for_probe(def: &SemanticViewDefinition) -> SemanticViewDefinition {
    match bind("", def.clone(), Values::Probe) {
        Ok(bound) => bound.def,
        // A probe sets no variable, so nothing can be missing or unknown.
        Err(_) => def.clone(),
    }
}

/// `value` in parentheses, cast to `data_type` when there is one.
fn typed(value: &str, data_type: Option<&str>) -> String {
    match data_type {
        Some(t) => format!("(CAST(({value}) AS {t}))"),
        None => format!("({value})"),
    }
}
