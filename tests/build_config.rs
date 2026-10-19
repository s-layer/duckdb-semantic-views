//! Build-plumbing guards for the decisions that pick *which DuckDB* a build
//! talks to: the engine the C++ shim is compiled and linked against
//! (`scripts/ensure_engine.py`) and the engine the sqllogictest runner is
//! installed from (`DUCKDB_TEST_VERSION` in the `Makefile`).
//!
//! Both are invisible to every other test in this repo — `cargo test` links a
//! bundled DuckDB and never reads `cpp/engine.manifest`, and sqllogictest only
//! ever sees whichever engine the harness already chose. The failure they guard
//! against is therefore not a wrong answer but a wrong *build*, and it shows up
//! only in a downstream distribution's CI. The Haybarn build (an alternative
//! DuckDB distribution that builds community extensions against its own engine
//! fork) failed `make configure` on every platform, because this Makefile
//! pinned `DUCKDB_TEST_VERSION` to an upstream release number that exists in no
//! index of theirs — and had it got past that, it would have compiled the shim
//! against upstream headers describing a different engine than the one loading
//! the result.
//!
//! The tests drive the real `Makefile` and the real script in throwaway
//! directories: no network (the sdist is a local archive passed with
//! `--sdist`), no real engine build (`cmake` on `PATH` is a stub that records
//! what it was asked for and prints the version line the real configure step
//! prints), and no dependence on this repo's own `.engine/` state.

use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn pin(name: &str) -> Option<String> {
    std::fs::read_to_string(repo_root().join(name))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The engine release the extension targets, e.g. `v2.0.0`. Read rather than
/// hardcoded so a version bump does not turn these guards red.
fn target_version() -> String {
    pin(".duckdb-version").expect("read .duckdb-version")
}

/// The `duckdb` PyPI release of that engine.
fn target_pypi_version() -> String {
    pin(".duckdb-pypi-version")
        .unwrap_or_else(|| target_version().trim_start_matches('v').to_string())
}

/// A fresh scratch directory, removed first so a crashed previous run cannot
/// leak state into this one.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sv-build-config-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, contents).expect("write file");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "user.name=test",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git (is it on PATH?)");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// `Makefile`: which DuckDB the test runner is installed from
// ---------------------------------------------------------------------------

/// Copies the real `Makefile` and pin files into a scratch project with *stub*
/// ci-tools makefiles, so `make` can expand its variables without the submodule
/// and without any of base.Makefile's own definitions colouring the result.
fn make_project(name: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::copy(repo_root().join("Makefile"), dir.join("Makefile")).expect("copy Makefile");
    for file in [
        ".duckdb-version",
        ".duckdb-capi-version",
        ".duckdb-pypi-version",
    ] {
        if repo_root().join(file).exists() {
            std::fs::copy(repo_root().join(file), dir.join(file))
                .unwrap_or_else(|e| panic!("copy {file}: {e}"));
        }
    }
    for stub in ["base.Makefile", "rust.Makefile"] {
        write(
            &dir.join("extension-ci-tools/makefiles/c_api_extensions")
                .join(stub),
            "# stub for tests/build_config.rs\n",
        );
    }
    // `include`d rather than appended so the Makefile under test stays byte-identical.
    write(
        &dir.join("print.mk"),
        "include Makefile\nprint-%:\n\t@echo '$($*)'\n",
    );
    dir
}

/// Runs `make` in the scratch project with `env` applied on top of a clean
/// slate for the variables under test.
fn make_command(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new("make");
    cmd.args(args).current_dir(dir);
    // The ambient environment must not leak into the expansion: CI and local
    // shells may already export the very variables under test.
    for key in ["DUCKDB_GIT_VERSION", "DUCKDB_TEST_VERSION", "DUCKDB_TAG"] {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run make (is `make` on PATH?)")
}

/// Expands `var` in the scratch project with `env` applied.
fn make_var(dir: &Path, var: &str, env: &[(&str, &str)]) -> String {
    let out = make_command(dir, &["-f", "print.mk", &format!("print-{var}")], env);
    assert!(
        out.status.success(),
        "make failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Locally and in upstream CI the runner must be pinned to the engine this
/// extension targets, so the engine under test cannot drift from the engine the
/// binary was built for.
#[test]
fn test_runner_pin_applies_to_the_upstream_engine() {
    let dir = make_project("pin-upstream");
    let pypi = target_pypi_version();

    // No harness: a local `make configure`.
    assert_eq!(make_var(&dir, "DUCKDB_TEST_VERSION", &[]), pypi);
    // Upstream CI building a release tag, which names the release verbatim.
    assert_eq!(
        make_var(
            &dir,
            "DUCKDB_TEST_VERSION",
            &[("DUCKDB_GIT_VERSION", &target_version())]
        ),
        pypi
    );
}

/// Our build workflows name a pre-release engine by its commit and pass the
/// release label as `duckdb_tag`. The tag is what identifies the engine then,
/// so the runner is still pinned; the same commit without the tag is not ours
/// to vouch for.
#[test]
fn test_runner_pin_follows_the_duckdb_tag() {
    let dir = make_project("pin-tag");
    let commit = "d4355d01c59362d57036ad377735dd5340276a5b";
    assert_eq!(
        make_var(
            &dir,
            "DUCKDB_TEST_VERSION",
            &[
                ("DUCKDB_GIT_VERSION", commit),
                ("DUCKDB_TAG", &target_version())
            ]
        ),
        target_pypi_version()
    );
    assert_eq!(
        make_var(
            &dir,
            "DUCKDB_TEST_VERSION",
            &[("DUCKDB_GIT_VERSION", commit)]
        ),
        "",
        "an untagged commit must leave the runner to the harness"
    );
}

/// A harness building against some *other* engine (a distribution fork, a
/// nightly `main`) ships its own test runner under its own versioning scheme.
/// Pinning our release number there resolves to a version that does not exist
/// and fails `make configure` — the Haybarn breakage this guards.
#[test]
fn test_runner_pin_skipped_for_a_foreign_engine() {
    let dir = make_project("pin-foreign");
    for engine in ["haybarn-v1.5.5-rc1", "main"] {
        assert_eq!(
            make_var(
                &dir,
                "DUCKDB_TEST_VERSION",
                &[("DUCKDB_GIT_VERSION", engine)]
            ),
            "",
            "DUCKDB_TEST_VERSION must be left to the harness when building against {engine}"
        );
    }
}

/// Even on the upstream engine an explicit value from the environment wins, so
/// a harness can always name the runner build it wants.
#[test]
fn test_runner_pin_yields_to_an_explicit_override() {
    let dir = make_project("pin-override");
    assert_eq!(
        make_var(
            &dir,
            "DUCKDB_TEST_VERSION",
            &[("DUCKDB_TEST_VERSION", "1.5.5rc1")]
        ),
        "1.5.5rc1"
    );
}

/// The binary's metadata and `DUCKDB_EXTENSION_MIN_DUCKDB_VERSION` carry the C
/// API version, which since 2.0 is not the engine version: stamping the engine
/// version there would declare a C API the binary was not built against.
#[test]
fn target_version_is_the_c_api_pin_not_the_engine() {
    let dir = make_project("capi");
    assert_eq!(
        make_var(&dir, "TARGET_DUCKDB_VERSION", &[]),
        pin(".duckdb-capi-version").expect("read .duckdb-capi-version")
    );
    assert_eq!(
        make_var(&dir, "ENGINE_DUCKDB_VERSION", &[]),
        target_version()
    );
}

/// extension-ci-tools runs `make set_duckdb_tag` after cloning the engine when
/// the caller passes `duckdb_tag`; base.Makefile's rule does nothing for C API
/// extensions, so ours must actually tag the clone, or the engine is built
/// under a development label and the extension loads in no released DuckDB.
#[test]
fn set_duckdb_tag_tags_the_engine_clone() {
    let dir = make_project("set-tag");
    let engine = dir.join("duckdb");
    std::fs::create_dir_all(&engine).expect("create engine dir");
    git(&engine, &["init", "-q"]);
    git(&engine, &["commit", "-q", "--allow-empty", "-m", "engine"]);

    let out = make_command(
        &dir,
        &["set_duckdb_tag"],
        &[("DUCKDB_TAG", "v2.0.0-alpha1")],
    );
    assert!(
        out.status.success(),
        "make set_duckdb_tag failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tags = Command::new("git")
        .args(["tag", "--points-at", "HEAD"])
        .current_dir(&engine)
        .output()
        .expect("run git tag");
    assert_eq!(
        String::from_utf8_lossy(&tags.stdout).trim(),
        "v2.0.0-alpha1"
    );

    // A re-run in the same clone is a no-op...
    let again = make_command(
        &dir,
        &["set_duckdb_tag"],
        &[("DUCKDB_TAG", "v2.0.0-alpha1")],
    );
    assert!(
        again.status.success(),
        "a re-run must succeed: {}",
        String::from_utf8_lossy(&again.stderr)
    );
    // ...but a tag of that name somewhere else is never moved.
    git(
        &engine,
        &["commit", "-q", "--allow-empty", "-m", "past the tag"],
    );
    let moved = make_command(
        &dir,
        &["set_duckdb_tag"],
        &[("DUCKDB_TAG", "v2.0.0-alpha1")],
    );
    assert!(
        !moved.status.success(),
        "a tag that is not at HEAD must fail the run, not move"
    );
}

// ---------------------------------------------------------------------------
// `scripts/ensure_engine.py`: which DuckDB the C++ shim is built against
// ---------------------------------------------------------------------------

/// The `cmake` the script finds on `PATH` in these tests. It mimics the two
/// calls the script makes:
///
/// * configure (`-S <src> -B <build> ...`) labels the build the way DuckDB's
///   CMake does — `DUCKDB_VERSION`, else `OVERRIDE_GIT_DESCRIBE`, else a
///   `-devN` label — and prints the same `git hash ..., version ...,` line;
///   `STUB_LABEL` overrides that, to play an engine that labels itself wrongly;
/// * build (`--build <build> ...`) writes the static libraries the real
///   `duckdb_static` target leaves behind, each holding the source tree's
///   `MARKER` and the label, plus a jemalloc library that must not be linked.
///
/// Every call is appended to `$STUB_LOG`, and each configure's `-D` flags to
/// `$STUB_LOG.args`.
const STUB_CMAKE: &str = r#"#!/usr/bin/env python3
import os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["STUB_LOG"], "a") as log:
    if "-S" in args:
        src = args[args.index("-S") + 1]
        build = pathlib.Path(args[args.index("-B") + 1])
        label = (os.environ.get("STUB_LABEL") or os.environ.get("DUCKDB_VERSION")
                 or os.environ.get("OVERRIDE_GIT_DESCRIBE") or "v2.0.0-dev7")
        generator = args[args.index("-G") + 1] if "-G" in args else "default"
        cache = build / "CMakeCache.txt"
        if cache.exists() and cache.read_text() != generator:
            sys.exit("CMake Error: generator does not match the generator used previously")
        build.mkdir(parents=True, exist_ok=True)
        cache.write_text(generator)
        (build / "stub-src").write_text(src)
        (build / "stub-label").write_text(label)
        log.write(f"configure {label}\n")
        with open(os.environ["STUB_LOG"] + ".args", "a") as args_log:
            args_log.write(" ".join(a for a in args if a.startswith("-D")) + "\n")
        print(f"-- git hash 0123456789, version {label}, extension folder {label}")
    elif args[:1] == ["--build"]:
        build = pathlib.Path(args[1])
        marker = (pathlib.Path((build / "stub-src").read_text()) / "MARKER").read_text().strip()
        label = (build / "stub-label").read_text()
        for lib in ["src/libduckdb_static.a", "third_party/fmt/libduckdb_fmt.a",
                    "third_party/jemalloc/libduckdb_jemalloc.a"]:
            path = build / lib
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"{marker} {label}\n")
        log.write("build\n")
    else:
        sys.exit(f"stub cmake: unexpected call {args}")
"#;

/// A scratch repo root with the pin files and a stub `cmake` on a private PATH
/// entry.
fn script_project(name: &str) -> PathBuf {
    let dir = scratch(name);
    write(
        &dir.join(".duckdb-version"),
        &format!("{}\n", target_version()),
    );
    write(
        &dir.join(".duckdb-pypi-version"),
        &format!("{}\n", target_pypi_version()),
    );
    let stub = dir.join("stub-bin/cmake");
    write(&stub, STUB_CMAKE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub cmake");
    }
    dir
}

/// Writes a minimal engine source tree at `dir`: what the script checks for
/// (`CMakeLists.txt`, `src/include/duckdb`) plus a third-party include
/// directory and the `MARKER` the stub build stamps into the libraries.
fn write_engine_source(dir: &Path, marker: &str) {
    write(&dir.join("CMakeLists.txt"), "# stub engine\n");
    write(&dir.join("src/include/duckdb.hpp"), "// stub\n");
    write(&dir.join("src/include/duckdb/main/stub.hpp"), "// stub\n");
    write(
        &dir.join("third_party/fmt/include/fmt/format.h"),
        "// stub\n",
    );
    // Like the real `third_party/brotli`: an `include/` directory, and next to
    // it a `VERSION` file that a case-insensitive file system finds for
    // `#include <version>`.
    write(
        &dir.join("third_party/brotli/include/brotli/decode.h"),
        "// stub\n",
    );
    write(&dir.join("third_party/brotli/VERSION"), "1.1.0\n");
    // Like `third_party/re2`: headers at the root, no `include/`.
    write(&dir.join("third_party/re2/re2/re2.h"), "// stub\n");
    write(&dir.join("MARKER"), &format!("{marker}\n"));
}

/// Builds a local stand-in for the `duckdb` sdist — the engine source under
/// `duckdb-<pypi>/external/duckdb/`, next to the Python package's own files —
/// and returns its path, for `--sdist`.
fn plant_sdist(root: &Path, marker: &str) -> PathBuf {
    plant_sdist_of(root, marker, &target_pypi_version())
}

/// The same, for the `duckdb` release `pypi` (its `PKG-INFO` says so).
fn plant_sdist_of(root: &Path, marker: &str, pypi: &str) -> PathBuf {
    let staging = root.join("sdist-staging");
    let top = staging.join(format!("duckdb-{pypi}"));
    write(
        &top.join("pyproject.toml"),
        "# the Python package, not the engine\n",
    );
    write(
        &top.join("PKG-INFO"),
        &format!("Metadata-Version: 2.1\nName: duckdb\nVersion: {pypi}\n"),
    );
    write_engine_source(&top.join("external/duckdb"), marker);
    let archive = root.join(format!("duckdb-{pypi}.tar.gz"));
    const MAKE_TAR: &str = "import sys, tarfile\n\
         with tarfile.open(sys.argv[1], 'w:gz') as t:\n    \
             t.add(sys.argv[2], arcname=sys.argv[3])\n";
    let status = Command::new("python3")
        .arg("-c")
        .arg(MAKE_TAR)
        .arg(&archive)
        .arg(&top)
        .arg(top.file_name().expect("name"))
        .status()
        .expect("run python3 (is it on PATH?)");
    assert!(status.success(), "failed to build the stub sdist");
    std::fs::remove_dir_all(&staging).expect("remove sdist staging");
    archive
}

/// Plants an engine source tree at `<root>/duckdb`, as a harness clones one.
fn plant_engine_tree(root: &Path, marker: &str) -> PathBuf {
    let tree = root.join("duckdb");
    write_engine_source(&tree, marker);
    tree
}

/// The same, as a git checkout, optionally at a tag.
fn plant_git_engine_tree(root: &Path, marker: &str, tag: Option<&str>) -> PathBuf {
    let tree = plant_engine_tree(root, marker);
    git(&tree, &["init", "-q"]);
    git(&tree, &["add", "-A"]);
    git(&tree, &["commit", "-qm", "engine"]);
    if let Some(tag) = tag {
        git(&tree, &["tag", tag]);
    }
    tree
}

/// Runs the script with the stub `cmake` first on PATH and `env` on top of an
/// environment cleared of the labels a harness sets.
fn run_script(root: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let path = format!(
        "{}:{}",
        root.join("stub-bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("python3");
    cmd.arg(repo_root().join("scripts/ensure_engine.py"))
        .arg("--repo-root")
        .arg(root)
        .args(args)
        .env("PATH", path)
        .env("STUB_LOG", root.join("cmake.log"));
    for key in [
        "DUCKDB_VERSION",
        "OVERRIDE_GIT_DESCRIBE",
        "STUB_LABEL",
        "OSX_BUILD_ARCH",
    ] {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run scripts/ensure_engine.py")
}

fn run_script_ok(root: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = run_script(root, args, env);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "ensure_engine.py failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// What the stub `cmake` was asked to do, one line per call.
fn cmake_calls(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("cmake.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The `-D` flags of each configure call, one string per call.
fn configure_args(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("cmake.log.args"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The `.engine/` entry the pinned release is cached under, as the script
/// names it: the wheel pin joins the id when it is not the plain version.
fn sdist_engine_id() -> String {
    let version = target_version();
    let pypi = target_pypi_version();
    let wheel = if pypi == version.trim_start_matches('v') {
        String::new()
    } else {
        format!("-py{pypi}")
    };
    format!("{version}{wheel}-{LAYOUT}")
}

/// `LAYOUT` in `scripts/ensure_engine.py`.
const LAYOUT: &str = "3";

/// The installed manifest as (version, include dirs, libraries).
fn manifest(root: &Path) -> (String, Vec<String>, Vec<String>) {
    let text = read(&root.join("cpp/engine.manifest"));
    let mut version = String::new();
    let (mut includes, mut libs) = (Vec::new(), Vec::new());
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("version ") {
            version = v.to_string();
        } else if let Some(p) = line.strip_prefix("include ") {
            includes.push(p.to_string());
        } else if let Some(p) = line.strip_prefix("lib ") {
            libs.push(p.to_string());
        }
    }
    (version, includes, libs)
}

/// What the linked `duckdb_static` was built from: `<MARKER> <label>`.
fn linked_engine(root: &Path) -> String {
    let (_, _, libs) = manifest(root);
    let core = libs
        .iter()
        .find(|l| l.ends_with("libduckdb_static.a"))
        .unwrap_or_else(|| panic!("no duckdb_static in the manifest: {libs:?}"));
    read(Path::new(core)).trim().to_string()
}

/// The default path for a developer and for our own CI: no engine tree, so the
/// pinned release is built from its sdist, labelled with the pin, and the
/// manifest points `build.rs` at it.
#[test]
fn pinned_release_is_built_from_the_sdist_under_the_pin() {
    let root = script_project("release");
    let sdist = plant_sdist(&root, "SDIST");

    run_script_ok(&root, &["--sdist", sdist.to_str().expect("utf-8")], &[]);

    let (version, includes, libs) = manifest(&root);
    assert_eq!(version, target_version());
    assert_eq!(linked_engine(&root), format!("SDIST {}", target_version()));
    assert_eq!(
        cmake_calls(&root),
        ["configure ".to_string() + &target_version(), "build".into()]
    );
    // The headers the shim compiles against come from the same extracted source.
    assert!(
        includes
            .iter()
            .any(|i| i.ends_with("src/include") && Path::new(i).join("duckdb.hpp").exists()),
        "the engine's src/include must be on the include path: {includes:?}"
    );
    assert!(
        includes
            .iter()
            .any(|i| i.ends_with("third_party/fmt/include")),
        "third-party include directories must be on the include path: {includes:?}"
    );
    // The extension must use the host's allocator.
    assert!(
        libs.iter().any(|l| l.ends_with("libduckdb_fmt.a")),
        "third-party libraries must be linked: {libs:?}"
    );
    assert!(
        !libs.iter().any(|l| l.contains("jemalloc")),
        "jemalloc must never be linked into the extension: {libs:?}"
    );
    // The Python package around the engine is not part of it.
    assert!(!root
        .join(format!(".engine/{}/src/pyproject.toml", sdist_engine_id()))
        .exists());
}

/// A third-party library with an `include/` directory is reached through
/// that directory only, as DuckDB's own CMake does. Its root holds sources and
/// files such as brotli's `VERSION`, which macOS's case-insensitive file system
/// returns for `#include <version>`, so putting the root on the include path
/// breaks the shim's compile there.
#[test]
fn a_third_party_root_with_an_include_directory_is_not_on_the_include_path() {
    let root = script_project("third-party-includes");
    let sdist = plant_sdist(&root, "SDIST");

    run_script_ok(&root, &["--sdist", sdist.to_str().expect("utf-8")], &[]);

    let (_, includes, _) = manifest(&root);
    for name in ["brotli", "fmt"] {
        assert!(
            includes
                .iter()
                .any(|i| i.ends_with(&format!("third_party/{name}/include"))),
            "{name}'s include directory must be on the include path: {includes:?}"
        );
        assert!(
            !includes
                .iter()
                .any(|i| i.ends_with(&format!("third_party/{name}"))),
            "{name}'s root must not be on the include path: {includes:?}"
        );
    }
    assert!(
        includes.iter().any(|i| i.ends_with("third_party/re2")),
        "a library with no include directory is reached through its root: {includes:?}"
    );
}

/// A configure that stamps a label other than the pin would give the
/// extension a version check that refuses the very host it was built for (or
/// accepts the wrong one). That must stop the build, and leave nothing behind
/// that a later run would mistake for a good build.
///
/// This is a backstop, not proof the sdist is the pinned engine: DuckDB's real
/// CMake stamps the `DUCKDB_VERSION` the script sets, so only a CMake that
/// labels differently reaches it (here, the stub's `STUB_LABEL`). The archive
/// is checked by `an_sdist_of_another_release_is_refused`, and the wheel pin
/// against the engine pin by `test/integration/test_engine_pins.py`.
#[test]
fn a_configure_label_other_than_the_pin_is_fatal_and_poisons_nothing() {
    let root = script_project("release-mislabelled");
    let sdist = plant_sdist(&root, "SDIST");

    let out = run_script(
        &root,
        &["--sdist", sdist.to_str().expect("utf-8")],
        &[("STUB_LABEL", "v1.4.0")],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a mislabelled engine must fail the build"
    );
    assert!(
        stderr.contains("v1.4.0") && stderr.contains(&target_version()),
        "the error must name both versions, got: {stderr}"
    );
    assert!(
        !root.join("cpp/engine.manifest").exists(),
        "nothing may be installed"
    );
    let stamp = root.join(format!(".engine/{}/manifest", sdist_engine_id()));
    assert!(
        !stamp.exists(),
        "a failed build must not be recorded in the cache"
    );

    // And the next run builds again rather than restoring it.
    run_script_ok(&root, &["--sdist", sdist.to_str().expect("utf-8")], &[]);
    assert_eq!(manifest(&root).0, target_version());
}

/// When the harness supplied the engine the extension will be loaded into, that
/// tree is built rather than the pinned release.
#[test]
fn an_engine_source_tree_wins_over_the_pinned_release() {
    let root = script_project("tree");
    plant_engine_tree(&root, "TREE");
    // No sdist: reaching the pinned path fails the test.
    let missing = root.join("no-such-sdist.tar.gz");

    let out = run_script_ok(
        &root,
        &["--sdist", missing.to_str().expect("utf-8")],
        &[("DUCKDB_VERSION", &target_version())],
    );

    assert_eq!(linked_engine(&root), format!("TREE {}", target_version()));
    assert!(
        out.contains("engine source tree"),
        "expected the tree to be reported: {out}"
    );
}

/// community-extensions exports `DUCKDB_VERSION` as the engine's commit, which
/// DuckDB's CMake would stamp into the library verbatim — and no host reports a
/// commit as its version. A tree checked out at a release tag is built under
/// that tag instead, as the release itself was.
#[test]
fn a_commit_label_from_the_harness_gives_way_to_the_trees_release_tag() {
    let root = script_project("tree-tag");
    plant_git_engine_tree(&root, "TREE", Some("v2.0.0"));

    run_script_ok(
        &root,
        &[],
        &[("DUCKDB_VERSION", "d4355d01c59362d57036ad377735dd5340276a5b")],
    );

    assert_eq!(cmake_calls(&root)[0], "configure v2.0.0");
    assert_eq!(manifest(&root).0, "v2.0.0");
}

/// A development tree is no release. Given only a commit by the harness, it
/// keeps CMake's own `-devN` label rather than borrowing our pin or the
/// release tag it has moved past, and the build says plainly that it will
/// load only in that engine. (`v1.5.6-15774-g<sha>` read as `v1.5.6` is how
/// a development tree once passed for the release: #261.)
#[test]
fn a_development_tree_past_its_release_tag_keeps_cmakes_own_label() {
    let root = script_project("tree-dev");
    let tree = plant_git_engine_tree(&root, "TREE", Some("v2.0.0"));
    git(
        &tree,
        &["commit", "-q", "--allow-empty", "-m", "past the release"],
    );

    let out = run_script_ok(
        &root,
        &[],
        &[("DUCKDB_VERSION", "d4355d01c59362d57036ad377735dd5340276a5b")],
    );

    assert_eq!(cmake_calls(&root)[0], "configure v2.0.0-dev7");
    assert_eq!(manifest(&root).0, "v2.0.0-dev7");
    assert!(
        out.contains("will load only in DuckDB v2.0.0-dev7"),
        "a build for another engine must say so: {out}"
    );
    assert!(
        out.contains("commit count"),
        "the fallback label must be reported as the guess it is: {out}"
    );
}

/// A branch name is which source to build, not what it is called: a harness
/// that builds `main` gets no `main` label.
#[test]
fn a_branch_name_from_the_harness_is_not_a_label() {
    let root = script_project("tree-branch");
    let tree = plant_git_engine_tree(&root, "TREE", None);
    git(&tree, &["branch", "-M", "main"]);

    run_script_ok(&root, &[], &[("DUCKDB_VERSION", "main")]);

    assert_eq!(cmake_calls(&root)[0], "configure v2.0.0-dev7");
}

/// A harness label that is not a release label -- a fork's nightly, a
/// `git describe` -- is what the harness's own engine build is stamped with,
/// so the extension is built under it verbatim. In particular it is never cut
/// down to the release label it starts with.
#[test]
fn a_fork_or_describe_label_from_the_harness_is_used_verbatim() {
    for (name, label) in [
        ("tree-fork", "haybarn-v1.5.5-rc1-42-g0123456789"),
        ("tree-describe", "v1.5.6-15774-gb15358c371d"),
    ] {
        let root = script_project(name);
        plant_git_engine_tree(&root, "TREE", Some("v1.5.6"));

        run_script_ok(
            &root,
            &[],
            &[
                ("DUCKDB_VERSION", "d4355d01c59362d57036ad377735dd5340276a5b"),
                ("OVERRIDE_GIT_DESCRIBE", label),
            ],
        );

        assert_eq!(cmake_calls(&root)[0], format!("configure {label}"));
        assert_eq!(manifest(&root).0, label);
    }
}

/// A harness label that *is* a release label is the harness telling us which
/// release its tree is, and is used as given.
#[test]
fn a_release_label_from_the_harness_is_used_as_given() {
    let root = script_project("tree-harness-label");
    plant_git_engine_tree(&root, "TREE", None);

    run_script_ok(&root, &[], &[("OVERRIDE_GIT_DESCRIBE", "v2.0.0-rc1")]);

    assert_eq!(cmake_calls(&root)[0], "configure v2.0.0-rc1");
}

/// A git tree is cached under its commit: the same commit again is a cache hit,
/// a new commit is a new engine.
#[test]
fn a_tree_is_cached_under_its_commit() {
    let root = script_project("tree-cache");
    let tree = plant_git_engine_tree(&root, "TREE", Some("v2.0.0"));
    run_script_ok(&root, &[], &[]);
    run_script_ok(&root, &[], &[]);
    assert_eq!(
        cmake_calls(&root).len(),
        2,
        "the second run must be a cache hit"
    );

    write(&tree.join("MARKER"), "TREE-NEXT\n");
    git(&tree, &["commit", "-qam", "next"]);
    run_script_ok(&root, &[], &[]);
    assert_eq!(cmake_calls(&root).len(), 4, "a new commit must rebuild");
    assert!(linked_engine(&root).starts_with("TREE-NEXT "));
}

/// A tree with no commit has nothing to cache it under, so it is built every
/// time rather than served whatever an earlier run left.
#[test]
fn a_tree_that_is_not_a_git_checkout_is_never_served_from_the_cache() {
    let root = script_project("tree-no-git");
    let tree = plant_engine_tree(&root, "FIRST");
    run_script_ok(&root, &[], &[]);
    write(&tree.join("MARKER"), "SECOND\n");
    run_script_ok(&root, &[], &[]);

    assert_eq!(cmake_calls(&root).len(), 4, "both runs must build");
    assert!(linked_engine(&root).starts_with("SECOND "));
}

/// A `duckdb/` directory that is not an engine (an empty submodule mount, a
/// checkout of the Python package) is not mistaken for one.
#[test]
fn a_duckdb_directory_that_is_not_an_engine_is_ignored() {
    let root = script_project("not-an-engine");
    write(&root.join("duckdb/pyproject.toml"), "# not an engine\n");
    let sdist = plant_sdist(&root, "SDIST");

    run_script_ok(&root, &["--sdist", sdist.to_str().expect("utf-8")], &[]);

    assert_eq!(linked_engine(&root), format!("SDIST {}", target_version()));
}

/// A version bump must build the new release, not keep linking the old one.
#[test]
fn a_version_bump_rebuilds() {
    let root = script_project("version-bump");
    let sdist = plant_sdist(&root, "SDIST");
    let sdist = sdist.to_str().expect("utf-8");

    run_script_ok(&root, &["--sdist", sdist, "--version", "v2.0.0"], &[]);
    run_script_ok(&root, &["--sdist", sdist, "--version", "v2.0.1"], &[]);

    assert_eq!(
        cmake_calls(&root),
        ["configure v2.0.0", "build", "configure v2.0.1", "build"]
    );
    assert_eq!(manifest(&root).0, "v2.0.1");
}

/// Switching to an engine tree re-points the build; switching back is a cache
/// hit, so a branch switch does not cost an engine build.
#[test]
fn switching_engines_reinstalls_and_switching_back_restores() {
    let root = script_project("switch");
    let sdist = plant_sdist(&root, "SDIST");
    run_script_ok(&root, &["--sdist", sdist.to_str().expect("utf-8")], &[]);
    assert!(linked_engine(&root).starts_with("SDIST "));

    plant_git_engine_tree(&root, "TREE", Some("v2.0.0"));
    run_script_ok(&root, &[], &[]);
    assert!(
        linked_engine(&root).starts_with("TREE "),
        "an engine tree appearing later must replace the pinned build"
    );

    std::fs::remove_dir_all(root.join("duckdb")).expect("remove engine tree");
    let missing = root.join("no-such-sdist.tar.gz");
    run_script_ok(&root, &["--sdist", missing.to_str().expect("utf-8")], &[]);
    assert!(linked_engine(&root).starts_with("SDIST "));
    assert_eq!(
        cmake_calls(&root).len(),
        4,
        "switching back must not rebuild"
    );
}

/// A repeat build must not rewrite the manifest: `build.rs` reruns when it
/// changes, so a needless rewrite costs a relink of the extension.
#[test]
fn a_current_manifest_is_left_untouched() {
    let root = script_project("noop");
    let sdist = plant_sdist(&root, "SDIST");
    run_script_ok(&root, &["--sdist", sdist.to_str().expect("utf-8")], &[]);
    let path = root.join("cpp/engine.manifest");
    let mtime = || {
        std::fs::metadata(&path)
            .expect("stat")
            .modified()
            .expect("mtime")
    };
    let before = mtime();

    let out = run_script_ok(&root, &[], &[]);

    assert_eq!(
        before,
        mtime(),
        "the installed manifest must not be touched"
    );
    assert!(
        out.contains("already current"),
        "expected a no-op, got: {out}"
    );
    assert_eq!(cmake_calls(&root).len(), 2);
}

/// The archive must be the release the wheel pin names: its engine is built
/// under our label, and nothing in the engine source could contradict it.
#[test]
fn an_sdist_of_another_release_is_refused() {
    let root = script_project("sdist-wrong-release");
    let sdist = plant_sdist_of(&root, "OTHER", "2.0.0.dev1");

    let out = run_script(&root, &["--sdist", sdist.to_str().expect("utf-8")], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "another release's sdist must fail");
    assert!(
        stderr.contains("2.0.0.dev1") && stderr.contains(&target_pypi_version()),
        "the error must name both releases, got: {stderr}"
    );
    assert!(cmake_calls(&root).is_empty(), "nothing may be built");
    assert!(!root.join("cpp/engine.manifest").exists());
}

/// Correcting the wheel pin must build that wheel's engine, not keep serving
/// the one built from the old pin under the same engine version.
#[test]
fn a_wheel_pin_change_is_a_new_engine() {
    let root = script_project("wheel-pin");
    let first = plant_sdist_of(&root, "FIRST", "2.0.0.dev1");
    write(&root.join(".duckdb-pypi-version"), "2.0.0.dev1\n");
    run_script_ok(&root, &["--sdist", first.to_str().expect("utf-8")], &[]);

    let second = plant_sdist_of(&root, "SECOND", "2.0.0.dev2");
    write(&root.join(".duckdb-pypi-version"), "2.0.0.dev2\n");
    run_script_ok(&root, &["--sdist", second.to_str().expect("utf-8")], &[]);

    assert_eq!(
        cmake_calls(&root).len(),
        4,
        "the new wheel pin must rebuild"
    );
    assert!(linked_engine(&root).starts_with("SECOND "));
}

/// extension-ci-tools cross-compiles osx_amd64 on an arm64 runner and says so
/// in `OSX_BUILD_ARCH`. DuckDB's CMake reads that only as a CMake variable, so
/// it must be passed through, and the cross build cached apart from the
/// native one.
#[test]
fn a_cross_compile_architecture_reaches_cmake_and_the_cache_id() {
    let root = script_project("osx-arch");
    let sdist = plant_sdist(&root, "SDIST");
    let sdist = sdist.to_str().expect("utf-8");

    run_script_ok(&root, &["--sdist", sdist], &[("OSX_BUILD_ARCH", "x86_64")]);
    run_script_ok(&root, &["--sdist", sdist], &[]);

    let args = configure_args(&root);
    assert_eq!(
        args.len(),
        2,
        "the native build must not reuse the cross build"
    );
    assert!(
        args[0].split(' ').any(|a| a == "-DOSX_BUILD_ARCH=x86_64"),
        "the cross-compile arch must reach CMake: {args:?}"
    );
    assert!(
        !args[1].contains("OSX_BUILD_ARCH"),
        "a native build must not be told to cross-compile: {args:?}"
    );
}

/// `--force` (`just update-engine`) exists to replace an engine nobody trusts
/// any more; the manifest `build.rs` watches must change with it, or cargo
/// keeps the previous link.
#[test]
fn a_forced_rebuild_changes_the_manifest() {
    let root = script_project("force");
    let sdist = plant_sdist(&root, "SDIST");
    let sdist = sdist.to_str().expect("utf-8");
    run_script_ok(&root, &["--sdist", sdist], &[]);
    let before = read(&root.join("cpp/engine.manifest"));

    let out = run_script_ok(&root, &["--sdist", sdist, "--force"], &[]);

    assert_eq!(cmake_calls(&root).len(), 4, "--force must rebuild");
    assert_ne!(
        before,
        read(&root.join("cpp/engine.manifest")),
        "a rebuilt engine must change the manifest"
    );
    assert!(
        out.contains("installed"),
        "expected a reinstall, got: {out}"
    );
}

/// The cache entry `.engine/<id>/` of the pinned release, in `root`.
fn sdist_cache(root: &Path) -> PathBuf {
    root.join(".engine").join(sdist_engine_id())
}

/// An interrupted extraction (a Ctrl-C or a full disk while the source is
/// copied into place) leaves a partial source tree. A run that finds no cached
/// build must not take that tree for the engine source.
#[test]
fn a_source_tree_left_by_an_unfinished_run_is_extracted_again() {
    let root = script_project("partial-src");
    let sdist = plant_sdist(&root, "SDIST");
    let sdist = sdist.to_str().expect("utf-8");
    // A run that extracts the source and then stops short of caching a build.
    let out = run_script(&root, &["--sdist", sdist], &[("STUB_LABEL", "v1.4.0")]);
    assert!(!out.status.success());
    // What an interruption leaves: some of the tree, not all of it.
    write(&sdist_cache(&root).join("src/MARKER"), "PARTIAL\n");

    run_script_ok(&root, &["--sdist", sdist], &[]);

    assert_eq!(linked_engine(&root), format!("SDIST {}", target_version()));
}

/// A failed configure or build leaves its build directory behind, CMake cache
/// included. CMake refuses to configure over a cache made with another
/// generator (installing ninja between runs is enough), so the next run must
/// start from an empty build directory, `--force` or not.
fn check_a_stale_build_directory_is_not_reused(name: &str, force: bool) {
    let root = script_project(name);
    let sdist = plant_sdist(&root, "SDIST");
    let sdist = sdist.to_str().expect("utf-8");
    let out = run_script(&root, &["--sdist", sdist], &[("STUB_LABEL", "v1.4.0")]);
    assert!(!out.status.success());
    write(
        &sdist_cache(&root).join("build/CMakeCache.txt"),
        "another generator",
    );

    let args: &[&str] = if force {
        &["--sdist", sdist, "--force"]
    } else {
        &["--sdist", sdist]
    };
    run_script_ok(&root, args, &[]);

    assert_eq!(manifest(&root).0, target_version());
}

#[test]
fn a_build_directory_left_by_an_unfinished_run_is_not_reused() {
    check_a_stale_build_directory_is_not_reused("stale-build", false);
}

#[test]
fn a_forced_rebuild_does_not_reuse_a_stale_build_directory() {
    check_a_stale_build_directory_is_not_reused("stale-build-force", true);
}

/// The cache entry's manifest is what marks a build as finished. One cut short
/// (an interrupted write, a full disk) must be rebuilt, not handed to
/// `build.rs` with half its libraries, or crash the script. `cut` keeps the
/// first `cut(full)` bytes of a complete manifest.
fn check_a_truncated_cache_manifest_is_rebuilt(name: &str, cut: fn(&str) -> usize) {
    let root = script_project(name);
    let sdist = plant_sdist(&root, "SDIST");
    let sdist = sdist.to_str().expect("utf-8");
    run_script_ok(&root, &["--sdist", sdist], &[]);
    let stamp = sdist_cache(&root).join("manifest");
    let full = read(&stamp);
    write(&stamp, &full[..cut(&full)]);

    run_script_ok(&root, &["--sdist", sdist], &[]);

    assert_eq!(
        cmake_calls(&root).len(),
        4,
        "a truncated manifest must rebuild"
    );
    let (version, _, libs) = manifest(&root);
    assert_eq!(version, target_version());
    assert!(
        libs.iter().any(|l| l.ends_with("libduckdb_static.a")),
        "the rebuilt manifest must list the libraries: {libs:?}"
    );
}

#[test]
fn an_empty_cache_manifest_is_rebuilt_not_trusted() {
    check_a_truncated_cache_manifest_is_rebuilt("empty-stamp", |_| 0);
}

#[test]
fn a_cache_manifest_cut_before_its_libraries_is_rebuilt_not_trusted() {
    check_a_truncated_cache_manifest_is_rebuilt("cut-stamp", |full| {
        full.find("\nlib ").expect("a lib line")
    });
}

/// A checkout that built before the engine build has the v1.5 amalgamation in
/// `cpp/include`, where the shim's `#include "duckdb.hpp"` finds it before the
/// engine's own header. The engine build must remove it.
#[test]
fn the_amalgamation_left_by_a_pre_engine_build_is_removed() {
    let root = script_project("stale-amalgamation");
    let sdist = plant_sdist(&root, "SDIST");
    let stale = [
        "cpp/include/duckdb.hpp",
        "cpp/include/duckdb.cpp",
        "cpp/include/.amalgamation_id",
    ];
    for path in stale {
        write(&root.join(path), "// DuckDB v1.5.6 amalgamation\n");
    }
    write(
        &root.join("cpp/include/parser_extension_compat.hpp"),
        "// ours\n",
    );

    let out = run_script_ok(&root, &["--sdist", sdist.to_str().expect("utf-8")], &[]);

    for path in stale {
        assert!(!root.join(path).exists(), "{path} must be removed");
    }
    assert!(
        root.join("cpp/include/parser_extension_compat.hpp")
            .exists(),
        "the shim's own headers must stay"
    );
    assert!(
        out.contains("amalgamation"),
        "the removal must be logged: {out}"
    );
}

/// The paths a manifest names, all of which a usable engine has.
fn manifest_paths_exist(root: &Path) -> bool {
    let (_, includes, libs) = manifest(root);
    includes.iter().chain(&libs).all(|p| Path::new(p).exists())
}

/// A forced rebuild deletes the engine's source before rebuilding it. If it
/// then fails (here the sdist is missing, as when PyPI is unreachable), the
/// old entry must not be served by the next run: its headers are gone.
#[test]
fn a_failed_forced_rebuild_is_not_served_from_the_cache() {
    let root = script_project("failed-force");
    let sdist = plant_sdist(&root, "SDIST");
    let sdist = sdist.to_str().expect("utf-8");
    run_script_ok(&root, &["--sdist", sdist], &[]);
    let missing = root.join("no-such-sdist.tar.gz");
    let out = run_script(
        &root,
        &["--sdist", missing.to_str().expect("utf-8"), "--force"],
        &[],
    );
    assert!(
        !out.status.success(),
        "a rebuild without its sdist must fail"
    );

    run_script_ok(&root, &["--sdist", sdist], &[]);

    assert!(
        manifest_paths_exist(&root),
        "the installed manifest names paths that are gone: {:?}",
        manifest(&root)
    );
}

/// The cache entry's libraries can disappear without the run that made them
/// (pruned by hand, or the checkout moved): an entry whose files are gone is
/// rebuilt, not installed.
#[test]
fn a_cache_entry_whose_libraries_are_gone_is_rebuilt() {
    let root = script_project("pruned-lib");
    let sdist = plant_sdist(&root, "SDIST");
    let sdist = sdist.to_str().expect("utf-8");
    run_script_ok(&root, &["--sdist", sdist], &[]);
    std::fs::remove_dir_all(sdist_cache(&root).join("lib")).expect("remove lib");

    run_script_ok(&root, &["--sdist", sdist], &[]);

    assert_eq!(cmake_calls(&root).len(), 4, "the pruned entry must rebuild");
    assert!(manifest_paths_exist(&root));
}

/// Edits to a tree that are not committed yet (debugging a fork's engine, say)
/// leave its commit unchanged; they must still reach the build rather than be
/// served the build of the commit.
#[test]
fn uncommitted_edits_to_a_tree_are_built() {
    let root = script_project("tree-dirty");
    let tree = plant_git_engine_tree(&root, "TREE", Some("v2.0.0"));
    run_script_ok(&root, &[], &[]);

    write(&tree.join("MARKER"), "TREE-EDITED\n");
    run_script_ok(&root, &[], &[]);

    assert!(
        linked_engine(&root).starts_with("TREE-EDITED "),
        "the edit must be built, got {}",
        linked_engine(&root)
    );

    // And the edited build is not then served for the commit itself.
    git(&tree, &["checkout", "--", "MARKER"]);
    run_script_ok(&root, &[], &[]);
    assert!(linked_engine(&root).starts_with("TREE "));
}
