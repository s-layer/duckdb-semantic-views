//! Querying a semantic view by name with standard SQL (Phase 5):
//!
//! ```sql
//! SELECT region, AGG(revenue) FROM shop WHERE channel = 'web' GROUP BY region
//! ```
//!
//! `shop` names no table, so `DuckDB` fails to bind the statement. The shim's
//! rebind hook (`SvStandardQuery` in `cpp/src/shim.cpp`) then asks here
//! whether the name is a semantic view and, when it is, for its members. It
//! uses them to rewrite the query block into the `SEMANTIC_VIEW(...)`
//! construct (dimensions from `GROUP BY`, metrics from `AGG(...)` and other
//! aggregates, facts from the rest), and `DuckDB` binds the rewritten
//! statement. Everything past that point, from member resolution to the
//! expansion, is the construct's.

use crate::model::SemanticViewDefinition;

/// One member, as the rewrite sees it: its kind, the logical table it is
/// defined on (empty for a derived metric) and its name, each lower-cased, as
/// `DuckDB` matches identifiers case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub kind: MemberKind,
    pub table: String,
    pub name: String,
}

/// What a name in a standard-SQL query over the view can refer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Dimension,
    Metric,
    Fact,
}

impl MemberKind {
    /// The one-letter tag the shim reads.
    const fn tag(self) -> char {
        match self {
            Self::Dimension => 'D',
            Self::Metric => 'M',
            Self::Fact => 'F',
        }
    }
}

/// The view's members, dimensions first, then metrics, then facts, each in
/// definition order. Private facts and metrics are listed too: the
/// construct refuses them by name, with its own message.
#[must_use]
pub fn members(def: &SemanticViewDefinition) -> Vec<Member> {
    let lower = |s: &str| s.to_lowercase();
    let table = |t: &Option<String>| t.as_deref().map(lower).unwrap_or_default();
    let mut out = Vec::new();
    for d in &def.dimensions {
        out.push(Member {
            kind: MemberKind::Dimension,
            table: table(&d.source_table),
            name: lower(&d.name),
        });
    }
    for m in &def.metrics {
        out.push(Member {
            kind: MemberKind::Metric,
            table: table(&m.source_table),
            name: lower(&m.name),
        });
    }
    for f in &def.facts {
        out.push(Member {
            kind: MemberKind::Fact,
            table: table(&f.source_table),
            name: lower(&f.name),
        });
    }
    out
}

/// The members in the shim's wire format: one line per member,
/// `<kind>\t<table>\t<name>`. Neither a table alias nor a member name can
/// hold a tab or a newline, since both are identifiers the DDL parsed.
#[must_use]
pub fn member_listing(members: &[Member]) -> String {
    let mut out = String::new();
    for m in members {
        out.push(m.kind.tag());
        out.push('\t');
        out.push_str(&m.table);
        out.push('\t');
        out.push_str(&m.name);
        out.push('\n');
    }
    out
}

/// Payload for the shim: empty when `name` is no semantic view the caller
/// can see, else `view\n` followed by [`member_listing`].
///
/// # Safety
///
/// `name_ptr` points to `name_len` UTF-8 bytes; `sp_ptr`/`sp_len` is the
/// search-path payload the other dispatchers take (null and 0 for none).
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_standard_query_view_rust(
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
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
        "sv_standard_query_view_rust",
        || unsafe {
            use crate::ddl::read_ffi::{read_search_path, read_str_arg};
            let raw = read_str_arg(name_ptr, name_len, "view name")?;
            let search_path = read_search_path(sp_ptr, sp_len)?;
            // A name the view-reference parser refuses, or one in another
            // database, is no semantic view: the statement keeps DuckDB's own
            // error for the missing table.
            let Ok(view) = crate::ident::parse_view_ref(&raw) else {
                return Ok(Vec::new());
            };
            let json = match crate::catalog::CatalogReader::lookup_in_caller(&view, &search_path) {
                Ok(Some(json)) => json,
                Ok(None) => return Ok(Vec::new()),
                // A name in another database refers to no view this catalog
                // holds.
                Err(_) if view.database.is_some() => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };
            let def = SemanticViewDefinition::from_json(&view.name, &json)?;
            Ok(format!("view\n{}", member_listing(&members(&def))).into_bytes())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> SemanticViewDefinition {
        SemanticViewDefinition::from_json(
            "shop",
            r#"{
                "tables": [{"alias": "o", "table": "orders"}, {"alias": "C", "table": "customers"}],
                "dimensions": [{"name": "Region", "expr": "c.region", "source_table": "C"}],
                "metrics": [
                    {"name": "revenue", "expr": "SUM(o.amount)", "source_table": "o"},
                    {"name": "aov", "expr": "revenue / order_count"}
                ],
                "facts": [{"name": "amount", "expr": "o.amount", "source_table": "o"}]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn members_are_listed_by_kind_lower_cased() {
        assert_eq!(
            member_listing(&members(&def())),
            "D\tc\tregion\nM\to\trevenue\nM\t\taov\nF\to\tamount\n"
        );
    }
}
