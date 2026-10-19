"""
Shared helpers for DuckLake integration tests.

Provides common boilerplate for loading the semantic_views extension and
attaching a DuckLake catalog. Used by both test_ducklake.py (local) and
test_ducklake_ci.py (CI).
"""

import os
import sys
from pathlib import Path


def get_project_root() -> Path:
    """Return the project root directory (3 levels up from this file)."""
    # test/integration/test_ducklake_helpers.py -> test/integration -> test -> project root
    return Path(__file__).resolve().parent.parent.parent


def get_ext_dir() -> str:
    """Return the project-local extension directory, creating it if needed."""
    ext_dir = get_project_root() / "test" / "data" / "duckdb_extensions"
    ext_dir.mkdir(parents=True, exist_ok=True)
    return str(ext_dir)


def exit_skipped_if_offline(what: str, error: Exception) -> None:
    """
    Offline-mode escape hatch for a DuckDB extension that cannot be downloaded.

    Called when installing a downloadable extension (e.g. ``ducklake``) failed.
    With ``SV_OFFLINE_EXTENSIONS=1`` -- the same opt-in the Makefile's
    sqllogictest target honours (see the Makefile's OFFLINE_EXTENSION_TESTS)
    -- print a loud SKIPPED line and exit 0, so a sandbox that cannot
    reach extensions.duckdb.org can still run the rest of ``just test-all``.

    Otherwise return, and the caller re-raises: without the variable the test
    fails exactly as before. The variable is refused when ``CI`` is set, so CI
    can never turn this coverage off.
    """
    if os.environ.get("SV_OFFLINE_EXTENSIONS") != "1":
        return
    if os.environ.get("CI"):
        print(
            "ERROR: SV_OFFLINE_EXTENSIONS=1 skips tests and is for local offline runs only; "
            f"refusing because CI={os.environ['CI']!r}",
            file=sys.stderr,
        )
        sys.exit(1)
    print(
        f"SKIPPED (SV_OFFLINE_EXTENSIONS=1): {what} -- could not install a DuckDB extension "
        f"from extensions.duckdb.org; NOT run. ({str(error).splitlines()[0]})"
    )
    sys.exit(0)


def get_extension_path() -> Path:
    """
    Return the semantic_views extension path.

    Checks SEMANTIC_VIEWS_EXTENSION_PATH environment variable first.
    Falls back to the CMake debug build path.
    """
    env_path = os.environ.get("SEMANTIC_VIEWS_EXTENSION_PATH")
    if env_path:
        return Path(env_path)
    return get_project_root() / "build" / "debug" / "semantic_views.duckdb_extension"


def load_extension(con, extension_path: Path) -> None:
    """
    Install and load the semantic_views extension plus DuckLake.

    Args:
        con: A duckdb.DuckDBPyConnection instance.
        extension_path: Path to the semantic_views .duckdb_extension file.
    """
    # FORCE INSTALL ensures the freshly-built binary overwrites any stale cached copy
    # in the project-local extension directory.
    con.execute(f"FORCE INSTALL '{extension_path}'")
    con.execute("LOAD semantic_views")
    con.execute("LOAD ducklake")


def attach_ducklake(con, ducklake_file: str, data_dir: str, alias: str = "jaffle") -> None:
    """
    Attach a DuckLake catalog to an existing connection.

    Args:
        con: A duckdb.DuckDBPyConnection instance.
        ducklake_file: Path to the .ducklake metadata file.
        data_dir: Path to the data directory. Must end with '/'.
        alias: The catalog alias to use in SQL (default: 'jaffle').
    """
    con.execute(f"ATTACH 'ducklake:{ducklake_file}' AS {alias} (DATA_PATH '{data_dir}')")
