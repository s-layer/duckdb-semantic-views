#!/usr/bin/env python3
"""
Install the Rust toolchain pinned in rust-toolchain.toml before cargo runs.

`rust-toolchain.toml` pins an exact release. On a machine that does not have
it, the first `cargo build` makes rustup download it lazily, once, with no
retry. The DuckDB community-extensions registry builds on fresh runners, so
every registry build takes that path, and a single network failure there fails
the platform and with it the whole release: the registry's osx_arm64 build of
v0.15.0 died on `dns error: failed to lookup address` while rustup synced the
channel, before anything of ours compiled. BuildAll and BuildQuick run the same
reusable workflow the registry does, so they take that path too; the workflows
that set up Rust with `.github/actions/setup-pinned-rust` already have it.

This does the same for every build that goes through the Makefile:

- Without rustup on PATH it does nothing: cargo comes from somewhere else, and
  the pin does not apply. Nor does it when `RUSTUP_TOOLCHAIN` or a
  `rustup override` for this directory picks another toolchain, since those
  take precedence over the file.
- When the pinned toolchain and its components are already installed it does
  nothing, with no network access (a plain `rustup toolchain install` would
  still sync the channel).
- Otherwise it installs them, retrying a failed attempt after a growing delay,
  and fails with rustup's last error once the attempts run out. Between
  attempts it clears a partial install, but only of a toolchain that was not
  installed before it started. One that only lacked a component stays, and
  the build goes on with it when the install keeps failing.

Environment, for tests: `SV_RUSTUP` names the rustup binary, and
`SV_TOOLCHAIN_RETRY_DELAY` sets the first delay in seconds (default 10).
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ATTEMPTS = 5


def read_pin(path: Path) -> tuple[str, list[str]]:
    """The pinned channel and its components, from rust-toolchain.toml."""
    text = path.read_text(encoding="utf-8")
    channel = re.search(r'^\s*channel\s*=\s*"([^"]+)"', text, re.M)
    if not channel:
        sys.exit(f"ensure_rust_toolchain: no channel in {path}")
    components = re.search(r"^\s*components\s*=\s*\[([^\]]*)\]", text, re.M)
    names = re.findall(r'"([^"]+)"', components.group(1)) if components else []
    return channel.group(1), names


def run(rustup: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([rustup, *args], capture_output=True, text=True)


def toolchain_listed(rustup: str, channel: str) -> bool:
    """Whether rustup has the toolchain installed, with or without components."""
    listed = run(rustup, "toolchain", "list")
    if listed.returncode != 0:
        return False
    names = [line.split()[0] for line in listed.stdout.splitlines() if line.strip()]
    # rustup lists a toolchain by its full name, `1.99.1-x86_64-unknown-linux-gnu`.
    return any(n == channel or n.startswith(channel + "-") for n in names)


def components_installed(rustup: str, channel: str, components: list[str]) -> bool:
    """Whether every component is installed for the toolchain."""
    if not components:
        return True
    have = run(rustup, "component", "list", "--installed", "--toolchain", channel)
    if have.returncode != 0:
        return False
    # Installed components are listed with the host suffix (`clippy-x86_64-…`).
    lines = have.stdout.split()
    return all(any(l == c or l.startswith(c + "-") for l in lines) for c in components)


def overridden(rustup: str, root: Path) -> str | None:
    """The toolchain a `rustup override` sets for `root` itself, if any.

    rustup walks up from the working directory and, in each directory, takes
    an override before a toolchain file. `root` holds rust-toolchain.toml, so
    only an override on `root` beats it; one on a parent is never reached.
    """
    listed = run(rustup, "override", "list")
    if listed.returncode != 0:
        return None
    for line in listed.stdout.splitlines():
        parts = line.split()
        if len(parts) < 2:
            continue
        path = Path(parts[0])
        if path == root:
            return parts[-1]
    return None


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    channel, components = read_pin(root / "rust-toolchain.toml")
    rustup = os.environ.get("SV_RUSTUP") or shutil.which("rustup")
    if not rustup:
        print("ensure_rust_toolchain: no rustup on PATH; using the cargo found there")
        return 0
    # Both take precedence over rust-toolchain.toml, so cargo would not use
    # the pinned toolchain even once it is installed.
    # (rustup itself sets RUSTUP_TOOLCHAIN for the tools it runs, so a value
    # naming the pin is no override.)
    chosen = os.environ.get("RUSTUP_TOOLCHAIN") or overridden(rustup, root)
    if chosen and chosen != channel and not chosen.startswith(channel + "-"):
        print(f"ensure_rust_toolchain: cargo uses Rust {chosen} here, not the pin; skipping")
        return 0
    had_toolchain = toolchain_listed(rustup, channel)
    if had_toolchain and components_installed(rustup, channel, components):
        return 0

    args = ["toolchain", "install", channel, "--profile", "minimal", "--no-self-update"]
    for c in components:
        args += ["--component", c]
    delay = float(os.environ.get("SV_TOOLCHAIN_RETRY_DELAY", "10"))
    for attempt in range(1, ATTEMPTS + 1):
        result = run(rustup, *args)
        if result.returncode == 0:
            print(f"ensure_rust_toolchain: installed Rust {channel}")
            return 0
        sys.stderr.write(result.stderr)
        if attempt == ATTEMPTS:
            break
        print(
            f"ensure_rust_toolchain: installing Rust {channel} failed "
            f"(attempt {attempt} of {ATTEMPTS}); retrying in {delay:g}s",
            file=sys.stderr,
        )
        # A failed install can leave a partial toolchain that makes the next
        # attempt fail too ("detected conflict"); clear it first, as
        # setup-pinned-rust does. A toolchain that was here before is not
        # ours to remove, and it still builds without the missing component.
        if not had_toolchain:
            run(rustup, "toolchain", "uninstall", channel)
        time.sleep(delay)
        delay *= 2
    print(
        f"ensure_rust_toolchain: could not install Rust {channel} after {ATTEMPTS} attempts",
        file=sys.stderr,
    )
    if had_toolchain:
        # Only a component is missing, and the build does not need one.
        print(
            f"ensure_rust_toolchain: building with the installed Rust {channel} anyway",
            file=sys.stderr,
        )
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
