//! Load-time check that the host is the `DuckDB` release we were built against.
//!
//! The extension declares the `C_STRUCT` ABI pinned to the v1.5.6 C API (see the
//! Makefile), so `DuckDB` itself only checks that the host's C API is at least that
//! old, and loads us into any newer release, `DuckDB` 2.0 included. Our C++ shim is
//! compiled against one exact release and reads host objects by layout, so in any
//! other release it is undefined behaviour: a v1.5.6 build segfaults on `LOAD` in
//! a 2.0 host (`test/integration/test_load_version_guard.py`). The entry point in
//! `lib.rs` therefore compares the two versions before it calls into the shim.
//!
//! The comparison is on the version string only, which is what `DuckDB` compared
//! for us under the previous `C_STRUCT_UNSTABLE` ABI. Source ids are not compared:
//! an engine built from a source tree with no git metadata (the `PyPI` sdist
//! `scripts/ensure_engine.py` builds from, for one) may not carry the real one,
//! and must still load.

/// `Ok` when `host` is the `DuckDB` release `built` names, otherwise the
/// message the failed `LOAD` reports.
///
/// The community repository only has builds for tagged releases, so the
/// `FORCE INSTALL` hint is given only when `host` is one. A development or
/// nightly host (`v1.5.7-dev42`) has a `-` in its version, and no install
/// command would find a build for it.
///
/// # Errors
///
/// Returns the user-facing refusal when the two versions differ.
pub fn check_host_version(built: &str, host: &str) -> Result<(), String> {
    if built == host {
        return Ok(());
    }
    let remedy = if host.contains('-') {
        "Load a semantic_views build compiled against this DuckDB."
    } else {
        "If a semantic_views build for this DuckDB has been published, install it with: \
         FORCE INSTALL semantic_views FROM community;"
    };
    Err(format!(
        "semantic_views was built for DuckDB {built} and cannot run in DuckDB {host}, \
         because it depends on that exact DuckDB release. {remedy}"
    ))
}

#[cfg(test)]
mod tests {
    use super::check_host_version;

    #[test]
    fn the_release_we_were_built_for_loads() {
        assert_eq!(check_host_version("v1.5.6", "v1.5.6"), Ok(()));
    }

    #[test]
    fn a_newer_release_is_refused_with_both_versions_named() {
        let err = check_host_version("v1.5.6", "v2.0.0").unwrap_err();
        assert!(err.contains("built for DuckDB v1.5.6"), "{err}");
        assert!(err.contains("cannot run in DuckDB v2.0.0"), "{err}");
        assert!(err.contains("FORCE INSTALL semantic_views"), "{err}");
    }

    #[test]
    fn an_older_release_is_refused() {
        assert!(check_host_version("v1.5.6", "v1.5.5").is_err());
    }

    /// A development build of the same line is a different engine.
    #[test]
    fn a_development_build_is_not_the_release() {
        assert!(check_host_version("v2.0.0", "v2.0.0-alpha43763").is_err());
    }

    /// The community repository has no build for a development host, so the
    /// refusal must not send the user to an install command that would fail.
    #[test]
    fn a_development_host_is_not_told_to_install_from_community() {
        let err = check_host_version("v1.5.6", "v2.0.0-dev2610011535").unwrap_err();
        assert!(
            err.contains("cannot run in DuckDB v2.0.0-dev2610011535"),
            "{err}"
        );
        assert!(!err.contains("FORCE INSTALL"), "{err}");
        assert!(err.contains("compiled against this DuckDB"), "{err}");
    }
}
