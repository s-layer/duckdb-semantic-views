#!/usr/bin/env python3
"""
Run a command; if it dies of an access violation on Windows, run it again under cdb.

    windows_crash_report.py -- <command> [args...]

The sqllogictest runner is one Python process. When the extension crashes it on
Windows, all Python can report is `faulthandler`'s Python stack -- the native
frames, and which module (the DuckDB wheel or the extension) faulted, are lost.
So when the command exits with STATUS_ACCESS_VIOLATION and the Debugging Tools
for Windows are installed (they are on GitHub's Windows runners), this runs the
same command again under `cdb`, stopping at the first access violation to print
the registers, the native stack and the loaded modules, then killing the
process.

The exit status is always the first run's, so a crash never reads as green, and
the re-run is only a report. Anywhere else (another OS, another failure, no
cdb), this only runs the command and passes its status through.
"""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

# STATUS_ACCESS_VIOLATION as Windows reports it (unsigned), and as a signed int.
ACCESS_VIOLATION = {0xC0000005, 0xC0000005 - (1 << 32)}

CDB_CANDIDATES = [
    Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)"))
    / "Windows Kits/10/Debuggers/x64/cdb.exe",
    Path(os.environ.get("ProgramFiles", r"C:\Program Files"))
    / "Windows Kits/10/Debuggers/x64/cdb.exe",
]

# Executed at the debugger's first break. `sxi ibp` ignores the initial
# breakpoint of each child process (-o follows children: the venv's python.exe
# is a launcher that starts the real interpreter as one). `sxe -c ... av` breaks
# on the FIRST chance of an access violation -- before faulthandler's handler
# sees it -- prints what is needed and quits, which kills the debuggee.
ON_ACCESS_VIOLATION = "; ".join(
    [
        ".echo ===== SV CRASH REPORT =====",
        "r",
        ".reload",
        "kn 80",
        "ub @rip L8",
        "u @rip L4",
        "lm",
        ".echo ===== END SV CRASH REPORT =====",
        "q",
    ]
)
CDB_SCRIPT = f""".symfix
sxi ibp
sxe -c "{ON_ACCESS_VIOLATION}" av
g
"""

RERUN_TIMEOUT_S = 1800


def find_cdb():
    for candidate in CDB_CANDIDATES:
        if candidate.is_file():
            return candidate
    return None


def main(argv):
    if not argv or argv[0] != "--" or len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    command = argv[1:]
    sys.stdout.flush()
    rc = subprocess.call(command)
    if os.name != "nt" or rc not in ACCESS_VIOLATION:
        return rc

    print(f"\n{command[0]} died of an access violation (exit {rc:#x}).", flush=True)
    cdb = find_cdb()
    if cdb is None:
        print("cdb.exe not found; no native crash report.", flush=True)
        return rc

    with tempfile.TemporaryDirectory() as tmp:
        script = Path(tmp) / "crash.cdb"
        script.write_text(CDB_SCRIPT)
        print(f"Re-running it under {cdb} for the native stack:", flush=True)
        try:
            subprocess.call(
                [str(cdb), "-o", "-G", "-cf", str(script), *command], timeout=RERUN_TIMEOUT_S
            )
        except subprocess.TimeoutExpired:
            print(f"The re-run under cdb took over {RERUN_TIMEOUT_S} s; stopped.", flush=True)
    print(f"Exiting with the first run's status ({rc:#x}).", flush=True)
    return rc


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
