"""Run psycopg 3's vendored test suite against a SecantusPGServer daemon.

The SQL analogue of the thirteen MongoDB driver gauges: psycopg's own tests
run **unmodified** (vendored at the exact version pinned in the ``dev`` extra)
against a real ``SecantusPGServer`` over TCP. The runner:

1. Spawns ``python -m secantus.sql.pgserver --host 127.0.0.1 --port <picked>
   --storage-path <tempdir>`` as a subprocess on a kernel-assigned ephemeral
   port (so gauges can run in parallel).
2. Waits for the listener, then verifies it is SecantusDB (``select
   version()`` must name it — a real Postgres on the port would silently
   inflate the numbers).
3. Runs the vendored pytest suite with ``PSYCOPG_TEST_DSN`` pointing at the
   daemon, one xdist worker (the suite shares databases; serial semantics,
   but a pytest-timeout kill only takes out the worker so the JSON report
   survives hangs).
4. ``generate_report.py`` renders ``docs/validation-report-psycopg.md``.

Run via ``uv run python -m invoke validate-psycopg``. Needs only the project
venv — psycopg[binary] ships libpq, and the suite's plugins (anyio,
pytest-randomly, pytest-cov) ride the ``dev`` extra.
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
VENDOR = REPO_ROOT / "vendor" / "psycopg"
RAW_OUT = REPO_ROOT / ".validation" / "psycopg-raw.json"

#: Wall-clock cap on the pytest invocation. The full sync half is ~28s against
#: a healthy server; a broken change leaves hung awaits, and per-test
#: ``timeout=20`` plus this backstop keeps a bad run bounded.
PYTEST_TIMEOUT_SECONDS = 1800.0


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
    """Abort unless the daemon at ``host:port`` is SecantusDB.

    A stray real Postgres (or another session's daemon) on the picked port
    would make every test pass vacuously."""
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
    from .include_paths import DESELECT_TESTS, INCLUDE

    if not (VENDOR / "tests").is_dir():
        print(
            "vendor/psycopg is missing — run `git submodule update --init vendor/psycopg`",
            file=sys.stderr,
        )
        return 2

    RAW_OUT.parent.mkdir(exist_ok=True)
    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-psycopg-gauge-")

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
            "PSYCOPG_TEST_DSN": f"host={host} port={port} user=postgres dbname=postgres",
        }
        deselect = [f"--deselect={t}" for t in DESELECT_TESTS]
        # Run from VENDOR so psycopg's own conftest/config apply (this is
        # their suite, unmodified; only the DSN points at us). No xdist: the
        # suite shares databases (serial by design), their session hooks
        # misbehave inside a worker, and the per-test timeout below contains
        # hangs well enough for a ~1-minute suite.
        cmd = [
            sys.executable,
            "-m",
            "pytest",
            "-p",
            "no:xdist",
            "-o",
            "timeout=20",
            # (their conftest uses config.cache, so no:cacheprovider must
            # NOT be disabled — .pytest_cache is in their .gitignore.)
            # psycopg's config treats warnings as errors, and pytest-benchmark
            # (a dev-extra plugin of OURS, not theirs) warns whenever xdist is
            # active — keep it out of their run entirely.
            "-p",
            "no:benchmark",
            "--continue-on-collection-errors",
            "--json-report",
            f"--json-report-file={RAW_OUT}",
            "--no-header",
            "--tb=no",
            "-q",
            *deselect,
            *INCLUDE,
        ]
        proc = subprocess.run(cmd, cwd=VENDOR, env=env, timeout=PYTEST_TIMEOUT_SECONDS)
        return proc.returncode
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()


if __name__ == "__main__":
    raise SystemExit(main())
