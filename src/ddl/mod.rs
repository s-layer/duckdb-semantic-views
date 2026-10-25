// Write-side DDL (CREATE/DROP/ALTER) is parsed by the grammar productions
// (`crate::grammar`) and lowered in `crate::parse`. The legacy table-function
// vtabs (define/drop/alter) and their `persist::execute_parameterized` helper
// were removed in v0.8.0's full architectural unification. Only
// `define::enrich_definition_for_create` remains — called when a CREATE is
// lowered.
pub mod alter_helpers_ffi;
pub mod define;
pub mod describe;
pub mod get_ddl;
pub mod list;
pub mod read_ffi;
pub mod read_yaml;
pub mod show_columns;
pub mod show_dims_for_metric;
pub mod show_entities;
pub mod show_materializations;
