// build.rs
// Cargo build script — compiles the C++ shim, links the DuckDB engine it is written
// against, and restricts exported symbols when building the loadable extension.
//
// Design: Only the `extension` feature triggers C++ compilation and symbol visibility
// configuration. During `cargo test` (default/bundled feature), this script exits immediately.
//
// C++ compilation: the cc crate compiles shim.cpp against the DuckDB engine's own headers
// (`src/include` plus the third-party include directories), and the engine's static
// libraries are linked into the cdylib. Both come from `cpp/engine.manifest`, which
// `scripts/ensure_engine.py` (`make ensure_engine`) writes after building the engine. The
// shim needs DuckDB's C++ internals (the parser-extension hook has no C-API equivalent),
// and a host only exports the C API, so the extension carries its own copy of the engine
// code it calls; `src/load_guard.rs` refuses any host that is not the exact release that
// copy was built from. DuckDB 2.0 ships no amalgamation, which is what the 1.x builds
// compiled here instead.
//
// Symbol visibility: restricts the exported symbols of the cdylib to the C_STRUCT entry
// point (semantic_views_init_c_api) on Linux and macOS. Without this, Rust stdlib symbols
// and the engine's C++ symbols leak into the extension binary.
// Note: semantic_views_version is appended by the CI post-build script after compilation;
// it is NOT compiled into the binary and must not appear in the symbol list.
//
// LSP support: `compile_commands.json` is regenerated on every `cargo build` / `cargo
// check`, sourcing flags from the same `CppBuildSpec` as the cc-crate invocation so
// clangd sees exactly what the build sees. clangd is pointed at the file via the
// checked-in `.clangd` at the repo root (`CompilationDatabase: target`). Until
// `make ensure_engine` has run there is no manifest, so the entry names only `cpp/include`.
//
// Output location: always `<CARGO_MANIFEST_DIR>/target/compile_commands.json` so the
// hardcoded `.clangd` path stays valid. When `CARGO_TARGET_DIR` is set to something
// other than `<CARGO_MANIFEST_DIR>/target`, a second copy is also written there to
// keep the build-system invariant (everything generated lives under the cargo target
// dir). Both `target/` directories are gitignored, so neither copy is committed.

/// Where `scripts/ensure_engine.py` records the engine build to use.
const ENGINE_MANIFEST: &str = "cpp/engine.manifest";

/// The engine build `cpp/engine.manifest` names: its header directories and its
/// static libraries, `duckdb_static` first.
struct EngineManifest {
    includes: Vec<String>,
    libs: Vec<String>,
}

fn read_engine_manifest() -> Option<EngineManifest> {
    let text = std::fs::read_to_string(ENGINE_MANIFEST).ok()?;
    let mut manifest = EngineManifest {
        includes: Vec::new(),
        libs: Vec::new(),
    };
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("include ") {
            manifest.includes.push(path.to_string());
        } else if let Some(path) = line.strip_prefix("lib ") {
            manifest.libs.push(path.to_string());
        }
    }
    Some(manifest)
}

/// Single source of truth for C++ build flags. Both the cc-crate compile invocation and
/// the `compile_commands.json` writer read from this so the LSP cannot drift from the build.
struct CppBuildSpec {
    std: &'static str,
    includes: Vec<String>,
    files: Vec<&'static str>,
    /// (name, value) — value is None for bare `-Dname` defines.
    defines: Vec<(&'static str, Option<&'static str>)>,
}

fn cpp_build_spec(is_windows: bool, engine: Option<&EngineManifest>) -> CppBuildSpec {
    let mut includes = vec!["cpp/include".to_string()];
    if let Some(engine) = engine {
        includes.extend(engine.includes.iter().cloned());
    }
    let mut spec = CppBuildSpec {
        std: "c++17",
        includes,
        files: vec!["cpp/src/shim.cpp"],
        defines: vec![("SV_DUCKDB_2", None)],
    };
    if is_windows {
        spec.defines.push(("WIN32_LEAN_AND_MEAN", None));
        spec.defines.push(("NOMINMAX", None));
        spec.defines.push(("NOGDI", None));
        spec.defines.push(("DUCKDB_STATIC_BUILD", None));
    }
    spec
}

/// Write a `compile_commands.json` reflecting `spec`.
///
/// Always writes to `<CARGO_MANIFEST_DIR>/target/compile_commands.json` so the
/// checked-in `.clangd` (which hardcodes `CompilationDatabase: target`) keeps
/// finding it. When `CARGO_TARGET_DIR` is set to a different path, a second copy
/// is also written under that directory to keep the build-system invariant that
/// generated files live under the cargo target dir. Both `target/` locations are
/// gitignored, so neither copy is committed.
///
/// Idempotent — only writes when the rendered JSON differs from the existing file, so
/// cargo's rerun-if-changed graph doesn't churn from build script self-output.
///
/// `directory` inside each entry stays as the workspace root so relative `file` and
/// `-I` paths resolve correctly.
fn write_compile_commands_json(spec: &CppBuildSpec) {
    let dir = std::env::current_dir().map_or_else(|_| ".".to_string(), |p| p.display().to_string());

    let mut flags: Vec<String> = vec!["clang++".to_string(), format!("-std={}", spec.std)];
    for include in &spec.includes {
        flags.push(format!("-I{include}"));
    }
    for (name, val) in &spec.defines {
        match val {
            Some(v) => flags.push(format!("-D{name}={v}")),
            None => flags.push(format!("-D{name}")),
        }
    }
    flags.push("-c".to_string());

    let mut entries: Vec<String> = Vec::with_capacity(spec.files.len());
    for file in &spec.files {
        let mut args = flags.clone();
        args.push((*file).to_string());
        let args_json = args
            .iter()
            .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ");
        entries.push(format!(
            "  {{\n    \"directory\": \"{}\",\n    \"file\": \"{}\",\n    \"arguments\": [{}]\n  }}",
            dir.replace('\\', "\\\\").replace('"', "\\\""),
            file.replace('\\', "\\\\").replace('"', "\\\""),
            args_json,
        ));
    }
    let new_json = format!("[\n{}\n]\n", entries.join(",\n"));

    // Resolve write targets. The `.clangd`-anchored copy under
    // <CARGO_MANIFEST_DIR>/target is the authoritative path clangd reads. When
    // CARGO_TARGET_DIR is set to a different location, also write a copy there
    // so generated artefacts stay under the configured target dir.
    let manifest_target = std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .map(|m| format!("{m}/target"));
    let cargo_target = std::env::var("CARGO_TARGET_DIR").ok();

    let mut targets: Vec<String> = Vec::new();
    if let Some(t) = manifest_target {
        targets.push(t);
    }
    if let Some(t) = cargo_target {
        if !targets.iter().any(|existing| existing == &t) {
            targets.push(t);
        }
    }
    if targets.is_empty() {
        println!(
            "cargo:warning=neither CARGO_MANIFEST_DIR nor CARGO_TARGET_DIR set; skipping compile_commands.json"
        );
        return;
    }

    for target_dir in &targets {
        if let Err(e) = std::fs::create_dir_all(target_dir) {
            println!("cargo:warning=failed to create {target_dir}: {e}");
            continue;
        }
        let path = format!("{target_dir}/compile_commands.json");
        if std::fs::read_to_string(&path)
            .ok()
            .as_deref()
            .is_some_and(|prev| prev == new_json)
        {
            continue;
        }
        if let Err(e) = std::fs::write(&path, &new_json) {
            println!("cargo:warning=failed to write {path}: {e}");
        }
    }
}

// DuckDB's CMake builds the engine against the static MSVC runtime (/MT), and
// .cargo/config.toml turns on `crt-static` to match. A RUSTFLAGS set in the
// environment replaces that config's rustflags instead of adding to them, and
// the link then mixes runtimes, so say so here rather than as an LNK2038 at the
// end of the build.
#[cfg(feature = "extension")]
fn check_msvc_static_runtime(is_windows: bool) {
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    assert!(
        !is_windows
            || target_env != "msvc"
            || target_features.split(',').any(|f| f == "crt-static"),
        "the DuckDB engine is built against the static MSVC runtime (/MT), but this \
         build does not link it statically (no `crt-static` target feature). A \
         RUSTFLAGS in the environment replaces the `+crt-static` that \
         .cargo/config.toml sets: add `-C target-feature=+crt-static` to it."
    );
}

fn main() {
    // Always emit compile_commands.json so clangd works without `--features extension`.
    // Single source of truth: the same CppBuildSpec drives the cc-crate compile below.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let is_windows = target_os == "windows";
    let engine = read_engine_manifest();
    write_compile_commands_json(&cpp_build_spec(is_windows, engine.as_ref()));

    // Always rerun if the C++ surface, the engine or this script changes — keeps both
    // the cc-crate build cache and compile_commands.json fresh. Once any
    // rerun-if-changed is emitted, cargo treats the list as exhaustive, so include
    // every relevant input. A missing path counts as "always rerun", so the
    // gitignored manifest joins the list only once it exists.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cpp/src/shim.cpp");
    println!("cargo:rerun-if-changed=cpp/src/shim.hpp");
    println!("cargo:rerun-if-changed=cpp/include/parser_extension_compat.hpp");
    if std::path::Path::new(ENGINE_MANIFEST).exists() {
        println!("cargo:rerun-if-changed={ENGINE_MANIFEST}");
    }
    // Cargo does not fingerprint `rustc-link-lib` inputs, so an engine rebuilt in
    // place would otherwise keep the previous link. Only the libraries that exist
    // are listed, for the same reason as the manifest.
    for lib in engine.iter().flat_map(|e| &e.libs) {
        if std::path::Path::new(lib).exists() {
            println!("cargo:rerun-if-changed={lib}");
        }
    }
    // Toggling SV_SKIP_CPP_BUILD flips whether the C++ shim is compiled and the
    // engine linked below, so it must invalidate the build-script cache. Without
    // this, a `cargo build --features extension` right after a
    // `SV_SKIP_CPP_BUILD=1 cargo clippy` could reuse the skipped output and
    // link against a never-built shim.
    println!("cargo:rerun-if-env-changed=SV_SKIP_CPP_BUILD");

    // Only configure C++ compilation and symbol visibility when building the loadable
    // extension binary. CARGO_FEATURE_EXTENSION is set by Cargo when `--features extension`
    // is passed. During `cargo test` (uses default/bundled feature), this block is skipped.
    if std::env::var("CARGO_FEATURE_EXTENSION").is_err() {
        return;
    }

    // SV_SKIP_CPP_BUILD lets `cargo clippy`/`cargo check --features extension`
    // type-check the extension-gated Rust WITHOUT building the DuckDB engine or
    // compiling the C++ shim. Analysis-only cargo modes never link the final
    // cdylib, so the missing DuckDB symbols and link directives don't matter —
    // this is purely to make linting the FFI layer fast (locally and in CI) and
    // does NOT produce a loadable extension. A real `cargo build --features
    // extension` must leave this unset.
    if std::env::var("SV_SKIP_CPP_BUILD").is_ok() {
        println!(
            "cargo:warning=SV_SKIP_CPP_BUILD set: skipping the C++ shim compile and engine link \
             (analysis-only; the resulting artifact will NOT link/load as an extension)"
        );
        return;
    }

    // Compile the C++ shim against the engine's headers and link the engine's
    // static libraries, which provide every DuckDB C++ symbol the shim uses
    // (constructors, RTTI, vtables) without relying on symbol export from the host
    // process (Python DuckDB compiles with -fvisibility=hidden). Symbol visibility
    // in the cdylib is restricted below so these definitions stay internal to the
    // extension binary.
    //
    // The cc crate is an optional build-dependency gated on the `extension` feature.
    #[cfg(feature = "extension")]
    {
        let Some(engine) = engine.as_ref() else {
            panic!(
                "{ENGINE_MANIFEST} is missing: the DuckDB engine the C++ shim links against \
                 has not been built. Run `make ensure_engine` (or `just build`), which \
                 builds it with scripts/ensure_engine.py."
            );
        };
        // `cpp/include` is on the include path ahead of the engine's headers, so
        // the v1.x amalgamation a pre-engine build left there would be compiled
        // in place of the engine's `duckdb.hpp`. `make ensure_engine` removes it.
        let stale = std::path::Path::new("cpp/include/duckdb.hpp");
        assert!(
            !stale.exists(),
            "{} is the DuckDB 1.x amalgamation a pre-engine build left behind, and it \
             would shadow the engine's own duckdb.hpp. Run `make ensure_engine` (or \
             `just build`), which removes it, or delete it.",
            stale.display()
        );
        check_msvc_static_runtime(is_windows);
        let spec = cpp_build_spec(is_windows, Some(engine));
        let mut build = cc::Build::new();
        build.cpp(true).std(spec.std).warnings(false); // DuckDB headers are not warning-clean

        // cl (and clang-cl) compile C++ without exception unwinding unless told,
        // so a DuckDB exception thrown through a shim frame would skip its
        // destructors; DuckDB's own CMake build gets /EHsc from CMake's
        // defaults. shim.cpp refuses to compile without it.
        if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc") {
            build.flag("/EHsc");
        }
        for include in &spec.includes {
            build.include(include);
        }
        for file in &spec.files {
            build.file(file);
        }
        for (name, val) in &spec.defines {
            build.define(name, *val);
        }
        build.compile("semantic_views_shim");

        // `duckdb_static` first: a single-pass linker resolves its references to
        // the third-party libraries listed after it.
        for lib in &engine.libs {
            let path = std::path::Path::new(lib);
            let dir = path.parent().expect("engine library has a directory");
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("engine library has a file name");
            let name = if path.extension().is_some_and(|e| e == "a") {
                stem.strip_prefix("lib").unwrap_or(stem)
            } else {
                stem
            };
            println!("cargo:rustc-link-search=native={}", dir.display());
            println!("cargo:rustc-link-lib=static={name}");
        }
    }

    // Symbol visibility: restrict the cdylib's exported symbols to the DuckDB CPP
    // entry point only. Without this, Rust stdlib symbols leak into the extension binary.
    //
    // Exports: semantic_views_init_c_api (Rust entry point, C_STRUCT ABI)
    //
    // Windows: __declspec(dllexport) on the #[no_mangle] entry point handles visibility.
    let out_dir = std::env::var("OUT_DIR").unwrap();

    match target_os.as_str() {
        "linux" => {
            // Linux symbol visibility: rustc already generates a --version-script for
            // cdylib targets. Adding a second version script causes GNU ld (especially
            // gcc-toolset-14 on arm64) to reject the link with "anonymous version tag
            // cannot be combined with other version tags". Instead, we use
            // --dynamic-list which cooperates with rustc's version script and restricts
            // visible symbols to our entry point.
            let dynlist_path = format!("{out_dir}/semantic_views.dynlist");
            std::fs::write(&dynlist_path, "{\n  semantic_views_init_c_api;\n};\n")
                .expect("failed to write dynamic list");
            println!("cargo:rustc-link-arg=-Wl,--dynamic-list={dynlist_path}");
        }
        "macos" => {
            // Exported symbols list: only the entry point is externally visible.
            // macOS uses underscore-prefixed names in the exported symbols file.
            // Note: semantic_views_version is appended by the CI post-build script
            // (extension-ci-tools); it does NOT exist in the compiled binary and must
            // not be listed here — the linker would fail with "undefined symbol".
            let exp_path = format!("{out_dir}/semantic_views.exp");
            std::fs::write(&exp_path, "_semantic_views_init_c_api\n")
                .expect("failed to write macOS exported symbols list");
            println!("cargo:rustc-link-arg=-Wl,-exported_symbols_list,{exp_path}");
        }
        "windows" => {
            // rstrtmgr: DuckDB v1.4.4 added duckdb::AdditionalLockInfo which calls
            // the Windows Restart Manager API (RmStartSession, RmEndSession,
            // RmRegisterResources, RmGetList). These are defined in rstrtmgr.lib,
            // which is not linked by default. Without this, the link fails with
            // LNK2019/LNK1120 unresolved external symbol errors.
            // rstrtmgr.lib ships with every Windows SDK so no installation is required.
            // Symbol visibility (__declspec(dllexport)) is handled by the #[no_mangle]
            // entry point — no extra linker flags needed for that.
            println!("cargo:rustc-link-lib=rstrtmgr");
        }
        _ => {
            // Other platforms need no extra link flags.
        }
    }
}
