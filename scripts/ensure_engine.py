#!/usr/bin/env python3
"""
Build the DuckDB engine the C++ shim links against, and point build.rs at it.

`build.rs` links a static DuckDB engine *into* the extension binary alongside
`cpp/src/shim.cpp`, because the parser-extension hook the shim registers is C++
internals with no C-API equivalent. That makes the engine an ABI contract, not
a convenience: the C++ reads host objects by layout, so the host that loads the
extension must be the exact DuckDB release the shim was compiled against
(`src/load_guard.rs` refuses any other at load time).

DuckDB 2.0 ships no amalgamation (`scripts/amalgamation.py` is a remnant) and
its release archives carry no C++ headers, so the engine is built here from
source with its own CMake build, and only the `duckdb_static` target.

Two sources, in priority order:

1. **An engine source tree** (`--src-dir`, default `./duckdb`). A harness that
   builds the extension against a specific engine clones it there (upstream
   community-extensions and extension-ci-tools do, as does Haybarn's fork).
   That tree *is* the engine the extension will be loaded into, so it is built
   under the version label its release build carries (see `tree_label`), never
   the one this repo pins: stamping our pin onto some other engine is the one
   way to build an extension that loads where its shim does not fit. Whatever
   the tree is, the extension then loads only in a DuckDB built from it (the
   load-time check compares the labels), so a tree other than the pinned
   release is reported but not refused.
2. **The pinned release** (`.duckdb-version`), from the PyPI sdist of the
   matching `duckdb` Python package (`.duckdb-pypi-version` while 2.0 is a
   pre-release, otherwise the version without its `v`). The sdist carries the
   exact engine source the wheel was built from, including `third_party/`, and
   PyPI is reachable from sandboxes that block GitHub. It is built with
   `DUCKDB_VERSION` set to the pin, so the library reports the same version
   string as the wheel. This is the local-developer and CI path.

The result is cached under `.engine/<id>/` (the static libraries plus, for the
pinned release, the extracted source for its headers), so switching branches
or engines is a cache hit. `cpp/engine.manifest` records which entry the build
uses: `build.rs` reads the include directories and libraries from it, and
reruns when it changes. It is rewritten only when its content changes, so an
unchanged engine keeps `cargo build` incremental.

Usage (normally invoked by `make ensure_engine`):

    python3 scripts/ensure_engine.py                    # auto-detect
    python3 scripts/ensure_engine.py --force            # rebuild the engine
    python3 scripts/ensure_engine.py --src-dir ''       # pinned release only

Requires cmake (and uses ninja when it is on PATH). Only the Python standard
library is needed.
"""

from __future__ import annotations

import argparse
import contextlib
import glob
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
from typing import NoReturn

DEFAULT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join("cpp", "engine.manifest")
# Engine builds that predate this layout or a change to how it is built would
# be reused from the cache without this in the id.
LAYOUT = "3"
# A DuckDB release label, as DuckDB's CMake parses one.
RELEASE_LABEL_RE = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+(-(alpha|rc|dev)[0-9]+)?$")
# A commit, as extension-ci-tools passes `duckdb_version` when it is a SHA.
COMMIT_RE = re.compile(r"^[0-9a-fA-F]{7,40}$")
# The variables a harness labels its engine build with, in the order DuckDB's
# CMake reads them.
# What a pre-engine (DuckDB 1.x) build installed. `cpp/include` is on the shim's
# include path, so a leftover `duckdb.hpp` there would be included in place of
# the engine's own header.
STALE_AMALGAMATION = (
    "cpp/include/duckdb.hpp",
    "cpp/include/duckdb.cpp",
    "cpp/include/.amalgamation_id",
)
HARNESS_LABEL_VARS = ("DUCKDB_VERSION", "OVERRIDE_GIT_DESCRIBE")
# The CMake configure line logs the label the build stamps into the library.
CMAKE_VERSION_RE = re.compile(r"git hash \S+, version (\S+),")
# Static libraries of the `duckdb_static` target that a link needs. jemalloc is
# left out: the extension must use the host's allocator, not install its own.
THIRD_PARTY_SKIP = ("jemalloc",)


def log(msg: str) -> None:
    print(f"[ensure-engine] {msg}", flush=True)


def fail(msg: str) -> NoReturn:
    raise SystemExit(f"[ensure-engine] ERROR: {msg}")


def read_pin(root: str, name: str) -> str | None:
    path = os.path.join(root, name)
    if not os.path.exists(path):
        return None
    with open(path, encoding="utf-8") as fh:
        return fh.read().strip() or None


def pypi_version(root: str, version: str) -> str:
    """The `duckdb` PyPI release whose wheel reports `version`."""
    return read_pin(root, ".duckdb-pypi-version") or version.removeprefix("v")


def engine_source_tree(root: str, src_dir: str) -> str | None:
    """
    Absolute path of a usable engine source tree, or None.

    "Usable" means it carries a CMake build and the headers the shim compiles
    against. A bare `duckdb/` directory that is something else -- an empty
    submodule mount point, a stray checkout of the Python package -- is not
    mistaken for an engine.
    """
    if not src_dir:
        return None
    path = src_dir if os.path.isabs(src_dir) else os.path.join(root, src_dir)
    if os.path.exists(os.path.join(path, "CMakeLists.txt")) and os.path.isdir(
        os.path.join(path, "src", "include", "duckdb")
    ):
        return os.path.abspath(path)
    return None


def git_commit(tree: str) -> str | None:
    """
    Short commit hash of `tree`, or None when it is not a git checkout.

    `safe.directory=*` because CI hands the tree to the build across an
    ownership boundary (the community-extensions Linux leg clones on the host
    and builds inside a container), where git otherwise refuses it outright.
    """
    try:
        out = subprocess.run(
            ["git", "-c", "safe.directory=*", "-C", tree, "rev-parse", "--short=10", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    return out.stdout.strip() or None


def git_dirty(tree: str) -> bool:
    """Whether `tree` has uncommitted changes to its tracked files."""
    try:
        out = subprocess.run(
            [
                "git",
                "-c",
                "safe.directory=*",
                "-C",
                tree,
                "status",
                "--porcelain",
                "--untracked-files=no",
            ],
            capture_output=True,
            text=True,
            check=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return False
    return bool(out.stdout.strip())


def git_exact_tag(tree: str) -> str | None:
    """The release tag `tree` is checked out at, or None."""
    try:
        out = subprocess.run(
            ["git", "-c", "safe.directory=*", "-C", tree, "describe", "--tags", "--exact-match"],
            capture_output=True,
            text=True,
            check=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    tag = out.stdout.strip()
    return tag if RELEASE_LABEL_RE.match(tag) else None


def git_ref_names(tree: str) -> set[str]:
    """
    Every branch and tag name in `tree`, short and remote-relative.

    `origin/main` is listed as `main` too: a harness checks out a branch
    detached, so its name survives only as the remote-tracking ref.
    """
    try:
        out = subprocess.run(
            ["git", "-c", "safe.directory=*", "-C", tree, "for-each-ref", "--format=%(refname)"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return set()
    names = set()
    for ref in out.split():
        for prefix in ("refs/heads/", "refs/tags/"):
            if ref.startswith(prefix):
                names.add(ref[len(prefix) :])
        if ref.startswith("refs/remotes/"):
            remote_ref = ref[len("refs/remotes/") :]
            names.add(remote_ref)
            names.add(remote_ref.split("/", 1)[-1])
    return names


def tree_label(tree: str) -> tuple[str | None, str]:
    """
    The version label to build `tree` with, and where it came from.

    The library reports this label as its version, and the extension refuses
    any host reporting another, so it has to be what the host's own build of
    the same source says. DuckDB's CMake stamps `DUCKDB_VERSION` (else
    `OVERRIDE_GIT_DESCRIBE`) verbatim, and otherwise makes up a `-devN` label
    from `release_version.txt` and the commit count -- even for a checkout of
    a release tag. So:

    1. a harness label that is a release label (`v2.0.0`, `v2.0.0-alpha47600`)
       is used as is;
    2. a harness value that is a git reference rather than a label -- a commit
       (community-extensions and extension-ci-tools pass `duckdb_version` as
       one) or a branch or tag name of the tree (`main`) -- names *which*
       source, not what it is called, so it is skipped;
    3. any other harness label (a fork's `haybarn-v1.5.5-rc1-42-g<sha>`, a
       nightly's `git describe`) is used verbatim, as the harness's own engine
       build does with the same variable: it is never cut down to a release
       label it is not;
    4. otherwise a release tag the tree is checked out at;
    5. otherwise CMake's own `-devN` label (None). That is a guess: N is the
       commit count of the clone, so a shallow clone gets a different label
       than the full one upstream builds from.
    """
    refs = None
    for name in HARNESS_LABEL_VARS:
        value = os.environ.get(name, "").strip()
        if not value:
            continue
        if RELEASE_LABEL_RE.match(value):
            return value, f"{name}={value}"
        if refs is None:
            refs = git_ref_names(tree)
        if COMMIT_RE.match(value) or value in refs:
            continue
        return value, f"{name}={value}, the harness's own label, used verbatim"
    tag = git_exact_tag(tree)
    if tag:
        return tag, f"the tree is checked out at {tag}"
    return None, (
        "not a release, and the harness gave no label: CMake labels it from "
        "release_version.txt and the clone's commit count, which matches the "
        "host only if the host was built from a clone of the same depth"
    )


def download_sdist(pypi: str, archive: str) -> None:
    """Download the sdist of the `duckdb` release `pypi` from PyPI to `archive`."""
    url = f"https://pypi.org/pypi/duckdb/{pypi}/json"
    log(f"looking up the duckdb {pypi} sdist ({url})")
    try:
        with urllib.request.urlopen(url) as resp:
            meta = json.load(resp)
    except OSError as exc:
        fail(f"could not read {url} ({exc})")
    sdists = [u for u in meta.get("urls", []) if u.get("packagetype") == "sdist"]
    if not sdists:
        fail(f"duckdb {pypi} has no sdist on PyPI")
    log(f"downloading {sdists[0]['url']}")
    try:
        urllib.request.urlretrieve(sdists[0]["url"], archive)
    except OSError as exc:
        fail(f"could not download {sdists[0]['url']} ({exc})")


def check_sdist_version(tar: tarfile.TarFile, pypi: str) -> None:
    """
    Fail unless the archive is the sdist of `duckdb` release `pypi`.

    The engine source carries no commit or label of its own -- the build
    takes its label from us -- so the archive's own metadata is the only
    evidence that it is the release the wheel pin names.
    """
    info = [m for m in tar.getmembers() if m.name.count("/") == 1 and m.name.endswith("/PKG-INFO")]
    if not info:
        fail("the sdist has no PKG-INFO, so its release cannot be checked")
    fh = tar.extractfile(info[0])
    text = fh.read().decode("utf-8", "replace") if fh else ""
    found = next(
        (
            line.split(":", 1)[1].strip()
            for line in text.splitlines()
            if line.startswith("Version:")
        ),
        None,
    )
    if found != pypi:
        fail(
            f"the sdist is duckdb {found}, not duckdb {pypi}: its engine would be built "
            f"under a label it does not carry. Use the sdist of duckdb {pypi}."
        )


def extract_engine(pypi: str, dest: str, sdist: str | None) -> None:
    """
    Extract the engine source (`external/duckdb`) of the sdist into `dest`.

    `sdist` is a local copy of the archive; without one it comes from PyPI.
    """
    with tempfile.TemporaryDirectory(prefix="dd-engine-") as tmp:
        archive = sdist or os.path.join(tmp, "sdist.tar.gz")
        if not sdist:
            download_sdist(pypi, archive)
        with tarfile.open(archive) as tar:
            check_sdist_version(tar, pypi)
            prefix = None
            members = []
            for member in tar.getmembers():
                parts = member.name.split("/")
                if len(parts) > 2 and parts[1:3] == ["external", "duckdb"]:
                    prefix = "/".join(parts[:3]) + "/"
                    members.append(member)
            if not prefix:
                fail(f"the duckdb {pypi} sdist has no external/duckdb engine source")
            for member in members:
                member.name = member.name[len(prefix) :]
            members = [m for m in members if m.name]
            try:
                tar.extractall(tmp + "/src", members=members, filter="data")
            except TypeError:  # Python without extraction filters (< 3.12, unpatched)
                tar.extractall(tmp + "/src", members=members)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        shutil.move(os.path.join(tmp, "src"), dest)


def platform_flags() -> list[str]:
    """
    CMake flags that make the engine match the target the extension is built for.

    extension-ci-tools cross-compiles osx_amd64 on an arm64 runner and says so
    in the `OSX_BUILD_ARCH` environment variable; DuckDB's CMake reads it only
    as a CMake variable, where it also sets the macOS 11.0 deployment target
    DuckDB's own builds use. A native macOS build gets that target too, so the
    extension needs no newer macOS than DuckDB does.
    """
    arch = os.environ.get("OSX_BUILD_ARCH", "").strip()
    if arch:
        return [f"-DOSX_BUILD_ARCH={arch}"]
    if sys.platform == "darwin":
        target = os.environ.get("MACOSX_DEPLOYMENT_TARGET", "").strip() or "11.0"
        return [f"-DCMAKE_OSX_DEPLOYMENT_TARGET={target}"]
    return []


def configure_engine(src: str, build: str, env: dict[str, str]) -> str:
    """Configure the engine build; return the label CMake stamps into it."""
    generator = ["-G", "Ninja"] if shutil.which("ninja") else []
    configure = [
        "cmake",
        "-S",
        src,
        "-B",
        build,
        *generator,
        "-DCMAKE_BUILD_TYPE=Release",
        "-DBUILD_SHELL=0",
        "-DBUILD_UNITTESTS=0",
        "-DENABLE_UNITTEST_CPP_TESTS=0",
        "-DCMAKE_POSITION_INDEPENDENT_CODE=ON",
        *platform_flags(),
    ]
    log("configuring: " + " ".join(configure))
    try:
        out = subprocess.run(configure, env=env, check=True, capture_output=True, text=True).stdout
    except FileNotFoundError:
        fail("cmake is not on PATH; it is needed to build the DuckDB engine")
    except subprocess.CalledProcessError as exc:
        fail(f"cmake configure failed:\n{exc.stdout}\n{exc.stderr}")
    m = CMAKE_VERSION_RE.search(out)
    if not m:
        fail("could not find the engine version in the cmake configure output")
    return m.group(1)


def build_engine(build: str, env: dict[str, str]) -> None:
    """Build the configured engine's `duckdb_static` target."""
    jobs = str(os.cpu_count() or 2)
    command = [
        "cmake",
        "--build",
        build,
        "--target",
        "duckdb_static",
        "--config",
        "Release",
        "--parallel",
        jobs,
    ]
    log("building (the first build takes a while): " + " ".join(command))
    try:
        subprocess.run(command, env=env, check=True)
    except subprocess.CalledProcessError as exc:
        fail(f"the engine build failed ({exc})")


def static_libraries(build: str) -> list[str]:
    """`duckdb_static` first, then the third-party libraries it depends on."""

    def find(pattern: str) -> list[str]:
        hits = []
        for ext in ("a", "lib"):
            hits += glob.glob(os.path.join(build, "**", f"{pattern}.{ext}"), recursive=True)
        return sorted(hits)

    core = find("libduckdb_static") + find("duckdb_static")
    if len(core) != 1:
        fail(f"expected one duckdb_static library under {build}, found {core}")
    third_party = [
        lib
        for lib in find(os.path.join("third_party", "**", "*duckdb_*"))
        if not any(skip in os.path.basename(lib) for skip in THIRD_PARTY_SKIP)
    ]
    return core + third_party


def include_dirs(src: str) -> list[str]:
    """The engine's headers plus every third-party include directory."""
    dirs = [os.path.join(src, "src", "include")]
    third_party = os.path.join(src, "third_party")
    for name in sorted(os.listdir(third_party)):
        path = os.path.join(third_party, name)
        if not os.path.isdir(path):
            continue
        # As DuckDB's CMake does: a library with an `include/` directory is
        # reached through it alone. Its root holds files such as brotli's
        # `VERSION`, which a case-insensitive file system (macOS) returns for
        # `#include <version>`.
        include = os.path.join(path, "include")
        dirs.append(include if os.path.isdir(include) else path)
    return dirs


def render_manifest(
    engine_id: str, label: str, built: str, includes: list[str], libs: list[str]
) -> str:
    # `built` changes with every build of the libraries, so a rebuild (`--force`,
    # or a cache entry rebuilt in place) changes the manifest `build.rs` watches.
    lines = [f"id {engine_id}", f"version {label}", f"built {built}"]
    lines += [f"include {path}" for path in includes]
    lines += [f"lib {path}" for path in libs]
    return "\n".join(lines) + "\n"


def write_atomically(path: str, text: str) -> None:
    """Write `path` so that an interrupted write leaves the old file or none."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    partial = path + ".partial"
    with open(partial, "w", encoding="utf-8") as fh:
        fh.write(text)
    os.replace(partial, path)


def manifest_label(text: str) -> str | None:
    for line in text.splitlines():
        if line.startswith("version "):
            return line.removeprefix("version ")
    return None


def cached_manifest(stamp: str, engine_id: str) -> str | None:
    """
    The cache entry's manifest, or None when it is missing or unusable.

    It is written atomically, but an entry from an interrupted run of an older
    script, a damaged file, or one whose headers or libraries have since gone
    (pruned by hand, a moved checkout) must be rebuilt rather than handed to
    `build.rs`.
    """
    if not os.path.exists(stamp):
        return None
    with open(stamp, encoding="utf-8") as fh:
        text = fh.read()
    lines = text.splitlines()
    complete = (
        lines[:1] == [f"id {engine_id}"]
        and manifest_label(text) is not None
        and any(line.startswith("lib ") for line in lines)
        and all(
            os.path.exists(line.split(" ", 1)[1])
            for line in lines
            if line.startswith(("include ", "lib "))
        )
    )
    return text if complete else None


def install_manifest(root: str, text: str) -> bool:
    path = os.path.join(root, MANIFEST)
    if os.path.exists(path):
        with open(path, encoding="utf-8") as fh:
            if fh.read() == text:
                return False
    write_atomically(path, text)
    return True


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo-root", default=DEFAULT_ROOT, help="repository root")
    ap.add_argument("--version", help="target DuckDB version (default: .duckdb-version)")
    ap.add_argument(
        "--src-dir",
        default="duckdb",
        help="engine source tree to build; '' disables the source-tree path",
    )
    ap.add_argument("--force", action="store_true", help="rebuild even when cached")
    ap.add_argument("--sdist", help="a local duckdb sdist to build instead of PyPI's")
    args = ap.parse_args(argv)

    root = os.path.abspath(args.repo_root)
    for stale in STALE_AMALGAMATION:
        path = os.path.join(root, stale)
        if os.path.exists(path):
            os.remove(path)
            log(f"removed {stale}, left by a DuckDB 1.x (amalgamation) build")
    version = args.version or read_pin(root, ".duckdb-version")
    if not version:
        fail("no target version: pass --version or create .duckdb-version")

    tree = engine_source_tree(root, args.src_dir)
    env = dict(os.environ)
    for name in HARNESS_LABEL_VARS:
        env.pop(name, None)
    # A cross-compile builds a different engine from the same source.
    arch = os.environ.get("OSX_BUILD_ARCH", "").strip()
    target = f"-{arch}" if arch else ""
    # A tree with no commit has no identity to cache under, so it is rebuilt
    # every time rather than served a build of whatever was there before.
    cacheable = True
    if tree:
        commit = git_commit(tree)
        # Uncommitted edits are not part of the commit: such a tree is rebuilt
        # every time, under an id of its own so the commit's build is not
        # replaced by the edited one.
        dirty = commit is not None and git_dirty(tree)
        cacheable = commit is not None and not dirty
        label, evidence = tree_label(tree)
        if label:
            env["DUCKDB_VERSION"] = label
        state = "-dirty" if dirty else ""
        engine_id = f"src-{label or 'dev'}-{commit or 'unidentified'}{state}{target}-{LAYOUT}"
        log(
            f"engine source tree: {tree} (commit {commit or 'unknown'}"
            f"{', with uncommitted changes' if dirty else ''}; {evidence})"
        )
    else:
        env["DUCKDB_VERSION"] = version
        # The wheel pin is part of the id: correcting it must not be served the
        # engine built from the old one.
        pypi = pypi_version(root, version)
        wheel = "" if pypi == version.removeprefix("v") else f"-py{pypi}"
        engine_id = f"{version}{wheel}{target}-{LAYOUT}"
    cache = os.path.join(root, ".engine", engine_id)
    src = tree or os.path.join(cache, "src")
    stamp = os.path.join(cache, "manifest")

    manifest = cached_manifest(stamp, engine_id) if cacheable and not args.force else None
    if manifest is None:
        # Nothing an earlier, unfinished run left is trusted: its source may be
        # a partial extraction, and its build directory may hold a CMake cache
        # for another generator, which CMake refuses to configure over. The
        # entry's own manifest goes first, so a rebuild that fails part-way
        # (after removing the source) leaves no entry behind that looks finished.
        with contextlib.suppress(FileNotFoundError):
            os.remove(stamp)
        if not tree:
            shutil.rmtree(src, ignore_errors=True)
            extract_engine(pypi_version(root, version), src, args.sdist)
        build = os.path.join(cache, "build")
        shutil.rmtree(build, ignore_errors=True)
        label = configure_engine(src, build, env)
        # Checked before the (long) build: the label is settled at configure.
        # DuckDB's CMake stamps the DUCKDB_VERSION we set, so this only catches
        # a CMake that labels differently; it says nothing about whether the
        # sdist is the engine the pin names. That is check_sdist_version's job
        # for the archive, and the wheel pin's own test
        # (test/integration/test_engine_pins.py) for the pin pair.
        if not tree and label != version:
            fail(
                f"the engine in the duckdb {pypi_version(root, version)} sdist "
                f"labels itself {label}, but this extension targets {version} "
                "(.duckdb-version). Fix .duckdb-pypi-version or .duckdb-version."
            )
        build_engine(build, env)
        lib_dir = os.path.join(cache, "lib")
        shutil.rmtree(lib_dir, ignore_errors=True)
        os.makedirs(lib_dir)
        libs = []
        for lib in static_libraries(build):
            dest = os.path.join(lib_dir, os.path.basename(lib))
            shutil.copyfile(lib, dest)
            libs.append(dest)
        manifest = render_manifest(engine_id, label, str(time.time_ns()), include_dirs(src), libs)
        # The libraries are copied out, so the object files can go; the cache
        # keeps only what a link needs.
        shutil.rmtree(build, ignore_errors=True)
        write_atomically(stamp, manifest)
        log(f"cached {cache}")

    changed = install_manifest(root, manifest)
    label = manifest_label(manifest)
    log(f"{'installed' if changed else 'already current'}: {MANIFEST} ({label}, {engine_id})")
    if label != version:
        log(
            f"NOTE: this engine is {label}, not {version} (.duckdb-version); the "
            f"extension will load only in DuckDB {label}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
