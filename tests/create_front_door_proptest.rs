//! T-5 (code-review 2026-07-11): drive the FULL `plan_create` CREATE front
//! door with the same hostile-shape definitions the round-trip property uses.
//!
//! `parse_proptest.rs`'s CREATE properties only ever feed the one fixed
//! canonical body (`build_as_body_suffix`, `[a-z_]` names), so the layers the
//! front door owns — `blank_sql_comments`, prefix detection, view-name
//! extraction, and byte-offset threading — were never exercised with quoted
//! identifiers, unicode payloads, or `--` / `/* */` markers embedded in
//! string literals. `roundtrip_proptest.rs` exercised those shapes, but it
//! enters at `parse_keyword_body`, one layer below the front door.
//!
//! This closes that asymmetry: a hostile definition is rendered to a complete
//! `CREATE OR REPLACE SEMANTIC VIEW` statement and pushed through the public
//! `plan_create` entry the parser hook itself calls. The front door must
//! (a) not reject it, and (b) carry the definition through intact.
//!
//! Cardinality inference (run inside the CREATE route) only rewrites `joins`
//! (ref-columns / inferred cardinality), so the assertion compares the
//! inference-invariant fields — tables, dimensions, facts, metrics — against
//! the input definition. Join fidelity is covered by `roundtrip_proptest.rs`.

use proptest::prelude::*;
use semantic_views::parse::{plan_create, CreateMode, RewriteAction};
use semantic_views::render_ddl::render_create_ddl;

mod common;
use common::arb_canonical_def;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn create_front_door_accepts_hostile_bodies(def in arb_canonical_def()) {
        // At least one of DIMENSIONS/METRICS is required by the grammar.
        prop_assume!(!def.dimensions.is_empty() || !def.metrics.is_empty());

        // "rt_view" is a bare, safe view name — all hostile content lives in
        // the body, which is exactly the surface the fixed-body CREATE
        // proptest never varied.
        let ddl = render_create_ddl("rt_view", &def)
            .expect("canonical definitions must render");

        let action = plan_create(&ddl)
            .unwrap_or_else(|e| {
                panic!(
                    "CREATE front door rejected a body that parse_keyword_body accepts: {}\n--- rendered ---\n{ddl}",
                    e.message
                )
            })
            .expect("valid CREATE DDL must produce Some(action)");

        match action {
            RewriteAction::Create { name, def: got, mode } => {
                prop_assert_eq!(name.name, "rt_view", "view name drift\n{}", ddl);
                // render_create_ddl emits `CREATE OR REPLACE`.
                prop_assert_eq!(mode, CreateMode::OrReplace, "mode drift\n{}", ddl);
                // Inference-invariant fields must survive the front door
                // (comment-blanking must not corrupt `--`/`/* */` inside
                // string literals, offset threading must not truncate, etc.).
                prop_assert_eq!(&got.tables, &def.tables, "tables drift\n{}", ddl);
                prop_assert_eq!(&got.dimensions, &def.dimensions, "dimensions drift\n{}", ddl);
                prop_assert_eq!(&got.facts, &def.facts, "facts drift\n{}", ddl);
                prop_assert_eq!(&got.metrics, &def.metrics, "metrics drift\n{}", ddl);
                prop_assert_eq!(&got.tags, &def.tags, "view tags drift\n{}", ddl);
            }
            other => prop_assert!(false, "expected Create route, got: {:?}\n{}", other, ddl),
        }
    }
}
