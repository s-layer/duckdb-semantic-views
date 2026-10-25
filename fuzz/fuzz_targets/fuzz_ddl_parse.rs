#![no_main]
// Phase 64 seed inputs covering quoted-identifier forms live in:
//   fuzz/seeds/fuzz_ddl_parse/seed_phase64_quoted_bare.txt
//   fuzz/seeds/fuzz_ddl_parse/seed_phase64_quoted_fqn.txt
//   fuzz/seeds/fuzz_ddl_parse/seed_phase64_mixed_quoting.txt
// The same inputs are asserted as permanent regression tests in
// tests/quoted_idents_regression.rs (workspace-level cargo test).
use libfuzzer_sys::fuzz_target;
use semantic_views::parse::{plan_create, RewriteAction};
use semantic_views::render_ddl::render_create_ddl;

fuzz_target!(|data: &[u8]| {
    // Reject invalid UTF-8 — not a crash, just out of scope for this target.
    let Ok(query) = std::str::from_utf8(data) else {
        return;
    };

    // plan_create must never panic, whatever the input. Its result (a
    // structured RewriteAction, None, or ParseError) is all acceptable —
    // only a panic would be a fuzz failure.
    {
        if let Ok(Some(RewriteAction::Create { name, def, .. })) = plan_create(query) {
            // Render oracle: a definition the CREATE front door accepted must
            // reconstruct to DDL. `render_create_ddl`'s only error is the
            // legacy case of an empty `tables` list, which CREATE now refuses
            // on both the AS body and FROM YAML (TECH-DEBT #94), so any
            // failure is a bug. `name` is a parsed reference now that
            // semantic views are schema-scoped; the renderer takes the bare
            // name slot, matching the unqualified `CREATE SEMANTIC VIEW <name>`
            // it emits.
            if let Err(e) = render_create_ddl(&name.name, &def) {
                panic!("render_create_ddl failed on a definition plan_create accepted: {e}");
            }
        }
    }
});
