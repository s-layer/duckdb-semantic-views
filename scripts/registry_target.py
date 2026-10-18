#!/usr/bin/env python3
"""Decide where in description.yml a release goes for the community-extensions registry.

The registry builds every extension against one default DuckDB (`DUCKDB_LATEST_STABLE`
in its `.github/workflows/build.yml`), reading the descriptor's `repo.ref`. It also
keeps building some older DuckDB releases, and for each of those its `scripts/build.py`
reads a release-named key instead of `ref` when the descriptor has one, e.g.

    elif os.environ['DUCKDB_VERSION'] == 'v1.5.6':
        if 'variegata' in desc['repo']:
            extension_ref = desc['repo']['variegata']

So a release pinned to the registry's default goes in `ref`, and a release pinned to
one of those older versions goes in that version's key. Any other pin is not built by
the registry at all, and publishing it would only produce a failing registry build.

Usage:
    registry_target.py key <our-pin> <build.yml> <build.py>
        Print the descriptor key (`ref`, or e.g. `variegata`). Exit 1, with the reason
        on stderr, when the registry does not build <our-pin>.
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
LATEST_STABLE = re.compile(r"^\s*DUCKDB_LATEST_STABLE:\s*['\"]?([^'\"\s]+)", re.M)
VERSION_DEFAULT = re.compile(r"^\s*DUCKDB_VERSION:\s*(?:\$\{\{.*\|\|\s*)?['\"]?([^'\"\s}]+)", re.M)
# One `== '<version>'` branch of build.py and the descriptor key it reads.
VERSION_BRANCH = re.compile(
    r"os\.environ\['DUCKDB_VERSION'\]\s*==\s*'([^']+)'\s*:\s*"
    r"if\s+'([A-Za-z_][A-Za-z0-9_]*)'\s+in\s+desc\['repo'\]"
)


def registry_default(build_yml: str) -> str | None:
    m = LATEST_STABLE.search(build_yml) or VERSION_DEFAULT.search(build_yml)
    return m.group(1) if m else None


def descriptor_key(pin: str, build_yml: str, build_py: str) -> str:
    default = registry_default(build_yml)
    if default is None:
        raise SystemExit("could not find the registry's default DuckDB in build.yml")
    if default == pin:
        return "ref"
    keys = {version: key for version, key in VERSION_BRANCH.findall(build_py)}
    if pin in keys:
        return keys[pin]
    supported = ", ".join(sorted(keys)) or "none"
    raise SystemExit(
        f"the registry does not build DuckDB {pin}: its default is {default} and the "
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
    if len(argv) == 5 and argv[1] == "key":
        pin = argv[2].strip()
        with open(argv[3]) as f:
            build_yml = f.read()
        with open(argv[4]) as f:
            build_py = f.read()
        print(descriptor_key(pin, build_yml, build_py))
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
