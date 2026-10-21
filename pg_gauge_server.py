"""Which PostgreSQL server a PG gauge drives: the Python server (the default,
deprecated) or the Rust one, chosen by ``SECANTUS_GAUGE_SERVER`` the way
the MongoDB gauges and the psycopg / sqllogictest gauges already choose.
The weekly ``validate.yml`` lanes measure the Rust one.

The Rust server's results go to a ``-rust-server`` sibling of the gauge's
raw report, so a Rust run never overwrites the Python server's published
numbers, and a binary built from a different ``crates/`` tree is refused
(``tools.provenance``) -- a stale build once put a wrong number on the live
website.
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent

RUST_BINARY = (
    REPO_ROOT
    / "crates"
    / "secantus-pg"
    / "target"
    / "debug"
    / ("secantusd-pg.exe" if sys.platform == "win32" else "secantusd-pg")
)


def which() -> str:
    """``python`` (default) or ``rust``."""
    server = os.environ.get("SECANTUS_GAUGE_SERVER", "python").lower()
    if server not in ("python", "rust"):
        raise SystemExit(f"SECANTUS_GAUGE_SERVER must be 'python' or 'rust', got {server!r}")
    return server


def raw_out(path: Path) -> Path:
    """``path`` for the Python server, its ``-rust-server`` sibling for Rust."""
    if which() == "python":
        return path
    return path.with_name(f"{path.stem}-rust-server{path.suffix}")


def is_rust_report(report: str | Path) -> bool:
    """Whether ``report`` is a Rust-server report: its name ends ``-rust-server``,
    the suffix every Rust-server raw and report carries."""
    return Path(report).stem.endswith("-rust-server")


def rust_version() -> str:
    """The Rust PostgreSQL server's version, from its crate's ``Cargo.toml``
    (its own version line, not the MongoDB server's)."""
    toml = (REPO_ROOT / "crates" / "secantus-pg" / "Cargo.toml").read_text()
    m = re.search(r'^version\s*=\s*"([^"]+)"', toml, re.MULTILINE)
    return m.group(1) if m else "unknown"


def server_line(report: str | Path, python_version: str) -> str:
    """The "which server was measured" line a report prints, chosen by the
    report's own name, so a Rust run can never be labelled the Python server."""
    if is_rust_report(report):
        return f"SecantusDB (Rust PostgreSQL server, `secantusd-pg`) {rust_version()}"
    return f"SecantusDB (Python server) {python_version}"


def daemon_argv(host: str, port: int, storage_dir: str) -> list[str]:
    """The command that starts the selected server on ``host:port``."""
    if which() == "rust":
        if not RUST_BINARY.exists():
            raise SystemExit(
                f"{RUST_BINARY.relative_to(REPO_ROOT)} not built -- run "
                "`uv run python -m invoke rust-pgserver-build`"
            )
        if str(REPO_ROOT) not in sys.path:
            sys.path.insert(0, str(REPO_ROOT))
        from tools.provenance import require_fresh_pgserver

        require_fresh_pgserver(RUST_BINARY, repo_root=REPO_ROOT)
        return [str(RUST_BINARY), storage_dir, f"{host}:{port}"]
    return [
        sys.executable,
        "-m",
        "secantus.sql.pgserver",
        "--host",
        host,
        "--port",
        str(port),
        "--storage-path",
        storage_dir,
    ]
