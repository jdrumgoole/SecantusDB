"""Run SQLAlchemy's dialect-compliance suite against a SecantusPGServer daemon.

Mechanics mirror ``psycopg_validation.runner``:

1. Spawn ``python -m secantus.sql.pgserver`` on a kernel-assigned ephemeral
   port over a throwaway storage dir.
2. Wait for the listener, verify it is SecantusDB (a real Postgres on the
   port would silently inflate the numbers).
3. Run pytest in ``sqlalchemy_validation/suite/`` — the three-file
   compliance-suite bootstrap SQLAlchemy documents for third-party dialects —
   with ``--dburi postgresql+psycopg://…`` pointing at the daemon, capability
   declarations from ``requirements.py``, JSON report to
   ``.validation/sqlalchemy-raw.json``.
4. ``generate_report.py`` renders ``docs/validation-report-sqlalchemy.md``.

Run via ``uv run python -m invoke validate-sqlalchemy``. Needs only the
project venv — sqlalchemy + psycopg[binary] ride the ``dev`` extra.
"""

from __future__ import annotations

import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SUITE_DIR = Path(__file__).resolve().parent / "suite"
RAW_OUT = REPO_ROOT / ".validation" / "sqlalchemy-raw.json"

#: The suite is ~1,600 tests and runs in a few minutes against a healthy
#: server; per-test ``timeout=30`` plus this backstop contains hangs.
PYTEST_TIMEOUT_SECONDS = 2400.0


def _pick_ephemeral_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_listener(host: str, port: int, timeout: float = 15.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"pg daemon at {host}:{port} did not become ready within {timeout}s")


def _verify_secantus_identity(host: str, port: int) -> None:
    import psycopg

    with psycopg.connect(
        host=host, port=port, dbname="postgres", user="postgres", connect_timeout=5
    ) as conn:
        (version,) = conn.execute("select version()").fetchone()
    if "SecantusDB" not in version:
        raise RuntimeError(
            f"daemon at {host}:{port} does not identify as SecantusDB "
            f"(version(): {version!r}) — refusing to run the gauge against it"
        )


def main() -> int:
    from .include_paths import DESELECT_TESTS

    RAW_OUT.parent.mkdir(exist_ok=True)
    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-sqlalchemy-gauge-")

    daemon = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "secantus.sql.pgserver",
            "--host",
            host,
            "--port",
            str(port),
            "--storage-path",
            storage_dir,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        _wait_for_listener(host, port)
        _verify_secantus_identity(host, port)

        env = {
            **os.environ,
            # requirement_cls in setup.cfg imports from the repo root; keep
            # any caller PYTHONPATH (a bare worktree venv reaches secantus
            # via PYTHONPATH=src) behind it.
            "PYTHONPATH": os.pathsep.join(
                filter(None, [str(REPO_ROOT), os.environ.get("PYTHONPATH", "")])
            ),
        }
        deselect = [f"--deselect={t}" for t in DESELECT_TESTS]
        # No xdist: the suite provisions shared schemas per class (serial by
        # design). pytest-benchmark warns under their warnings-as-errors
        # config, same as the psycopg gauge — keep it out of the run.
        cmd = [
            sys.executable,
            "-m",
            "pytest",
            "-p",
            "no:xdist",
            "-p",
            "no:benchmark",
            "-p",
            "no:randomly",
            "-o",
            "timeout=30",
            "--dburi",
            f"postgresql+psycopg://postgres@{host}:{port}/postgres",
            "--continue-on-collection-errors",
            "--json-report",
            f"--json-report-file={RAW_OUT}",
            "--no-header",
            "--tb=no",
            "-q",
            *deselect,
            "test_suite.py",
        ]
        proc = subprocess.run(cmd, cwd=SUITE_DIR, env=env, timeout=PYTEST_TIMEOUT_SECONDS)
        return proc.returncode
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()


if __name__ == "__main__":
    raise SystemExit(main())
