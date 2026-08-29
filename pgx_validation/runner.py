"""Run jackc/pgx's pgconn + pgproto3 test packages against a SecantusPGServer.

Mechanics mirror ``psycopg_validation.runner``:

1. Spawn ``python -m secantus.sql.pgserver`` on a kernel-assigned ephemeral
   port over a throwaway storage dir.
2. Wait for the listener and verify it is SecantusDB.
3. Run ``go test -count=1 -json`` for the packages in
   ``include_paths.PACKAGES`` inside ``vendor/pgx``, with
   ``PGX_TEST_DATABASE`` pointing at the daemon. The JSON-lines stream lands
   in ``.validation/pgx-raw.json``; ``generate_report.py`` renders
   ``docs/validation-report-pgx.md``.

Run via ``uv run python -m invoke validate-pgx`` (requires ``go`` on PATH).
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "pgx"
RAW_OUT = REPO_ROOT / ".validation" / "pgx-raw.json"

GO_TEST_TIMEOUT = "600s"
SUBPROCESS_TIMEOUT_SECONDS = 1800.0


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
    from .include_paths import PACKAGES, SKIP_RUN

    if shutil.which("go") is None:
        print("the pgx gauge requires the Go toolchain (`go` on PATH)", file=sys.stderr)
        return 2
    if not (VENDOR / "pgconn").is_dir():
        print(
            "vendor/pgx is missing — run `git submodule update --init vendor/pgx`",
            file=sys.stderr,
        )
        return 2

    RAW_OUT.parent.mkdir(exist_ok=True)
    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-pgx-gauge-")

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
            "PGX_TEST_DATABASE": f"host={host} port={port} dbname=postgres user=postgres",
        }
        cmd = [
            "go",
            "test",
            "-count=1",
            f"-timeout={GO_TEST_TIMEOUT}",
            "-json",
        ]
        if SKIP_RUN:
            cmd += ["-skip", SKIP_RUN]
        cmd += PACKAGES
        with RAW_OUT.open("wb") as out:
            proc = subprocess.run(
                cmd, cwd=VENDOR, env=env, stdout=out, timeout=SUBPROCESS_TIMEOUT_SECONDS
            )
        return proc.returncode
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()


if __name__ == "__main__":
    raise SystemExit(main())
