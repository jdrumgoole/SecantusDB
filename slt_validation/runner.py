"""Run the sqllogictest corpus against a SecantusPGServer daemon, one per file.

The SQL analogue of the driver gauges (tasks/sql-gauges-plan.md G1): the
SQLite-originated sqllogictest corpus, vendored pristine at
``vendor/sqllogictest``, executed by `sqllogictest-rs
<https://github.com/risinglightdb/sqllogictest-rs>`_ over real pgwire. The
runner:

1. Preprocesses the included files into ``.validation/slt-corpus/``
   (``slt_validation.preprocess`` — the corpus itself is never modified).
2. Per file: spawns ``python -m secantus.sql.pgserver`` on a kernel-assigned
   ephemeral port with a fresh temp storage dir (corpus files assume a clean
   database), verifies the daemon is SecantusDB, and runs
   ``sqllogictest --engine postgres`` against it.
3. Writes ``.validation/slt-raw.json`` (per-file ok / seconds / first error);
   ``generate_report.py`` renders ``docs/validation-report-slt.md``.

Requires the ``sqllogictest`` binary (``cargo install sqllogictest-bin``).
Run via ``uv run python -m invoke validate-slt``. Python server only — the
Rust server has no SQL front end.
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENDOR = REPO_ROOT / "vendor" / "sqllogictest" / "test"
CORPUS = REPO_ROOT / ".validation" / "slt-corpus"
RAW_OUT = REPO_ROOT / ".validation" / "slt-raw.json"

#: Per-file wall-clock cap. The slowest included file (select3.test) is ~40s;
#: a hang (a regressed awaitless wait, a runaway join) gets cut well before
#: it stalls the gauge.
FILE_TIMEOUT_SECONDS = 300.0


def _sqllogictest_bin() -> str:
    exe = shutil.which("sqllogictest") or str(Path.home() / ".cargo" / "bin" / "sqllogictest")
    if not Path(exe).exists():
        print(
            "the sqllogictest runner is missing — install it with "
            "`cargo install sqllogictest-bin` (needs a Rust toolchain)",
            file=sys.stderr,
        )
        raise SystemExit(2)
    return exe


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
            time.sleep(0.05)
    raise RuntimeError(f"pg daemon at {host}:{port} did not become ready within {timeout}s")


def _verify_secantus_identity(host: str, port: int) -> None:
    """Abort unless the daemon at ``host:port`` is SecantusDB — a stray real
    Postgres on the picked port would inflate the numbers."""
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


def _run_file(slt: str, test_file: Path) -> dict:
    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-slt-gauge-")
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
        t0 = time.monotonic()
        try:
            proc = subprocess.run(
                [
                    slt,
                    "--engine",
                    "postgres",
                    "--host",
                    host,
                    "--port",
                    str(port),
                    "--db",
                    "postgres",
                    "--user",
                    "postgres",
                    str(test_file),
                ],
                capture_output=True,
                text=True,
                timeout=FILE_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            return {"ok": False, "seconds": FILE_TIMEOUT_SECONDS, "error": "TIMEOUT"}
        seconds = round(time.monotonic() - t0, 2)
        if proc.returncode == 0:
            return {"ok": True, "seconds": seconds, "error": ""}
        tail = (proc.stdout + "\n" + proc.stderr).strip().split("\n")
        return {"ok": False, "seconds": seconds, "error": "\n".join(tail[-25:])}
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
        shutil.rmtree(storage_dir, ignore_errors=True)


def main() -> int:
    from .include_paths import INCLUDE
    from .preprocess import preprocess_files

    if not VENDOR.is_dir():
        print(
            "vendor/sqllogictest is missing — run "
            "`git submodule update --init vendor/sqllogictest`",
            file=sys.stderr,
        )
        return 2
    slt = _sqllogictest_bin()

    if CORPUS.exists():
        shutil.rmtree(CORPUS)
    RAW_OUT.parent.mkdir(exist_ok=True)
    preprocess_files(VENDOR, CORPUS, INCLUDE)

    results: dict[str, dict] = {}
    for idx, rel in enumerate(INCLUDE, 1):
        res = _run_file(slt, CORPUS / rel)
        results[rel] = res
        status = "PASS" if res["ok"] else "FAIL"
        print(f"[{idx}/{len(INCLUDE)}] {status} {rel} ({res['seconds']}s)", flush=True)
    RAW_OUT.write_text(json.dumps(results, indent=1))
    npass = sum(1 for r in results.values() if r["ok"])
    print(f"\n{npass}/{len(INCLUDE)} files pass — raw results in {RAW_OUT}")
    return 0


if __name__ == "__main__":
    os.environ.setdefault("PYTHONUNBUFFERED", "1")
    raise SystemExit(main())
