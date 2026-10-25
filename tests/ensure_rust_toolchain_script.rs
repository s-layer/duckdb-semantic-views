//! Guards for `scripts/ensure_rust_toolchain.py`, which the Makefile runs before
//! `cargo build` so a registry build installs the pinned toolchain with retries
//! instead of letting rustup fetch it lazily, once.
//!
//! The registry's runners are the only place the install path runs for real,
//! so these drive the script against a fake `rustup` (a shell script that logs
//! its arguments and answers from a scenario) through `SV_RUSTUP`.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The pinned channel, read the way the script reads it.
fn pinned_channel() -> String {
    let text = std::fs::read_to_string(repo_root().join("rust-toolchain.toml")).unwrap();
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("channel"))
        .expect("channel line");
    line.split('"').nth(1).expect("quoted channel").to_string()
}

/// A fresh scratch directory, removed first so a crashed previous run cannot
/// leak state into this one.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("sv-ensure-toolchain-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Write a fake `rustup` into `dir`. It appends each call's arguments to
/// `dir/calls`, and runs `body` (sh) with them; `$DIR` is `dir`.
fn fake_rustup(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("rustup");
    let script = format!(
        "#!/bin/sh\nDIR='{}'\necho \"$*\" >> \"$DIR/calls\"\n{body}\n",
        dir.display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn run_script(rustup: &Path) -> Output {
    Command::new("python3")
        .arg(repo_root().join("scripts/ensure_rust_toolchain.py"))
        .env("SV_RUSTUP", rustup)
        .env("SV_TOOLCHAIN_RETRY_DELAY", "0")
        .output()
        .expect("run python3 (is it on PATH?)")
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn installs(dir: &Path) -> usize {
    calls(dir)
        .iter()
        .filter(|c| c.starts_with("toolchain install"))
        .count()
}

/// The scenario's answers to `toolchain list` and `component list`.
fn listing(toolchains: &str, components: &str) -> String {
    format!(
        "case \"$1 $2\" in\n\
         'toolchain list') printf '{toolchains}' ;;\n\
         'component list') printf '{components}' ;;\n\
         esac"
    )
}

#[test]
fn an_installed_toolchain_is_left_alone() {
    let dir = scratch("installed");
    let ch = pinned_channel();
    let rustup = fake_rustup(
        &dir,
        &format!(
            "{}\ncase \"$1 $2\" in 'toolchain install') exit 1 ;; esac",
            listing(
                &format!("stable-x86_64-unknown-linux-gnu (default)\\n{ch}-x86_64-unknown-linux-gnu (active)\\n"),
                "cargo-x86_64-unknown-linux-gnu\\nclippy-x86_64-unknown-linux-gnu\\nrustfmt-x86_64-unknown-linux-gnu\\n",
            )
        ),
    );
    let out = run_script(&rustup);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(installs(&dir), 0, "calls: {:?}", calls(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_component_installs_the_toolchain() {
    let dir = scratch("component");
    let ch = pinned_channel();
    let rustup = fake_rustup(
        &dir,
        &listing(
            &format!("{ch}-x86_64-unknown-linux-gnu\\n"),
            "cargo-x86_64-unknown-linux-gnu\\nrustfmt-x86_64-unknown-linux-gnu\\n",
        ),
    );
    let out = run_script(&rustup);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let install = calls(&dir)
        .into_iter()
        .find(|c| c.starts_with("toolchain install"))
        .expect("an install");
    assert!(
        install.contains(&format!("install {ch} "))
            && install.contains("--component rustfmt")
            && install.contains("--component clippy")
            && install.contains("--no-self-update"),
        "{install}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_install_is_retried_after_clearing_it() {
    let dir = scratch("retry");
    // Nothing installed; the first two installs fail, the third succeeds.
    let rustup = fake_rustup(
        &dir,
        "case \"$1 $2\" in\n\
         'toolchain install')\n\
           n=$(cat \"$DIR/n\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$DIR/n\"\n\
           if [ $n -le 2 ]; then echo 'error: dns error' >&2; exit 1; fi ;;\n\
         esac",
    );
    let out = run_script(&rustup);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(installs(&dir), 3, "calls: {:?}", calls(&dir));
    let log = calls(&dir);
    let uninstalls = log
        .iter()
        .filter(|c| c.starts_with("toolchain uninstall"))
        .count();
    assert_eq!(uninstalls, 2, "calls: {log:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("attempt 1 of 5") && stderr.contains("dns error"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_install_that_keeps_failing_fails_the_build() {
    let dir = scratch("fail");
    let rustup = fake_rustup(
        &dir,
        "case \"$1 $2\" in 'toolchain install') echo 'error: dns error' >&2; exit 1 ;; esac",
    );
    let out = run_script(&rustup);
    assert!(!out.status.success());
    assert_eq!(installs(&dir), 5, "calls: {:?}", calls(&dir));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("after 5 attempts"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_rustup_is_not_an_error() {
    let dir = scratch("none");
    // An empty PATH apart from python3's own directory, so `which rustup` fails.
    let python = which("python3").expect("python3 on PATH");
    let out = Command::new(&python)
        .arg(repo_root().join("scripts/ensure_rust_toolchain.py"))
        .env("PATH", &dir)
        .env_remove("SV_RUSTUP")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("no rustup on PATH"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join(name))
            .find(|p| p.is_file())
    })
}
