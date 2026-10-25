//! T-14 (code-review 2026-07-16): a DIRECTED CRLF end-to-end body test.
//!
//! The lexer treats `\r` as whitespace (`is_ascii_whitespace`, `lexer.rs`), so
//! a CREATE body with Windows (CRLF) line endings is handled "by construction".
//! But that was only ever *implicit* — no test drove a CRLF body through the
//! real `plan_create` CREATE front door, and the extra `\r` bytes are exactly
//! the kind of thing byte-offset threading (error carets) can silently
//! miscount. These two tests pin both halves:
//!   (a) a CRLF body parses to the *same* definition its LF twin does — no
//!       token is merged, split, or corrupted by the interleaved `\r`;
//!   (b) an error caret in a CRLF body lands on the correct *byte*, i.e. the
//!       `\r`s are counted in the offset rather than dropped.

use semantic_views::parse::{plan_create, CreateMode, RewriteAction};

/// A complete, valid CREATE with LF line endings between every clause and
/// entry. The CRLF twin is produced by replacing each `\n` with `\r\n`.
const LF_DDL: &str = "CREATE OR REPLACE SEMANTIC VIEW crlf_view AS
TABLES (
    o AS orders PRIMARY KEY (id),
    c AS customers PRIMARY KEY (id)
)
RELATIONSHIPS (
    o_to_c AS o(customer_id) REFERENCES c
)
DIMENSIONS (
    o.region AS o.region,
    c.name AS c.name
)
METRICS (
    o.revenue AS SUM(o.amount)
)";

#[test]
fn crlf_body_parses_identically_to_lf() {
    let crlf = LF_DDL.replace('\n', "\r\n");
    assert!(
        crlf.contains("\r\n"),
        "test setup: CRLF conversion happened"
    );

    let lf = plan_create(LF_DDL)
        .expect("LF body must parse")
        .expect("LF body is a valid CREATE");
    let cr = plan_create(&crlf)
        .expect("CRLF body must parse")
        .expect("CRLF body is a valid CREATE");

    match (lf, cr) {
        (
            RewriteAction::Create {
                name: ln,
                def: ld,
                mode: lm,
            },
            RewriteAction::Create {
                name: cn,
                def: cd,
                mode: cm,
            },
        ) => {
            assert_eq!(cn.name, "crlf_view", "view name drift under CRLF");
            assert_eq!(cn, ln);
            assert_eq!(cm, CreateMode::OrReplace);
            assert_eq!(cm, lm);
            // The interleaved `\r` must not perturb any parsed field: the CRLF
            // definition equals its LF twin field-for-field (both run through
            // the same cardinality inference in the CREATE route, so `joins`
            // compares cleanly too).
            assert_eq!(cd.tables, ld.tables, "tables drift under CRLF");
            assert_eq!(cd.joins, ld.joins, "relationships drift under CRLF");
            assert_eq!(cd.dimensions, ld.dimensions, "dimensions drift under CRLF");
            assert_eq!(cd.metrics, ld.metrics, "metrics drift under CRLF");
            // And concretely, the content survived intact (not merely "equal to
            // an equally-broken LF parse").
            assert_eq!(cd.tables.len(), 2);
            assert_eq!(cd.dimensions.len(), 2);
            assert_eq!(cd.dimensions[0].name, "region");
            assert_eq!(cd.dimensions[0].expr, "o.region");
            assert_eq!(cd.dimensions[1].name, "name");
            assert_eq!(cd.metrics.len(), 1);
            assert_eq!(cd.metrics[0].name, "revenue");
            assert_eq!(cd.metrics[0].expr, "SUM(o.amount)");
        }
        (l, c) => panic!("expected two Create actions, got {l:?} / {c:?}"),
    }
}

#[test]
fn crlf_body_error_caret_counts_cr_bytes() {
    // A CRLF body whose second `DIMENSIONS` clause is a duplicate. The caret
    // must anchor on the SECOND `DIMENSIONS` keyword — located here with
    // `rfind` on the actual CRLF string, so its byte index already includes
    // every preceding `\r`. A caret that dropped the `\r` bytes would report a
    // smaller offset and mismatch.
    let lf = "CREATE OR REPLACE SEMANTIC VIEW crlf_dup AS
TABLES (o AS orders PRIMARY KEY (id))
DIMENSIONS (o.region AS o.region)
DIMENSIONS (o.status AS o.status)
METRICS (o.revenue AS SUM(o.amount))";
    let crlf = lf.replace('\n', "\r\n");

    let err = plan_create(&crlf).expect_err("duplicate DIMENSIONS must error");
    assert!(
        err.message
            .contains("Duplicate clause keyword 'DIMENSIONS'"),
        "got: {}",
        err.message
    );
    let second_dimensions = crlf.rfind("DIMENSIONS").expect("two DIMENSIONS present");
    assert_eq!(
        err.position,
        Some(second_dimensions),
        "caret must point at the 2nd DIMENSIONS at its true CRLF byte offset: {}",
        err.message
    );
}
