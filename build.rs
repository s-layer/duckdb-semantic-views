// build.rs
// Cargo build script — compiles C++ shim and restricts exported symbols when building the
// loadable extension.
//
// Design: Only the `extension` feature triggers C++ compilation and symbol visibility
// configuration. During `cargo test` (default/bundled feature), this script exits immediately.
//
// C++ compilation: The cc crate compiles shim.cpp against the vendored DuckDB amalgamation
// header (cpp/include/duckdb.hpp). This produces a static library that gets linked into the
// cdylib extension binary.
//
// Symbol visibility: restricts the exported symbols of the cdylib to the C_STRUCT entry
// point (semantic_views_init_c_api) on Linux and macOS. Without this, Rust stdlib symbols
// leak into the extension binary.
// Note: semantic_views_version is appended by the CI post-build script after compilation;
// it is NOT compiled into the binary and must not appear in the symbol list.
//
// Windows Win32 macro patching: duckdb.cpp includes <windows.h> mid-file, which defines
// macros that conflict with DuckDB C++ identifiers (GetObject -> GetObjectA, interface ->
// struct). WIN32_LEAN_AND_MEAN/NOGDI do not reliably suppress these across all Windows SDK
// versions. On Windows, build.rs generates a patched copy of duckdb.cpp in OUT_DIR with
// explicit #undef blocks inserted after each <windows.h> include.
//
// LSP support: `compile_commands.json` is regenerated on every `cargo build` / `cargo
// check`, sourcing flags from the same `CppBuildSpec` as the cc-crate invocation so
// clangd sees exactly what the build sees. clangd is pointed at the file via the
// checked-in `.clangd` at the repo root (`CompilationDatabase: target`).
//
// Output location: always `<CARGO_MANIFEST_DIR>/target/compile_commands.json` so the
// hardcoded `.clangd` path stays valid. When `CARGO_TARGET_DIR` is set to something
// other than `<CARGO_MANIFEST_DIR>/target`, a second copy is also written there to
// keep the build-system invariant (everything generated lives under the cargo target
// dir). Both `target/` directories are gitignored, so neither copy is committed.
//
// Windows caveat: the cc-crate compile substitutes a patched copy of duckdb.cpp from
// `OUT_DIR` (see `patch_duckdb_cpp_for_windows` below), but `compile_commands.json`
// still names `cpp/include/duckdb.cpp`. Clangd on Windows therefore parses the
// unpatched amalgamation and may surface `GetObject` / `interface` macro-conflict
// diagnostics that the actual build does not see. The diagnostics are cosmetic; the
// produced extension binary is still built from the patched source.

/// Single source of truth for C++ build flags. Both the cc-crate compile invocation and
/// the `compile_commands.json` writer read from this so the LSP cannot drift from the build.
struct CppBuildSpec {
    std: &'static str,
    include: &'static str,
    files: Vec<&'static str>,
    /// (name, value) — value is None for bare `-Dname` defines.
    defines: Vec<(&'static str, Option<&'static str>)>,
}

fn cpp_build_spec(is_windows: bool) -> CppBuildSpec {
    let mut spec = CppBuildSpec {
        std: "c++17",
        include: "cpp/include",
        files: vec!["cpp/include/duckdb.cpp", "cpp/src/shim.cpp"],
        defines: Vec::new(),
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

    let mut flags: Vec<String> = vec![
        "clang++".to_string(),
        format!("-std={}", spec.std),
        format!("-I{}", spec.include),
    ];
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

fn main() {
    // Always emit compile_commands.json so clangd works without `--features extension`.
    // Single source of truth: the same CppBuildSpec drives the cc-crate compile below.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let is_windows = target_os == "windows";
    write_compile_commands_json(&cpp_build_spec(is_windows));

    // Always rerun if the C++ surface or this script changes — keeps both the cc-crate
    // build cache and compile_commands.json fresh. Once any rerun-if-changed is emitted,
    // cargo treats the list as exhaustive, so include every relevant input.
    //
    // duckdb.hpp / duckdb.cpp are gitignored (downloaded by `just update-headers`).
    // Cargo treats a `rerun-if-changed` whose path is missing as "always rerun", which
    // would force every `cargo check` on a fresh checkout to re-execute this script.
    // Gate them on existence so they only join the dependency graph once present.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cpp/src/shim.cpp");
    println!("cargo:rerun-if-changed=cpp/src/shim.hpp");
    println!("cargo:rerun-if-changed=cpp/include/parser_extension_compat.hpp");
    if std::path::Path::new("cpp/include/duckdb.hpp").exists() {
        println!("cargo:rerun-if-changed=cpp/include/duckdb.hpp");
    }
    if std::path::Path::new("cpp/include/duckdb.cpp").exists() {
        println!("cargo:rerun-if-changed=cpp/include/duckdb.cpp");
    }
    // Toggling SV_SKIP_CPP_BUILD flips whether the C++ shim/amalgamation is
    // compiled below, so it must invalidate the build-script cache. Without
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
    // type-check the extension-gated Rust WITHOUT compiling the ~25 MB DuckDB
    // amalgamation + C++ shim (a ~10-min cc build). Analysis-only cargo modes
    // never link the final cdylib, so the missing DuckDB symbols and link
    // directives don't matter — this is purely to make linting the FFI layer
    // fast (locally and in CI) and does NOT produce a loadable extension.
    // A real `cargo build --features extension` must leave this unset.
    if std::env::var("SV_SKIP_CPP_BUILD").is_ok() {
        println!(
            "cargo:warning=SV_SKIP_CPP_BUILD set: skipping DuckDB amalgamation + C++ shim compile \
             (analysis-only; the resulting artifact will NOT link/load as an extension)"
        );
        return;
    }

    // Compile the DuckDB amalgamation source + C++ shim.
    // duckdb.cpp provides all DuckDB C++ symbol definitions (constructors, RTTI,
    // vtables) so the shim can use ParserExtension, TableFunction, etc. without
    // relying on symbol export from the host process (Python DuckDB compiles with
    // -fvisibility=hidden). Symbol visibility in the cdylib is restricted below
    // so these definitions stay internal to the extension binary.
    //
    // First build: ~2-5 min (duckdb.cpp is ~300K lines). Cached by cc crate after.
    //
    // The cc crate is an optional build-dependency gated on the `extension` feature.
    #[cfg(feature = "extension")]
    {
        // On Windows, generate a patched copy of duckdb.cpp in OUT_DIR.
        // duckdb.cpp is gitignored and re-downloaded from DuckDB releases in CI,
        // so patches must be applied at build time, not committed.
        //
        // The patch inserts explicit #undef blocks for Win32 macros that conflict
        // with DuckDB C++ identifiers, following the same pattern DuckDB already
        // uses for CreateDirectory/MoveFile/RemoveDirectory in the same file.
        let duckdb_cpp_path = if is_windows {
            patch_duckdb_cpp_for_windows()
        } else {
            "cpp/include/duckdb.cpp".to_string()
        };

        let spec = cpp_build_spec(is_windows);
        let mut build = cc::Build::new();
        build
            .cpp(true)
            .std(spec.std)
            .include(spec.include)
            .warnings(false); // Suppress warnings from DuckDB amalgamation

        // cl (and clang-cl) compile C++ without exception unwinding unless told,
        // so a DuckDB exception thrown through a frame would skip its destructors;
        // DuckDB's own CMake build gets /EHsc from CMake's defaults. shim.cpp
        // refuses to compile without it.
        if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc") {
            build.flag("/EHsc");
        }

        // Substitute the patched duckdb.cpp path on Windows; other files come from spec.
        for file in &spec.files {
            if *file == "cpp/include/duckdb.cpp" {
                build.file(&duckdb_cpp_path);
            } else {
                build.file(file);
            }
        }
        for (name, val) in &spec.defines {
            build.define(name, *val);
        }

        if is_windows {
            // MSVC preprocessor defines applied before any source is compiled.
            // WIN32_LEAN_AND_MEAN — reduces Windows headers included by windows.h
            // NOMINMAX — prevents min/max macros that break std::numeric_limits<T>::max()
            // NOGDI — defense-in-depth: asks wingdi.h to skip GetObject macro definition
            // DUCKDB_STATIC_BUILD — prevents DUCKDB_API expanding to __declspec(dllimport)
            //
            // Note: GetObject and `interface` are also explicitly #undef-d in the patched
            // duckdb.cpp (see patch_duckdb_cpp_for_windows below), because NOGDI and
            // WIN32_LEAN_AND_MEAN are not reliable across all Windows SDK configurations.
            //
            // /bigobj: duckdb.cpp (~300K lines) exceeds MSVC's default 65,536-section COFF limit.
            // flag_if_supported is a no-op on non-MSVC toolchains.
            build.flag_if_supported("/bigobj");
        }

        build.compile("semantic_views_shim");
    }

    // Symbol visibility: restrict the cdylib's exported symbols to the DuckDB CPP
    // entry point only. Without this, Rust stdlib symbols leak into the extension binary.
    //
    // Exports: semantic_views_init_c_api (Rust entry point, C_STRUCT ABI)
    //
    // Windows: __declspec(dllexport) on the #[no_mangle] entry point handles visibility.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
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

/// Generate a patched copy of `duckdb.cpp` in `OUT_DIR` for Windows builds.
///
/// `duckdb.cpp` includes `<windows.h>` mid-file (after all `DuckDB` declarations are
/// already processed from `duckdb.hpp`). The `windows.h` include can define macros that
/// conflict with identifiers in the `DuckDB` C++ implementation code that follows:
///
/// - `GetObject` → `GetObjectA` (`wingdi.h`): conflicts with `ComplexJSON::GetObject`
///   (line ~36327) and `ObjectCache::GetObject` (line ~37656).
/// - `interface` → `struct` (`objbase.h`): conflicts with `MultiFileReader` `interface`
///   variable names (line ~65873+).
///
/// A third, unrelated MSVC conflict is also handled here (Patch 3): the bundled `fmt` 6.1.2
/// references `stdext::checked_array_iterator`, which recent MSVC STL versions removed. That
/// patch flips the `#ifdef _SECURE_SCL` guard to take `fmt`'s portable raw-pointer path.
///
/// The fix adds explicit `#undef` blocks after each `<windows.h>` include, following
/// the same pattern `DuckDB` already uses for `CreateDirectory`/`MoveFile`/`RemoveDirectory`
/// in the same file. We patch at build time (not at commit time) because `duckdb.cpp`
/// is gitignored and re-downloaded from `DuckDB` releases in CI.
///
/// Returns the path to the patched file (or the original if already patched).
#[cfg(feature = "extension")]
fn patch_duckdb_cpp_for_windows() -> String {
    let orig = "cpp/include/duckdb.cpp";
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let patched = format!("{out_dir}/duckdb_patched.cpp");

    // The markers below are written with `\n`. The release zip's duckdb.cpp has LF
    // line endings, but one generated on Windows from an engine tree (the
    // `generate_from_source` path in scripts/ensure_amalgamation.py, which the
    // community-extensions build takes) has CRLF, because the engine's
    // scripts/amalgamation.py writes in text mode. Normalise so the markers match
    // either way; MSVC compiles LF sources as is.
    let content = std::fs::read_to_string(orig)
        .expect(
            "failed to read cpp/include/duckdb.cpp — \
             run 'make ensure_amalgamation' or 'just update-headers' to download it",
        )
        .replace("\r\n", "\n");

    // Idempotent: if already patched (e.g. local dev copy with manual edits), use as-is.
    // We check for one of our undef markers to detect prior patching.
    if content.contains("undef GetObject") {
        std::fs::write(&patched, &content).expect("failed to write duckdb_patched.cpp to OUT_DIR");
        return patched;
    }

    // --- Patch 1: after the first <windows.h> include (DLL loading helpers block) ---
    //
    // DuckDB <= 1.4.x had two separate <windows.h> includes. The first was in a
    // DLL loading helpers block, marked by "// Platform-specific helpers". DuckDB 1.5.0
    // consolidated to a single <windows.h> include (handled by Patch 2 below), so
    // this patch is expected to be a no-op for DuckDB >= 1.5.0.
    //
    // Original context (DuckDB 1.4.x duckdb.cpp ~line 25363-25372):
    //   #endif // defined(_WIN32)
    //
    //   // Platform-specific helpers        <-- patch inserted before this
    let win32_undef_block = "\
        // Undefine Windows macros that conflict with DuckDB C++ identifiers.\n\
        // GetObject (wingdi.h) -> clashes with ComplexJSON::GetObject, ObjectCache::GetObject\n\
        // interface (objbase.h) -> clashes with MultiFileReader 'interface' variable names\n\
        #if defined(_WIN32)\n\
        #  ifdef GetObject\n\
        #    undef GetObject\n\
        #  endif\n\
        #  ifdef interface\n\
        #    undef interface\n\
        #  endif\n\
        #endif\n\n";

    let patch1_before = "#endif // defined(_WIN32)\n\n// Platform-specific helpers";
    let patch1_after =
        format!("#endif // defined(_WIN32)\n\n{win32_undef_block}// Platform-specific helpers");

    let content = if content.contains(patch1_before) {
        content.replace(patch1_before, &patch1_after)
    } else {
        // Expected for DuckDB >= 1.5.0 where the first windows.h include was removed.
        content
    };

    // --- Patch 2: after the second <windows.h> include (file system helpers block) ---
    //
    // Original context (duckdb.cpp ~line 38078-38094):
    //   #if defined(_WIN32)
    //   #ifndef NOMINMAX ... #endif
    //   #ifndef _WINSOCKAPI_ ... #endif
    //   #include <windows.h>
    //   #undef CreateDirectory    <-- DuckDB already undefs some macros here
    //   #undef MoveFile
    //   #undef RemoveDirectory
    //                             <-- we append our undefs after RemoveDirectory
    //   #endif
    //   #else
    //   #include <sys/mman.h>
    //
    // A second windows.h include can re-define interface (used as C++ identifier name
    // starting at ~line 65873). GetObject undef is defensive here.
    let patch2_before = "#undef CreateDirectory\n#undef MoveFile\n#undef RemoveDirectory\n\n#endif\n\n#else\n#include <sys/mman.h>";
    let patch2_after = "#undef CreateDirectory\n#undef MoveFile\n#undef RemoveDirectory\n\
        #ifdef GetObject\n\
        #  undef GetObject\n\
        #endif\n\
        #ifdef interface\n\
        #  undef interface\n\
        #endif\n\n\
        #endif\n\n#else\n#include <sys/mman.h>";

    // AR-9: a patch-2 miss is a HARD error, not a `cargo:warning`. This
    // function only runs on Windows builds (build.rs `is_windows` guard), and
    // the bundled amalgamation is version-pinned, so the marker is present for
    // every version we ship against. If it moves (a DuckDB bump reshaping the
    // <windows.h> block) the #undef is silently dropped and the second
    // windows.h include re-defines the C++ identifier `interface` — the MSVC
    // build then fails much later with opaque C2xxxx errors far from the cause.
    // Fail the build here, at the patch site, with an actionable message.
    let content = if content.contains(patch2_before) {
        content.replace(patch2_before, patch2_after)
    } else {
        panic!(
            "duckdb.cpp Win32 patch 2 FAILED: expected marker not found. The bundled \
             DuckDB amalgamation changed shape (most likely a version bump), so this \
             Windows-only #undef patch no longer applies. Left unpatched, the MSVC build \
             fails later with opaque errors when the second <windows.h> include re-defines \
             the C++ identifier `interface`. Update `patch2_before` in \
             build.rs::patch_duckdb_cpp_for_windows to match the new amalgamation."
        );
    };

    // --- Patch 3: neutralize fmt's removed-MSVC-API reference ---
    //
    // The bundled fmt 6.1.2 (FMT_VERSION 60102, ~line 16639) selects a checked iterator on
    // MSVC via:
    //
    //   #ifdef _SECURE_SCL
    //   template <typename T> using checked_ptr = stdext::checked_array_iterator<T*>;
    //   ...
    //   #else
    //   template <typename T> using checked_ptr = T*;   // portable path
    //   #endif
    //
    // `_SECURE_SCL` is defined by the MSVC STL headers whenever they are included, so MSVC
    // always takes the first branch. Recent MSVC STL versions REMOVED stdext::checked_array_iterator
    // entirely (the stdext namespace no longer exists), so that branch fails to compile with
    //   error C2653: 'stdext': is not a class or namespace name
    // This is a hard removal, not a deprecation, so no _SILENCE_* macro restores it, and the
    // `#ifdef` guard (defined-ness, not value) means -D_SECURE_SCL=0 would still take it.
    //
    // We flip the guard to `#if 0` so the compiler takes fmt's portable `checked_ptr = T*`
    // path — exactly what every non-Windows platform already compiles, and what release builds
    // want (checked iterators are a debug-only aid). The stdext reference becomes dead code.
    let patch3_before = "#ifdef _SECURE_SCL\n// Make a checked iterator to avoid MSVC warnings.\ntemplate <typename T> using checked_ptr = stdext::checked_array_iterator<T*>;";
    let patch3_after = "#if 0 // semantic-views patch: stdext::checked_array_iterator removed from modern MSVC STL — force fmt's portable raw-pointer path\n// Make a checked iterator to avoid MSVC warnings.\ntemplate <typename T> using checked_ptr = stdext::checked_array_iterator<T*>;";

    // AR-9: a patch-3 miss is a HARD error ONLY when the dangerous reference is
    // still present but has drifted out of `patch3_before`'s exact shape — that
    // is the case #60 cared about (a silently-unapplied patch shipping a broken
    // MSVC build). But DuckDB's bundled fmt no longer emits the
    // `stdext::checked_array_iterator` block at all (it is absent as of the
    // v1.5.4 amalgamation — `grep` finds zero occurrences of `stdext`), so the
    // original unconditional hard-fail was a false positive that has kept every
    // Windows build red since the marker disappeared. Distinguish the two:
    //   - `patch3_before` present            -> apply the guard flip;
    //   - `stdext::checked_array_iterator` present but not in that shape
    //                                        -> the patch drifted: fail loudly;
    //   - neither present                    -> nothing to neutralize, MSVC
    //                                           already compiles fmt's portable
    //                                           `checked_ptr = T*` path: skip.
    let content = if content.contains(patch3_before) {
        content.replace(patch3_before, patch3_after)
    } else if content.contains("stdext::checked_array_iterator") {
        panic!(
            "duckdb.cpp Win32 patch 3 FAILED: `stdext::checked_array_iterator` is present but \
             not in the exact form `patch3_before` matches, so the guard flip was silently \
             dropped. Left unpatched, MSVC selects fmt's removed \
             `stdext::checked_array_iterator` and fails with `error C2653: 'stdext' is not a \
             class or namespace name`. Update `patch3_before` in \
             build.rs::patch_duckdb_cpp_for_windows to match the new amalgamation."
        );
    } else {
        // fmt's MSVC checked-iterator block was removed upstream; there is no
        // `stdext` reference for MSVC to reject. Nothing to patch.
        content
    };

    std::fs::write(&patched, content).expect("failed to write duckdb_patched.cpp to OUT_DIR");

    patched
}
