//! Guards for `scripts/registry_target.py`, which `PublishExtension.yml` runs to
//! decide which `description.yml` key a release goes in for the DuckDB
//! community-extensions registry, and to write it there.
//!
//! Nothing else exercises it: it runs only inside that workflow, against files
//! fetched from `duckdb/community-extensions`. Those files changed layout once
//! already. When the registry moved its default build to a DuckDB 2.0 commit, the
//! old gate (a grep for a `vX.Y.Z` `DUCKDB_VERSION`) found nothing and stopped the
//! v0.14.0 publish. The fixtures below are trimmed copies of both layouts.

use std::path::PathBuf;
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A fresh scratch directory, removed first so a crashed previous run cannot
/// leak state into this one.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("sv-registry-target-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new("python3")
        .arg(repo_root().join("scripts/registry_target.py"))
        .args(args)
        .output()
        .expect("run python3 (is it on PATH?)")
}

/// `registry_target.py key <pin> <build.yml> <build.py>`: the key on success,
/// the error message on failure.
fn key(name: &str, pin: &str, build_yml: &str, build_py: &str) -> Result<String, String> {
    let dir = scratch(name);
    let (yml, py) = (dir.join("build.yml"), dir.join("build.py"));
    std::fs::write(&yml, build_yml).expect("write build.yml");
    std::fs::write(&py, build_py).expect("write build.py");
    let out = run(&["key", pin, yml.to_str().unwrap(), py.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

/// `registry_target.py set <description.yml> <key> <sha>`, returning the file.
fn set(name: &str, descriptor: &str, key: &str, sha: &str) -> String {
    let dir = scratch(name);
    let path = dir.join("description.yml");
    std::fs::write(&path, descriptor).expect("write description.yml");
    let out = run(&["set", path.to_str().unwrap(), key, sha]);
    assert!(
        out.status.success(),
        "registry_target.py set failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result = std::fs::read_to_string(&path).expect("read description.yml");
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The registry's build.yml up to v1.5.6: a bare tag.
const BUILD_YML_TAGGED: &str = "jobs:\n  build:\n    env:\n      DUCKDB_VERSION: v1.5.6\n";

/// Its build.yml since the default moved to a DuckDB 2.0 commit.
const BUILD_YML_SHA: &str = "\
jobs:
  build:
    env:
      DUCKDB_LATEST_STABLE: 'b15358c371daf10dad799a43b6167f6ccdce84d8'
      DUCKDB_VERSION: ${{ inputs.duckdb_version || 'b15358c371daf10dad799a43b6167f6ccdce84d8' }}
";

/// Its build.py, with the per-release keys it reads instead of `ref`.
const BUILD_PY: &str = "\
	extension_ref = desc['repo']['ref']
	if os.environ['DUCKDB_VERSION'] == 'v1.4.5':
		if 'andium' in desc['repo']:
			extension_ref = desc['repo']['andium']
	elif os.environ['DUCKDB_VERSION'] == 'v1.5.6':
		if 'variegata' in desc['repo']:
			extension_ref = desc['repo']['variegata']
	elif os.environ['DUCKDB_VERSION'] != os.environ['DUCKDB_LATEST_STABLE']:
		if desc['repo'].get('ref_next'):
			extension_ref = desc['repo']['ref_next']
";

const DESCRIPTOR: &str = "\
extension:
  name: semantic_views
  version: 0.13.0

repo:
  github: s-layer/duckdb-semantic-views
  ref: 27e6271218f3ee9687e9f15cae6e85171eded45c

docs:
  hello_world: |
    -- ref: not this one
";

#[test]
fn a_pin_equal_to_the_registry_default_goes_in_ref() {
    assert_eq!(
        key("tagged", "v1.5.6", BUILD_YML_TAGGED, BUILD_PY).as_deref(),
        Ok("ref")
    );
}

/// The v0.14.0 case: the default is a 2.0 commit, and v1.5.6 is read from
/// `variegata`.
#[test]
fn a_pin_on_an_older_registry_line_goes_in_that_lines_key() {
    assert_eq!(
        key("variegata", "v1.5.6", BUILD_YML_SHA, BUILD_PY).as_deref(),
        Ok("variegata")
    );
    assert_eq!(
        key("andium", "v1.4.5", BUILD_YML_SHA, BUILD_PY).as_deref(),
        Ok("andium")
    );
}

#[test]
fn a_pin_the_registry_does_not_build_is_refused() {
    let err = key("unbuilt", "v1.5.7", BUILD_YML_SHA, BUILD_PY).unwrap_err();
    assert!(err.contains("does not build DuckDB v1.5.7"), "{err}");
    assert!(err.contains("v1.4.5, v1.5.6"), "{err}");
}

#[test]
fn an_unreadable_build_yml_is_refused() {
    let err = key("unreadable", "v1.5.6", "jobs: {}\n", BUILD_PY).unwrap_err();
    assert!(
        err.contains("could not find the registry's default"),
        "{err}"
    );
}

#[test]
fn setting_ref_rewrites_only_the_repo_ref() {
    let out = set("ref", DESCRIPTOR, "ref", "abc");
    assert_eq!(
        out,
        DESCRIPTOR.replace("27e6271218f3ee9687e9f15cae6e85171eded45c", "abc")
    );
}

/// A new key goes after `ref:`, which stays where the previous release left it
/// (the registry still builds its default DuckDB from that).
#[test]
fn setting_a_missing_key_adds_it_after_ref() {
    let out = set("add", DESCRIPTOR, "variegata", "abc");
    assert_eq!(
        out,
        DESCRIPTOR.replace(
            "  ref: 27e6271218f3ee9687e9f15cae6e85171eded45c\n",
            "  ref: 27e6271218f3ee9687e9f15cae6e85171eded45c\n  variegata: abc\n"
        )
    );
}

#[test]
fn setting_an_existing_key_rewrites_it_in_place() {
    let once = set("again-1", DESCRIPTOR, "variegata", "abc");
    let twice = set("again-2", &once, "variegata", "def");
    assert_eq!(twice, once.replace("variegata: abc", "variegata: def"));
}

/// `registry_target.py release-sha <version>` run inside `repo`: the SHA on
/// success, the error message on failure.
fn release_sha(repo: &std::path::Path, version: &str) -> Result<String, String> {
    let out = Command::new("python3")
        .arg(repo_root().join("scripts/registry_target.py"))
        .args(["release-sha", version])
        .current_dir(repo)
        .output()
        .expect("run python3 (is it on PATH?)");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(["-c", "tag.gpgsign=false"])
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repo whose `v1.2.3` tag is one commit behind HEAD, as `main` was when
/// `just release` ran for v0.14.0: it pinned HEAD (the Phase 2 merge and a
/// docs fix) instead of the tagged release.
fn repo_with_tag_behind_head(name: &str, annotated: bool) -> (PathBuf, String, String) {
    let dir = scratch(name);
    git(&dir, &["init", "-q"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "release"]);
    let tagged = git(&dir, &["rev-parse", "HEAD"]);
    if annotated {
        git(&dir, &["tag", "-a", "v1.2.3", "-m", "v1.2.3"]);
    } else {
        git(&dir, &["tag", "v1.2.3"]);
    }
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "later work"]);
    let head = git(&dir, &["rev-parse", "HEAD"]);
    (dir, tagged, head)
}

#[test]
fn the_release_sha_is_the_tagged_commit_not_head() {
    for annotated in [false, true] {
        let (dir, tagged, head) = repo_with_tag_behind_head(&format!("sha-{annotated}"), annotated);
        let got = release_sha(&dir, "1.2.3");
        let _ = std::fs::remove_dir_all(&dir);
        assert_ne!(tagged, head);
        assert_eq!(got, Ok(tagged), "annotated={annotated}");
    }
}

#[test]
fn a_release_with_no_tag_is_refused() {
    let (dir, _, _) = repo_with_tag_behind_head("sha-missing", false);
    let got = release_sha(&dir, "9.9.9");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("a missing tag must fail");
    assert!(err.contains("v9.9.9"), "{err}");
}

#[test]
fn a_branch_named_like_the_tag_does_not_count_as_the_release() {
    let (dir, _, _) = repo_with_tag_behind_head("sha-branch", false);
    git(&dir, &["branch", "v9.9.9"]);
    let got = release_sha(&dir, "9.9.9");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("a branch is not a release tag");
    assert!(err.contains("v9.9.9"), "{err}");
}

/// `registry_target.py release-sha <version> <branch>`: the tagged commit,
/// refused unless `<branch>` contains it.
fn release_sha_on(repo: &std::path::Path, version: &str, branch: &str) -> Result<String, String> {
    let out = Command::new("python3")
        .arg(repo_root().join("scripts/registry_target.py"))
        .args(["release-sha", version, branch])
        .current_dir(repo)
        .output()
        .expect("run python3 (is it on PATH?)");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

#[test]
fn a_tag_on_the_release_branch_is_the_release() {
    let (dir, tagged, _) = repo_with_tag_behind_head("sha-on-branch", false);
    git(&dir, &["branch", "-M", "main"]);
    let got = release_sha_on(&dir, "1.2.3", "main");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(got, Ok(tagged));
}

/// A tag made on a feature branch (or a local commit never pushed) must not
/// be published: the registry would build a commit `main` doesn't have.
#[test]
fn a_tag_off_the_release_branch_is_refused() {
    let (dir, _, _) = repo_with_tag_behind_head("sha-off-branch", false);
    git(&dir, &["branch", "-M", "main"]);
    git(&dir, &["checkout", "-q", "-b", "feature", "HEAD~1"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "unmerged"]);
    git(&dir, &["tag", "v2.0.0"]);
    let got = release_sha_on(&dir, "2.0.0", "main");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("a tag that main does not contain must fail");
    assert!(err.contains("v2.0.0") && err.contains("main"), "{err}");
}

/// A branch that does not resolve is a bad argument, not a release that is
/// off the branch.
#[test]
fn a_branch_that_does_not_resolve_is_reported_as_such() {
    let (dir, _, _) = repo_with_tag_behind_head("sha-no-branch", false);
    let got = release_sha_on(&dir, "1.2.3", "origin/nope");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("an unknown branch must fail");
    assert!(
        err.contains("could not check v1.2.3 against 'origin/nope'"),
        "{err}"
    );
    assert!(!err.contains("is not on"), "{err}");
}

/// `registry_target.py unpushed <version> <branch>`, run in `repo`: success when
/// `just release` may go on, the error message when it must stop.
fn unpushed(repo: &std::path::Path, version: &str, branch: &str) -> Result<(), String> {
    let out = Command::new("python3")
        .arg(repo_root().join("scripts/registry_target.py"))
        .args(["unpushed", version, branch])
        .current_dir(repo)
        .output()
        .expect("run python3 (is it on PATH?)");
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

/// A repo whose `main` matches the `pushed` branch, standing in for
/// `origin/main`, with a description.yml on both.
fn repo_in_sync(name: &str) -> PathBuf {
    let dir = scratch(name);
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("description.yml"), "repo:\n  ref: aaa\n").unwrap();
    std::fs::write(dir.join("CHANGELOG.md"), "# Changelog\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "release"]);
    git(&dir, &["branch", "pushed"]);
    dir
}

/// Commit a change to `file` on the current branch.
fn commit_change(dir: &std::path::Path, files: &[&str], message: &str) {
    for file in files {
        let path = dir.join(file);
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str("changed\n");
        std::fs::write(&path, text).unwrap();
    }
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", message]);
}

#[test]
fn a_main_level_with_the_remote_may_release() {
    let dir = repo_in_sync("unpushed-none");
    let got = unpushed(&dir, "1.2.3", "pushed");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(got, Ok(()));
}

/// `git merge --ff-only` passes when local main is ahead, and the release push
/// would then publish whatever was on it.
#[test]
fn an_unpushed_commit_on_main_stops_the_release() {
    let dir = repo_in_sync("unpushed-wip");
    commit_change(&dir, &["CHANGELOG.md"], "wip: tweak changelog");
    let got = unpushed(&dir, "1.2.3", "pushed");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("an unpushed commit must stop the release");
    assert!(
        err.contains("not on pushed") && err.contains("wip: tweak changelog"),
        "{err}"
    );
}

/// A re-run after the release push failed finds its own description.yml
/// commit still unpushed, and goes on to push it.
#[test]
fn a_rerun_with_only_this_releases_description_commit_may_release() {
    let dir = repo_in_sync("unpushed-rerun");
    commit_change(
        &dir,
        &["description.yml"],
        "chore: update description.yml for v1.2.3 release",
    );
    let got = unpushed(&dir, "1.2.3", "pushed");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(got, Ok(()));
}

#[test]
fn another_releases_description_commit_stops_the_release() {
    let dir = repo_in_sync("unpushed-other-version");
    commit_change(
        &dir,
        &["description.yml"],
        "chore: update description.yml for v1.2.2 release",
    );
    let got = unpushed(&dir, "1.2.3", "pushed");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("a commit for another release must stop it");
    assert!(err.contains("v1.2.2"), "{err}");
}

#[test]
fn a_description_commit_that_touches_other_files_stops_the_release() {
    let dir = repo_in_sync("unpushed-wider");
    commit_change(
        &dir,
        &["description.yml", "CHANGELOG.md"],
        "chore: update description.yml for v1.2.3 release",
    );
    let got = unpushed(&dir, "1.2.3", "pushed");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("a commit that changes more than description.yml must stop it");
    assert!(err.contains("not on pushed"), "{err}");
}

#[test]
fn a_description_commit_on_top_of_unpushed_work_stops_the_release() {
    let dir = repo_in_sync("unpushed-stacked");
    commit_change(&dir, &["CHANGELOG.md"], "wip: tweak changelog");
    commit_change(
        &dir,
        &["description.yml"],
        "chore: update description.yml for v1.2.3 release",
    );
    let got = unpushed(&dir, "1.2.3", "pushed");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("unpushed work under the release commit must stop it");
    assert!(err.contains("wip: tweak changelog"), "{err}");
}

#[test]
fn an_unpushed_check_against_a_missing_branch_is_reported_as_such() {
    let dir = repo_in_sync("unpushed-no-branch");
    let got = unpushed(&dir, "1.2.3", "origin/nope");
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("an unknown branch must fail");
    assert!(
        err.contains("could not list commits not on 'origin/nope'"),
        "{err}"
    );
}
