//! `CREATE OR ALTER SEMANTIC VIEW` (Phase 6.6): the definition a statement
//! gives, with what it can't state carried over from the view it alters.
//!
//! As in Snowflake, `CREATE OR ALTER` creates the view when it is missing and
//! otherwise makes it match the statement: a clause or property the statement
//! leaves out is unset. What the statement cannot state is kept:
//!
//! - **Tags.** The statement can't set any (Snowflake: "Any existing tags are
//!   preserved"), so the view's tags, and the tags of each table, dimension,
//!   fact and metric the statement keeps by name, carry over.
//! - **Materializations**, when the statement has no `MATERIALIZATIONS`
//!   clause (Snowflake's CREATE OR ALTER grammar has none; there a
//!   materialization is its own object). One is kept when the statement
//!   leaves the query it answers unchanged: the expansion of its dimensions
//!   and metrics is the same SQL under the new definition as under the old.
//!   Snowflake suspends one whose objects changed; with no refresh to resume
//!   it, this extension drops it.
//! - **`created_on`**: the view is altered, not created again.
//!
//! The outer `INSERT OR REPLACE` the statement lowers to
//! (`parse::native_sql`) reads the new definition through the
//! `__sv_or_alter(new_def, view_name, schema)` scalar, which finds the view
//! being altered in the caller's transaction and calls [`merge`].

use crate::ident::normalize_ident_part;
use crate::model::{SemanticViewDefinition, Tag};

/// Why `def` can't be a `CREATE OR ALTER`'s definition, if it can't: it sets a
/// tag on the view or on one of its objects. Snowflake's statement takes no
/// tags and keeps the existing ones.
#[must_use]
pub fn tag_error(def: &SemanticViewDefinition) -> Option<String> {
    let owner = if def.tags.is_empty() {
        def.tables
            .iter()
            .find(|t| !t.tags.is_empty())
            .map(|t| format!("table '{}'", t.alias))
            .or_else(|| {
                def.dimensions
                    .iter()
                    .find(|d| !d.tags.is_empty())
                    .map(|d| format!("dimension '{}'", d.name))
            })
            .or_else(|| {
                def.facts
                    .iter()
                    .find(|f| !f.tags.is_empty())
                    .map(|f| format!("fact '{}'", f.name))
            })
            .or_else(|| {
                def.metrics
                    .iter()
                    .find(|m| !m.tags.is_empty())
                    .map(|m| format!("metric '{}'", m.name))
            })?
    } else {
        "the view".to_string()
    };
    Some(format!(
        "CREATE OR ALTER SEMANTIC VIEW can't set tags, as in Snowflake, and {owner} has one: an \
         altered view keeps its existing tags. Set the view's own with ALTER SEMANTIC VIEW ... SET \
         TAG, or use CREATE OR REPLACE to give every tag anew"
    ))
}

/// `new`, the definition a `CREATE OR ALTER` gives, with what it can't state
/// taken from `old`, the view it alters (see the module docs). `None` creates
/// the view: `new` as it is.
#[must_use]
pub fn merge(
    view_name: &str,
    mut new: SemanticViewDefinition,
    old: Option<&SemanticViewDefinition>,
) -> SemanticViewDefinition {
    let Some(old) = old else {
        return new;
    };
    new.created_on.clone_from(&old.created_on);
    new.tags.clone_from(&old.tags);
    keep_tags(&mut new.tables, &old.tables, |t| &t.alias, |t| &mut t.tags);
    keep_tags(
        &mut new.dimensions,
        &old.dimensions,
        |d| &d.name,
        |d| &mut d.tags,
    );
    keep_tags(&mut new.facts, &old.facts, |f| &f.name, |f| &mut f.tags);
    keep_tags(&mut new.metrics, &old.metrics, |m| &m.name, |m| &mut m.tags);
    if new.materializations.is_empty() {
        new.materializations = old
            .materializations
            .iter()
            .filter(|mat| answers_the_same(view_name, old, &new, mat))
            .cloned()
            .collect();
    }
    new
}

/// Give each object of `new` the tags of the object of `old` with its name.
fn keep_tags<T: Clone>(
    new: &mut [T],
    old: &[T],
    name: impl Fn(&T) -> &String,
    tags: impl Fn(&mut T) -> &mut Vec<Tag>,
) {
    for n in new.iter_mut() {
        let key = normalize_ident_part(name(n));
        if let Some(o) = old.iter().find(|o| normalize_ident_part(name(o)) == key) {
            let mut o = o.clone();
            *tags(n) = std::mem::take(tags(&mut o));
        }
    }
}

/// Whether `mat` answers the same query under `new` as under `old`: the
/// expansion of its dimensions and metrics, with no materialization to route
/// to and each variable as its default (`variables::Values::Probe`), is the
/// same SQL in both. A materialization naming a member `new` dropped does not
/// expand under `new`, so it is not kept. Routing takes only that exact
/// request with no `where_clause` (`expand::materialization`), so no other
/// query reads the table.
fn answers_the_same(
    view_name: &str,
    old: &SemanticViewDefinition,
    new: &SemanticViewDefinition,
    mat: &crate::model::Materialization,
) -> bool {
    use crate::expand::{expand, DimensionName, MetricName, QueryRequest};
    let req = QueryRequest {
        dimensions: mat
            .dimensions
            .iter()
            .map(|d| DimensionName::new(d.as_str()))
            .collect(),
        metrics: mat
            .metrics
            .iter()
            .map(|m| MetricName::new(m.as_str()))
            .collect(),
        facts: vec![],
        where_clause: None,
    };
    let expansion = |def: &SemanticViewDefinition| {
        let unrouted = SemanticViewDefinition {
            materializations: Vec::new(),
            ..def.clone()
        };
        let bound =
            crate::variables::bind(view_name, unrouted, crate::variables::Values::Probe).ok()?;
        expand(view_name, &bound.def, &req).ok()
    };
    matches!((expansion(old), expansion(new)), (Some(a), Some(b)) if a == b)
}

/// The stored keys [`merge`] can change: those of the objects that carry tags,
/// the materializations and `created_on`.
#[cfg(feature = "extension")]
const MERGED_KEYS: &[&str] = &[
    "created_on",
    "tags",
    "tables",
    "dimensions",
    "facts",
    "metrics",
    "materializations",
];

/// The `__sv_or_alter(new_def, view_name, schema)` scalar: `new_def` merged
/// with the definition of `schema`.`view_name` as the caller's transaction
/// sees it, or `new_def` unchanged when there is none.
///
/// # Safety
///
/// Each pointer must point to the matching number of readable UTF-8 bytes;
/// the out-pointers and `error_buf` are as [`crate::ddl::read_ffi::run_dispatcher`]
/// takes them.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_or_alter_exec_rust(
    def_ptr: *const u8,
    def_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    schema_ptr: *const u8,
    schema_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    crate::ddl::read_ffi::run_dispatcher(
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_or_alter_exec_rust",
        || {
            use crate::ddl::read_ffi::read_str_arg;
            let json = unsafe { read_str_arg(def_ptr, def_len, "definition") }?;
            let name = unsafe { read_str_arg(name_ptr, name_len, "view name") }?;
            let schema = unsafe { read_str_arg(schema_ptr, schema_len, "schema") }?;
            let new = SemanticViewDefinition::from_json(&name, &json)?;
            if let Some(e) = tag_error(&new) {
                return Err(e);
            }
            let defs = crate::ddl::read_ffi::caller_definitions(Some(&name))?;
            let old = defs
                .rows
                .iter()
                .find(|(row_schema, row_name, _)| {
                    row_name.as_deref() == Some(name.as_str())
                        && row_schema
                            .as_deref()
                            .is_some_and(|s| s.eq_ignore_ascii_case(&schema))
                })
                .and_then(|(_, _, d)| d.as_deref())
                .map(|d| SemanticViewDefinition::from_json(&name, d))
                .transpose()?;
            let merged = merge(&name, new, old.as_ref());
            // Only the keys `merge` sets are replaced: the stored JSON carries
            // keys the struct does not (`schema_version`).
            let internal = |e: serde_json::Error| format!("internal error: {e}");
            let mut out: serde_json::Value = serde_json::from_str(&json).map_err(internal)?;
            let merged = serde_json::to_value(&merged).map_err(internal)?;
            if let (Some(out), Some(merged)) = (out.as_object_mut(), merged.as_object()) {
                for key in MERGED_KEYS {
                    match merged.get(*key) {
                        Some(v) => out.insert((*key).to_string(), v.clone()),
                        None => out.remove(*key),
                    };
                }
            }
            serde_json::to_string(&out)
                .map(String::into_bytes)
                .map_err(internal)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{merge, tag_error};
    use crate::model::{SemanticViewDefinition, Tag, TagName};
    use crate::parse::{plan_create, RewriteAction};

    /// The definition `CREATE SEMANTIC VIEW v <body>` plans.
    fn def(body: &str) -> SemanticViewDefinition {
        match plan_create(&format!("CREATE SEMANTIC VIEW v {body}")).unwrap() {
            Some(RewriteAction::Create { def, .. }) => *def,
            other => panic!("not a CREATE: {other:?}"),
        }
    }

    fn tag(name: &str, value: &str) -> Tag {
        Tag {
            name: TagName {
                database: None,
                schema: None,
                tag: name.to_string(),
            },
            value: value.to_string(),
        }
    }

    const BODY: &str = "TABLES (o AS orders PRIMARY KEY (id)) \
        FACTS (o.amt AS o.amount) \
        DIMENSIONS (o.region AS o.region, o.status AS o.status) \
        METRICS (o.revenue AS SUM(o.amt), o.n AS COUNT(*))";

    const MAT: &str = "MATERIALIZATIONS (by_region AS (TABLE agg, DIMENSIONS (region), \
        METRICS (revenue)))";

    fn mats(d: &SemanticViewDefinition) -> Vec<&str> {
        d.materializations.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn a_missing_view_is_created_as_the_statement_gives_it() {
        let new = def(BODY);
        assert_eq!(merge("v", new.clone(), None), new);
    }

    #[test]
    fn created_on_and_the_view_tags_carry_over_and_the_comment_is_the_statements() {
        let mut old = def(&format!("{BODY} COMMENT = 'old'"));
        old.created_on = Some("2026-01-01T00:00:00Z".to_string());
        old.tags = vec![tag("owner", "x")];
        let mut new = def(BODY);
        new.created_on = Some("2026-10-29T00:00:00Z".to_string());
        let got = merge("v", new, Some(&old));
        assert_eq!(got.created_on.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(got.tags, vec![tag("owner", "x")]);
        assert_eq!(got.comment, None);
    }

    /// A member keeps its tags by name, matched as `DuckDB` matches names; a
    /// renamed one is dropped and added, so its tags go.
    #[test]
    fn member_tags_follow_the_name() {
        let mut old = def(BODY);
        old.tables[0].tags = vec![tag("t", "1")];
        old.dimensions[0].tags = vec![tag("d", "1")];
        old.dimensions[1].tags = vec![tag("d", "2")];
        old.facts[0].tags = vec![tag("f", "1")];
        old.metrics[0].tags = vec![tag("m", "1")];
        let new = def("TABLES (o AS orders PRIMARY KEY (id)) \
            FACTS (o.amt AS o.amount * 2) \
            DIMENSIONS (o.\"REGION\" AS upper(o.region), o.state AS o.status) \
            METRICS (o.revenue AS SUM(o.amt), o.n AS COUNT(*))");
        let got = merge("v", new, Some(&old));
        assert_eq!(got.tables[0].tags, vec![tag("t", "1")]);
        assert_eq!(got.dimensions[0].tags, vec![tag("d", "1")]);
        assert_eq!(got.dimensions[1].tags, vec![]);
        assert_eq!(got.facts[0].tags, vec![tag("f", "1")]);
        assert_eq!(got.metrics[0].tags, vec![tag("m", "1")]);
        assert_eq!(got.metrics[1].tags, vec![]);
    }

    #[test]
    fn a_materialization_is_kept_while_its_query_is_unchanged() {
        let old = def(&format!("{BODY} {MAT}"));
        // `status` and `n` change; `by_region` reads neither.
        let new = def("TABLES (o AS orders PRIMARY KEY (id)) \
            FACTS (o.amt AS o.amount) \
            DIMENSIONS (o.region AS o.region, o.status AS lower(o.status)) \
            METRICS (o.revenue AS SUM(o.amt), o.n AS COUNT(o.id)) COMMENT = 'new'");
        assert_eq!(mats(&merge("v", new, Some(&old))), ["by_region"]);
    }

    #[test]
    fn a_materialization_over_a_changed_or_removed_member_is_dropped() {
        let old = def(&format!("{BODY} {MAT}"));
        for (what, body) in [
            ("the metric", BODY.replace("SUM(o.amt)", "MAX(o.amt)")),
            (
                "the fact the metric reads",
                BODY.replace("AS o.amount", "AS o.amount + 1"),
            ),
            (
                "the dimension",
                BODY.replace("AS o.region,", "AS upper(o.region),"),
            ),
            (
                "a removed dimension",
                BODY.replace("o.region AS o.region, ", ""),
            ),
            (
                "a removed metric",
                BODY.replace("o.revenue AS SUM(o.amt), ", ""),
            ),
            ("the table", BODY.replace("AS orders", "AS orders_v2")),
        ] {
            let got = merge("v", def(&body), Some(&old));
            assert!(
                got.materializations.is_empty(),
                "{what} changed: {:?}",
                mats(&got)
            );
        }
    }

    #[test]
    fn a_materializations_clause_replaces_the_old_ones() {
        let old = def(&format!("{BODY} {MAT}"));
        let new = def(&format!(
            "{BODY} MATERIALIZATIONS (by_status AS (TABLE agg2, DIMENSIONS (status), \
             METRICS (n)))"
        ));
        assert_eq!(mats(&merge("v", new, Some(&old))), ["by_status"]);
    }

    /// A member reading a variable answers with the variable's default, so a
    /// changed default changes the query.
    #[test]
    fn a_materialization_over_a_variable_follows_its_default() {
        let body = |default: &str| {
            format!(
                "TABLES (o AS orders PRIMARY KEY (id)) VARIABLES (k INTEGER DEFAULT {default}) \
                 DIMENSIONS (o.region AS o.region) METRICS (o.revenue AS SUM(o.amount * k)) {MAT}"
            )
        };
        let old = def(&body("1"));
        let kept = def(&body("1").replace(MAT, ""));
        let changed = def(&body("2").replace(MAT, ""));
        assert_eq!(mats(&merge("v", kept, Some(&old))), ["by_region"]);
        assert!(merge("v", changed, Some(&old)).materializations.is_empty());
    }

    #[test]
    fn a_tag_anywhere_is_refused_and_named() {
        assert_eq!(tag_error(&def(BODY)), None);
        let mut d = def(BODY);
        d.metrics[1].tags = vec![tag("m", "1")];
        assert!(tag_error(&d).unwrap().contains("metric 'n' has one"));
        d.facts[0].tags = vec![tag("f", "1")];
        assert!(tag_error(&d).unwrap().contains("fact 'amt' has one"));
        d.dimensions[1].tags = vec![tag("d", "1")];
        assert!(tag_error(&d)
            .unwrap()
            .contains("dimension 'status' has one"));
        d.tables[0].tags = vec![tag("t", "1")];
        assert!(tag_error(&d).unwrap().contains("table 'o' has one"));
        d.tags = vec![tag("v", "1")];
        let e = tag_error(&d).unwrap();
        assert!(e.contains("the view has one"), "{e}");
        assert!(
            e.starts_with("CREATE OR ALTER SEMANTIC VIEW can't set tags"),
            "{e}"
        );
    }
}
