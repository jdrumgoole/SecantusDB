"""TEMPORARY diagnostic for the Windows psycopg gauge (batch 73).

Runs suspect psycopg tests one at a time, under a hard wall-clock limit,
against the reference PostgreSQL (``PGDIAG_REF_DSN``) and a freshly spawned
``secantusd-pg``, printing elapsed time, the pytest tail and the server's own
log tail. A hang here is reported, not fatal: pytest's faulthandler dumps the
stacks after 60 s and the subprocess limit ends it.
"""

from __future__ import annotations

import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
VENDOR = REPO / "vendor" / "psycopg"
BIN = REPO / "crates" / "secantus-pgserver" / "target" / "debug"
BIN = BIN / ("secantusd-pg.exe" if sys.platform == "win32" else "secantusd-pg")

TESTS = os.environ.get(
    "PGDIAG_TESTS", "tests/test_concurrency_async.py::test_type_error_shadow"
).split(",")
REF_DSN = os.environ.get("PGDIAG_REF_DSN", "host=127.0.0.1 port=5432 user=postgres dbname=postgres")
LIMIT = float(os.environ.get("PGDIAG_LIMIT", "240"))


def _port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _run(label: str, dsn: str, test: str) -> None:
    env = {**os.environ, "PSYCOPG_TEST_DSN": dsn}
    cmd = [
        sys.executable,
        "-m",
        "pytest",
        "-p",
        "no:xdist",
        "-p",
        "no:randomly",
        "-p",
        "no:benchmark",
        "-o",
        "timeout=0",
        "-o",
        "faulthandler_timeout=60",
        "-vv",
        "-s",
        "--no-header",
        test,
    ]
    t0 = time.monotonic()
    try:
        p = subprocess.run(cmd, cwd=VENDOR, env=env, capture_output=True, text=True, timeout=LIMIT)
        out, rc = p.stdout + p.stderr, p.returncode
    except subprocess.TimeoutExpired as e:
        out = (
            (e.stdout or b"").decode(errors="replace")
            if isinstance(e.stdout, bytes)
            else (e.stdout or "")
        )
        out += (
            (e.stderr or b"").decode(errors="replace")
            if isinstance(e.stderr, bytes)
            else (e.stderr or "")
        )
        rc = "TIMEOUT"
    dt = time.monotonic() - t0
    print(f"===== {label} {test}: rc={rc} elapsed={dt:.1f}s")
    print("\n".join(out.splitlines()[-80:]))


def main() -> None:
    for test in TESTS:
        _run("POSTGRES", REF_DSN, test)
        port = _port()
        store = tempfile.mkdtemp(prefix="pgdiag-")
        log = open(Path(store).with_suffix(".log"), "w+")  # noqa: SIM115 -- closed with the server
        srv = subprocess.Popen([str(BIN), store, f"127.0.0.1:{port}"], stdout=log, stderr=log)
        time.sleep(3)
        try:
            _run("SECANTUSD-PG", f"host=127.0.0.1 port={port} user=postgres dbname=postgres", test)
        finally:
            srv.terminate()
            srv.wait(timeout=30)
            log.seek(0)
            print("----- server log tail")
            print("\n".join(log.read().splitlines()[-40:]))


if __name__ == "__main__":
    main()
