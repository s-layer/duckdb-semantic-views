//! Phase 64 regression test — quoted identifier handling.
//!
//! These inputs were the bug-report reproductions for Phase 64.
//! They must never regress: `plan_create()` must accept each one and produce a
//! `RewriteAction::Create` whose stored view name is the bare unquoted last part
//! of the identifier (e.g. `"memory"."main"."orders_sv"` → `orders_sv`).
//!
//! Companion seed files for the libfuzzer target:
//!   - fuzz/seeds/fuzz_ddl_parse/seed_phase64_quoted_bare.txt
//!   - fuzz/seeds/fuzz_ddl_parse/seed_phase64_quoted_fqn.txt
//!   - fuzz/seeds/fuzz_ddl_parse/seed_phase64_mixed_quoting.txt

use semantic_views::ident::ViewRef;
use semantic_views::parse::{plan_create, RewriteAction};

const QUOTED_BARE_NAME: &str = "CREATE SEMANTIC VIEW \"memory\".\"main\".\"orders_sv\" AS \
                                TABLES (o AS orders PRIMARY KEY (id)) \
                                DIMENSIONS (o.r AS o.r) \
                                METRICS (o.t AS SUM(o.a))";

const QUOTED_FQN: &str = "CREATE OR REPLACE SEMANTIC VIEW \"db\".\"s\".\"v\" AS \
                          TABLES (o AS orders PRIMARY KEY (id)) \
                          DIMENSIONS (o.r AS o.r) \
                          METRICS (o.t AS SUM(o.a))";

const MIXED_QUOTING: &str = "CREATE SEMANTIC VIEW db.\"s\".v AS \
                             TABLES (o AS orders PRIMARY KEY (id)) \
                             DIMENSIONS (o.r AS o.r) \
                             METRICS (o.t AS SUM(o.a))";

/// Plan a CREATE statement and return the reference it captured.
fn created_view_ref(query: &str) -> ViewRef {
    match plan_create(query)
        .expect("parse should not error")
        .expect("rewrite should return Some")
    {
        RewriteAction::Create { name, .. } => name,
        other => panic!("expected RewriteAction::Create, got {other:?}"),
    }
}

/// The bare, normalised name slot of a planned CREATE.
fn created_view_name(query: &str) -> String {
    created_view_ref(query).name
}

#[test]
fn fully_quoted_fqn_normalises_to_bare_name() {
    // "memory"."main"."orders_sv" must be stored as the bare last part, with no
    // quotes anywhere in the NAME. The qualifier is no longer discarded — it
    // now decides which schema the view is created in — so this also pins that
    // the two halves are separated rather than concatenated.
    assert_eq!(created_view_name(QUOTED_BARE_NAME), "orders_sv");
    let r = created_view_ref(QUOTED_BARE_NAME);
    assert_eq!(r.schema.as_deref(), Some("main"));
    assert_eq!(r.database.as_deref(), Some("memory"));
}

#[test]
fn quoted_fqn_short_parts_normalise_to_bare_name() {
    assert_eq!(created_view_name(QUOTED_FQN), "v");
    assert_eq!(created_view_ref(QUOTED_FQN).schema.as_deref(), Some("s"));
}

#[test]
fn mixed_quoting_normalises_to_bare_name() {
    assert_eq!(created_view_name(MIXED_QUOTING), "v");
    assert_eq!(created_view_ref(MIXED_QUOTING).schema.as_deref(), Some("s"));
}
