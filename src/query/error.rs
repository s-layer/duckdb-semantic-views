use std::fmt;

use crate::expand::ExpandError;

/// Errors that can occur when executing a semantic view query.
#[derive(Debug)]
pub enum QueryError {
    /// The requested semantic view does not exist in the catalog.
    ViewNotFound {
        name: String,
        suggestion: Option<String>,
        available: Vec<String>,
    },
    /// The query specified neither dimensions nor metrics.
    EmptyRequest { view_name: String },
    /// A wildcard (`*` / `prefix*`) in dimensions/metrics/facts failed to
    /// expand (R-3, code-review 2026-07-11: these errors were previously
    /// smuggled through `ExpandError::EmptyRequest`'s `view_name` field,
    /// rendering the diagnostic inside quotes followed by irrelevant
    /// "specify at least one of DIMENSIONS, METRICS or FACTS" advice).
    WildcardExpansion { view_name: String, detail: String },
    /// A query item or `column_order` entry is malformed, or the items ask
    /// for a column layout that cannot be built (two columns with one name).
    InvalidRequest { view_name: String, detail: String },
    /// The expansion engine returned an error.
    ExpandFailed { source: ExpandError },
    /// The expanded SQL failed to execute against `DuckDB`.
    SqlExecution {
        expanded_sql: String,
        duckdb_error: String,
    },
    /// Runtime type mismatch between source query result and bind-time output
    /// declaration. This would cause a hard crash (SIGABRT) in
    /// `duckdb_vector_reference_vector` if not caught.
    TypeMismatch {
        column_index: usize,
        column_name: String,
        source_type_id: u32,
        dest_type_id: u32,
    },
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ViewNotFound {
                name,
                suggestion,
                available,
            } => {
                write!(f, "Semantic view '{name}' not found.")?;
                if let Some(s) = suggestion {
                    write!(f, " Did you mean '{s}'?")?;
                }
                if !available.is_empty() {
                    write!(f, " Available views: [{}].", available.join(", "))?;
                }
                write!(f, " Run SHOW SEMANTIC VIEWS to see all registered views.")
            }
            // R-16 (code-review 2026-07-11): delegate to `ExpandError::EmptyRequest`
            // so the two render identically and can't drift apart. The clone is
            // cheap — this is the error path.
            Self::EmptyRequest { view_name } => write!(
                f,
                "{}",
                ExpandError::EmptyRequest {
                    view_name: view_name.clone(),
                }
            ),
            Self::WildcardExpansion { view_name, detail }
            | Self::InvalidRequest { view_name, detail } => {
                write!(f, "semantic view '{view_name}': {detail}")
            }
            Self::ExpandFailed { source } => {
                write!(f, "{source}")
            }
            Self::SqlExecution {
                expanded_sql,
                duckdb_error,
            } => {
                write!(
                    f,
                    "SQL execution failed: {duckdb_error}\nExpanded SQL:\n{expanded_sql}"
                )
            }
            Self::TypeMismatch {
                column_index,
                column_name,
                source_type_id,
                dest_type_id,
            } => {
                write!(
                    f,
                    "Type mismatch at column {column_index} (\"{column_name}\"): \
                     query result type ID {source_type_id} does not match \
                     bind-declared type ID {dest_type_id}. \
                     This prevents zero-copy vector transfer. \
                     Please report this as a bug."
                )
            }
        }
    }
}

impl std::error::Error for QueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ExpandFailed { source } => Some(source),
            _ => None,
        }
    }
}

impl From<ExpandError> for QueryError {
    fn from(source: ExpandError) -> Self {
        Self::ExpandFailed { source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_expansion_display_renders_detail_directly() {
        // R-3 regression (code-review 2026-07-11): wildcard failures were
        // smuggled through ExpandError::EmptyRequest's view_name field,
        // rendering as `semantic view 'orders: unknown alias ...': specify
        // at least dimensions := [...]` — diagnostic buried, advice wrong.
        let e = QueryError::WildcardExpansion {
            view_name: "orders".to_string(),
            detail: "unknown alias 'x' in wildcard 'x.*'".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "semantic view 'orders': unknown alias 'x' in wildcard 'x.*'"
        );
    }

    #[test]
    fn view_not_found_points_at_show_semantic_views() {
        // The listing hint names the documented DDL, not the table function
        // `SHOW SEMANTIC VIEWS` lowers to (`list_semantic_views()` is an
        // implementation detail of that statement).
        let e = QueryError::ViewNotFound {
            name: "ordrs".to_string(),
            suggestion: Some("orders".to_string()),
            available: vec!["orders".to_string()],
        };
        assert_eq!(
            e.to_string(),
            "Semantic view 'ordrs' not found. Did you mean 'orders'? \
             Available views: [orders]. \
             Run SHOW SEMANTIC VIEWS to see all registered views."
        );
    }

    #[test]
    fn empty_request_message_matches_expand_error_verbatim() {
        // R-16 (code-review 2026-07-11): `QueryError::EmptyRequest`'s Display
        // delegates to `ExpandError::EmptyRequest`, so the two are guaranteed to
        // render identically. Pin it so the wording can't drift apart again
        // (the original defect this refactor removed).
        let query_side = QueryError::EmptyRequest {
            view_name: "orders".to_string(),
        };
        let expand_side = ExpandError::EmptyRequest {
            view_name: "orders".to_string(),
        };
        assert_eq!(query_side.to_string(), expand_side.to_string());
        // And that the single source carries the current, fuller wording.
        assert!(query_side.to_string().contains("facts := [...]"));
        assert!(query_side
            .to_string()
            .contains("DESCRIBE SEMANTIC VIEW orders"));
    }
}
