#!/usr/bin/env python3
"""Decide where in description.yml a release goes for the community-extensions registry.

The registry builds every extension against one default DuckDB (`DUCKDB_LATEST_STABLE`
in its `.github/workflows/build.yml`), reading the descriptor's `repo.ref`. It also
keeps building some older DuckDB releases, and for each of those its `scripts/build.py`
reads a release-named key instead of `ref` when the descriptor has one, e.g.

    elif os.environ['DUCKDB_VERSION'] == 'v1.5.6':
        if 'variegata' in desc['repo']:
            extension_ref = desc['repo']['variegata']

When that default is a commit, its build job's `duckdb_tag` default names the release
it is built as (`v2.0.0-alpha46057` for `b15358c…`), and a pin naming that release is
the default too, as is a release whose tag points at that commit (the registry may
name a release by commit with no label). So a release pinned to the registry's default
goes in `ref`, and a release pinned to one of those older versions goes in that
version's key. Any other pin is not built by the registry's own runs, and publishing it
would only produce a failing registry build. The one exception is not modelled here:
for a DuckDB other than the default that a `workflow_dispatch` run of the registry names,
`build.py` reads `repo.ref_next`, which a maintainer sets by hand when wanted
(see MAINTAINER.md).

Usage:
    registry_target.py key <our-pin> <build.yml> <build.py> [<pin-commit>]
        Print the descriptor key (`ref`, or e.g. `variegata`). Exit 1, with the reason
        on stderr, when the registry does not build <our-pin>. <pin-commit> is the
        DuckDB commit our pin's tag points at; when the registry's default is that
        commit, the pin is its default build. Empty or absent, it is not consulted.
    registry_target.py set <description.yml> <key> <sha>
        Point `repo.<key>` at <sha>, adding the key after `ref:` if it is missing.
    registry_target.py release-sha <version> [<branch>]
        Print the commit the `v<version>` tag points at, run inside our checkout.
        Exit 1 when there is no such tag. The registry must build the tagged
        release, not whatever `main` has moved on to since (v0.14.0 was first
        published pinned to a later `main`). With <branch> (`just release` passes
        `origin/main`), also exit 1 unless <branch> contains that commit, so a tag
        made on a feature branch or an unpushed commit is never published.
    registry_target.py unpushed <version> <branch>
        Exit 1 when HEAD has commits <branch> does not, listing them, since the
        release pushes HEAD to <branch> and would publish them with it. The one
        exception is a re-run after that push failed: a single commit with the
        release's own subject that changes only description.yml.
"""

from __future__ import annotations

import re
import subprocess
import sys

# `DUCKDB_LATEST_STABLE: 'b15358c…'` (current layout), else the default in
# `DUCKDB_VERSION: ${{ inputs.duckdb_version || 'v1.5.6' }}` or a bare
# `DUCKDB_VERSION: v1.5.6` (the layout through v1.5.6).
LATEST_STABLE = re.compile(r"^[ \t]*DUCKDB_LATEST_STABLE:\s*['\"]?([^'\"\s]+)", re.M)
VERSION_DEFAULT = re.compile(r"^[ \t]*DUCKDB_VERSION:\s*(?:\$\{\{.*\|\|\s*)?['\"]?([^'\"\s}]+)", re.M)
# A job's `duckdb_version:` / `duckdb_tag:` in its `with:` block, as an
# `${{ inputs.x || '<default>' }}` expression or a plain value. The build job's
# `duckdb_tag` is the release label a default *commit* is built under
# (extension-ci-tools tags its clone with it), so a pin naming that label is the
# registry's default build.
JOB_VERSION = re.compile(r"^[ \t]*duckdb_version:[ \t]*(?:\$\{\{.*\|\|\s*)?['\"]?([^'\"\s}]+)", re.M)
JOB_TAG = re.compile(r"^[ \t]*duckdb_tag:[ \t]*(?:\$\{\{.*\|\|\s*)?['\"]?([^'\"\s}]+)", re.M)
# The first key under `jobs:`, whose indentation is that of every job key.
FIRST_JOB = re.compile(r"^jobs:[ \t]*\n(?:[ \t]*(?:#.*)?\n)*([ \t]+)[A-Za-z0-9_-]+:", re.M)
RELEASE_TAG = re.compile(r"^v\d+\.\d+\.\d+")
HEX = re.compile(r"^[0-9a-f]{7,40}$")
# One `== '<version>'` branch of build.py and the descriptor key it reads.
VERSION_BRANCH = re.compile(
    r"os\.environ\['DUCKDB_VERSION'\]\s*==\s*'([^']+)'\s*:\s*"
    r"if\s+'([A-Za-z_][A-Za-z0-9_]*)'\s+in\s+desc\['repo'\]"
)


def registry_default(build_yml: str) -> str | None:
    m = LATEST_STABLE.search(build_yml) or VERSION_DEFAULT.search(build_yml)
    return m.group(1) if m else None


def default_label(build_yml: str, default: str) -> str | None:
    """
    Return the release label the registry builds its default *commit* under, if any.

    Read only from the job whose own `duckdb_version` is that commit, so another job's
    label (an older line's build) is never mistaken for the default's. A default that
    is itself a release tag needs no label, and a stale one left beside it is ignored.
    """
    if RELEASE_TAG.match(default):
        return None
    first = FIRST_JOB.search(build_yml)
    indent = re.escape(first.group(1)) if first else "  "
    # A key at the job keys' indentation (under `jobs:`, or under `on:`) starts a block.
    job_key = re.compile(rf"^{indent}[A-Za-z0-9_-]+:[ \t]*$", re.M)
    starts = [m.start() for m in job_key.finditer(build_yml)] + [len(build_yml)]
    for start, end in zip(starts, starts[1:]):
        block = build_yml[start:end]
        version, tag = JOB_VERSION.search(block), JOB_TAG.search(block)
        if version and tag and version.group(1) == default:
            return tag.group(1)
    return None


def same_commit(a: str, b: str) -> bool:
    """Return whether two commit ids, either of which may be abbreviated, name one commit."""
    a, b = a.lower(), b.lower()
    return bool(HEX.match(a) and HEX.match(b)) and (a.startswith(b) or b.startswith(a))


def descriptor_key(pin: str, build_yml: str, build_py: str, pin_commit: str = "") -> str:
    default = registry_default(build_yml)
    if default is None:
        raise SystemExit("could not find the registry's default DuckDB in build.yml")
    label = default_label(build_yml, default)
    if pin in (default, label) or same_commit(pin_commit, default):
        return "ref"
    keys = {version: key for version, key in VERSION_BRANCH.findall(build_py)}
    if pin in keys:
        return keys[pin]
    supported = ", ".join(sorted(keys)) or "none"
    raise SystemExit(
        f"the registry does not build DuckDB {pin}: its default is {default}"
        f"{f' (built as {label})' if label else ''} and the "
        f"older releases it builds are {supported}. Align .duckdb-version with one of "
        "those (or wait for the registry) before publishing."
    )


def set_key(text: str, key: str, sha: str) -> str:
    lines = text.splitlines(keepends=True)
    try:
        start = next(i for i, line in enumerate(lines) if line.rstrip() == "repo:")
    except StopIteration:
        raise SystemExit("description.yml has no top-level `repo:` block") from None
    end = next(
        (
            i
            for i in range(start + 1, len(lines))
            if lines[i].strip() and not lines[i].startswith(" ")
        ),
        len(lines),
    )
    entry = re.compile(rf"^  {re.escape(key)}:")
    for i in range(start + 1, end):
        if entry.match(lines[i]):
            lines[i] = f"  {key}: {sha}\n"
            return "".join(lines)
    ref = next((i for i in range(start + 1, end) if lines[i].startswith("  ref:")), None)
    if ref is None:
        raise SystemExit("description.yml `repo:` block has no `ref:`")
    lines.insert(ref + 1, f"  {key}: {sha}\n")
    return "".join(lines)


def release_sha(version: str, branch: str | None = None) -> str:
    tag = f"v{version}"
    out = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}^{{commit}}"],
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        raise SystemExit(f"no {tag} tag in this checkout: tag the release (or fetch tags) first")
    sha = out.stdout.strip()
    if branch is not None:
        on_branch = subprocess.run(
            ["git", "merge-base", "--is-ancestor", sha, branch], capture_output=True, text=True
        )
        # 1 is "not an ancestor"; anything else means git could not check.
        if on_branch.returncode == 1:
            raise SystemExit(
                f"{tag} ({sha[:12]}) is not on {branch}: merge the release to {branch} "
                f"and tag it there"
            )
        if on_branch.returncode != 0:
            raise SystemExit(
                f"could not check {tag} against {branch!r} (is it a ref in this checkout?): "
                f"{on_branch.stderr.strip()}"
            )
    return sha


def release_commit_subject(version: str) -> str:
    return f"chore: update description.yml for v{version} release"


def check_unpushed(version: str, branch: str) -> None:
    out = subprocess.run(
        ["git", "log", "--format=%H %s", f"{branch}..HEAD"], capture_output=True, text=True
    )
    if out.returncode != 0:
        raise SystemExit(
            f"could not list commits not on {branch!r} (is it a ref in this checkout?): "
            f"{out.stderr.strip()}"
        )
    commits = [line.split(" ", 1) for line in out.stdout.splitlines()]
    if not commits:
        return
    if len(commits) == 1 and commits[0][1] == release_commit_subject(version):
        files = subprocess.run(
            ["git", "diff-tree", "--no-commit-id", "--name-only", "-r", commits[0][0]],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.split()
        if files == ["description.yml"]:
            return
    listed = "\n".join(f"  {sha[:12]} {subject}" for sha, subject in commits)
    raise SystemExit(
        f"local HEAD has commits that are not on {branch}, and the release would push "
        f"them:\n{listed}\nPush them through review, or drop them, then re-run."
    )


def main(argv: list[str]) -> None:
    if len(argv) in (5, 6) and argv[1] == "key":
        pin = argv[2].strip()
        with open(argv[3]) as f:
            build_yml = f.read()
        with open(argv[4]) as f:
            build_py = f.read()
        pin_commit = argv[5].strip() if len(argv) == 6 else ""
        print(descriptor_key(pin, build_yml, build_py, pin_commit))
    elif len(argv) == 5 and argv[1] == "set":
        path, key, sha = argv[2], argv[3], argv[4]
        with open(path) as f:
            text = f.read()
        with open(path, "w") as f:
            f.write(set_key(text, key, sha))
    elif len(argv) in (3, 4) and argv[1] == "release-sha":
        print(release_sha(argv[2].strip(), argv[3].strip() if len(argv) == 4 else None))
    elif len(argv) == 4 and argv[1] == "unpushed":
        check_unpushed(argv[2].strip(), argv[3].strip())
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
