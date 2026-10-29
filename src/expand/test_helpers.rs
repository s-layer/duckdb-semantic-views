//! Shared test helpers for expand submodule tests.
//!
//! Provides builder functions for common SemanticViewDefinition fixtures,
//! following the pattern established in `graph/test_helpers.rs`.

use crate::model::{
    AccessModifier, Dimension, Fact, Join, Materialization, Metric, NonAdditiveDim, NullsOrder,
    SemanticViewDefinition, SortOrder, TableRef, WindowSpec,
};

/// Base orders view: single table, 2 dimensions, 2 metrics.
///
/// - table: "orders" (alias "orders")
/// - dimensions: region, status
/// - metrics: total_revenue = sum(amount), order_count = count(*)
pub(super) fn orders_view() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![TableRef {
            alias: "orders".to_string(),
            table: "orders".to_string(),
            definition: None,
            pk_columns: vec![],
            unique_constraints: vec![],
            comment: None,
            synonyms: vec![],
            tags: Vec::new(),
        }],
        dimensions: vec![
            Dimension {
                name: "region".to_string(),
                expr: "region".to_string(),
                source_table: None,
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                cortex_search_service: None,
                is_enum: false,
                sample_values: Vec::new(),
            },
            Dimension {
                name: "status".to_string(),
                expr: "status".to_string(),
                source_table: None,
                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                cortex_search_service: None,
                is_enum: false,
                sample_values: Vec::new(),
            },
        ],
        metrics: vec![
            Metric {
                name: "total_revenue".to_string(),
                expr: "sum(amount)".to_string(),
                source_table: None,
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
                tags: Vec::new(),
            },
            Metric {
                name: "order_count".to_string(),
                expr: "count(*)".to_string(),
                source_table: None,
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
                tags: Vec::new(),
            },
        ],
        joins: vec![],
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
    }
}

/// Minimal single-dim, single-metric view for focused tests.
///
/// - table: configurable (alias = table name)
/// - dimensions: one with given name/expr
/// - metrics: one with given name/expr
pub(super) fn minimal_def(
    table: &str,
    dim_name: &str,
    dim_expr: &str,
    metric_name: &str,
    metric_expr: &str,
) -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![TableRef {
            alias: table.to_string(),
            table: table.to_string(),
            definition: None,
            pk_columns: vec![],
            unique_constraints: vec![],
            comment: None,
            synonyms: vec![],
            tags: Vec::new(),
        }],
        dimensions: vec![Dimension {
            name: dim_name.to_string(),
            expr: dim_expr.to_string(),
            source_table: None,
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            tags: Vec::new(),
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
        }],
        metrics: vec![Metric {
            name: metric_name.to_string(),
            expr: metric_expr.to_string(),
            source_table: None,
            output_type: None,
            using_relationships: vec![],
            comment: None,
            synonyms: vec![],
            access: AccessModifier::Public,
            non_additive_by: vec![],
            window_spec: None,
            tags: Vec::new(),
        }],
        joins: vec![],
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
    }
}

/// Extension trait for test fixture mutations.
///
/// Allows builder-style chaining: `orders_view().with_dimension(...).with_join(...)`
pub(super) trait TestFixtureExt {
    fn with_dimension(self, name: &str, expr: &str, source_table: Option<&str>) -> Self;
    fn with_metric(self, name: &str, expr: &str, source_table: Option<&str>) -> Self;
    fn with_table(self, alias: &str, table: &str, pk_columns: &[&str]) -> Self;
    fn with_fact(self, name: &str, expr: &str, source_table: &str) -> Self;
    fn with_private_fact(self, name: &str, expr: &str, source_table: &str) -> Self;
    fn with_using_relationship(self, metric_name: &str, relationships: &[&str]) -> Self;
    fn with_pkfk_join(
        self,
        name: &str,
        from_alias: &str,
        to_alias: &str,
        fk_columns: &[&str],
        ref_columns: &[&str],
    ) -> Self;
    fn clear_dimensions(self) -> Self;
    fn clear_metrics(self) -> Self;
    fn with_non_additive_by(
        self,
        metric_name: &str,
        dims: &[(&str, SortOrder, NullsOrder)],
    ) -> Self;
    fn with_window_spec(self, metric_name: &str, spec: WindowSpec) -> Self;
    fn with_materialization(
        self,
        name: &str,
        table: &str,
        dimensions: &[&str],
        metrics: &[&str],
    ) -> Self;
}

impl TestFixtureExt for SemanticViewDefinition {
    fn with_dimension(mut self, name: &str, expr: &str, source_table: Option<&str>) -> Self {
        self.dimensions.push(Dimension {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: source_table.map(|s| s.to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            tags: Vec::new(),
            cortex_search_service: None,
            is_enum: false,
            sample_values: Vec::new(),
        });
        self
    }

    fn with_metric(mut self, name: &str, expr: &str, source_table: Option<&str>) -> Self {
        self.metrics.push(Metric {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: source_table.map(|s| s.to_string()),
            output_type: None,
            using_relationships: vec![],
            comment: None,
            synonyms: vec![],
            access: AccessModifier::Public,
            non_additive_by: vec![],
            window_spec: None,
            tags: Vec::new(),
        });
        self
    }

    fn with_table(mut self, alias: &str, table: &str, pk_columns: &[&str]) -> Self {
        self.tables.push(TableRef {
            alias: alias.to_string(),
            table: table.to_string(),
            definition: None,
            pk_columns: pk_columns.iter().map(|s| s.to_string()).collect(),
            unique_constraints: vec![],
            comment: None,
            synonyms: vec![],
            tags: Vec::new(),
        });
        self
    }

    fn with_fact(mut self, name: &str, expr: &str, source_table: &str) -> Self {
        self.facts.push(Fact {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some(source_table.to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Public,
            tags: Vec::new(),
            sample_values: Vec::new(),
        });
        self
    }

    fn with_private_fact(mut self, name: &str, expr: &str, source_table: &str) -> Self {
        self.facts.push(Fact {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some(source_table.to_string()),
            output_type: None,
            comment: None,
            synonyms: vec![],
            is_filter: false,
            access: AccessModifier::Private,
            tags: Vec::new(),
            sample_values: Vec::new(),
        });
        self
    }

    fn with_using_relationship(mut self, metric_name: &str, relationships: &[&str]) -> Self {
        if let Some(m) = self.metrics.iter_mut().find(|m| m.name == metric_name) {
            m.using_relationships = relationships.iter().map(|s| s.to_string()).collect();
        }
        self
    }

    fn with_pkfk_join(
        mut self,
        name: &str,
        from_alias: &str,
        to_alias: &str,
        fk_columns: &[&str],
        ref_columns: &[&str],
    ) -> Self {
        self.joins.push(Join {
            table: to_alias.to_string(),
            from_alias: from_alias.to_string(),
            fk_columns: fk_columns.iter().map(|s| s.to_string()).collect(),
            ref_columns: ref_columns.iter().map(|s| s.to_string()).collect(),
            name: Some(name.to_string()),
            ..Default::default()
        });
        self
    }

    fn clear_dimensions(mut self) -> Self {
        self.dimensions.clear();
        self
    }

    fn clear_metrics(mut self) -> Self {
        self.metrics.clear();
        self
    }

    fn with_non_additive_by(
        mut self,
        metric_name: &str,
        dims: &[(&str, SortOrder, NullsOrder)],
    ) -> Self {
        if let Some(m) = self.metrics.iter_mut().find(|m| m.name == metric_name) {
            m.non_additive_by = dims
                .iter()
                .map(|(dim, order, nulls)| NonAdditiveDim {
                    dimension: dim.to_string(),
                    order: *order,
                    nulls: *nulls,
                })
                .collect();
        }
        self
    }

    fn with_window_spec(mut self, metric_name: &str, spec: WindowSpec) -> Self {
        if let Some(m) = self.metrics.iter_mut().find(|m| m.name == metric_name) {
            m.window_spec = Some(spec);
        }
        self
    }

    fn with_materialization(
        mut self,
        name: &str,
        table: &str,
        dimensions: &[&str],
        metrics: &[&str],
    ) -> Self {
        self.materializations.push(Materialization {
            name: name.to_string(),
            table: table.to_string(),
            dimensions: dimensions.iter().map(|s| s.to_string()).collect(),
            metrics: metrics.iter().map(|s| s.to_string()).collect(),
        });
        self
    }
}
