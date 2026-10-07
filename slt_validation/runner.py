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
#: The Rust PostgreSQL server, driven when SECANTUS_GAUGE_SERVER=rust (as the
#: psycopg gauge does). `.exe` on Windows, where the bare name never exists.
RUST_BINARY = (
    REPO_ROOT
    / "crates"
    / "secantus-pg"
    / "target"
    / "debug"
    / ("secantusd-pg.exe" if sys.platform == "win32" else "secantusd-pg")
)


def _which_server() -> str:
    which = os.environ.get("SECANTUS_GAUGE_SERVER", "python").lower()
    if which not in ("python", "rust"):
        raise SystemExit(f"SECANTUS_GAUGE_SERVER must be 'python' or 'rust', got {which!r}")
    return which


def _raw_out(which: str) -> Path:
    """A separate report per server, so a Rust run never overwrites the
    Python server's published numbers."""
    return RAW_OUT if which == "python" else RAW_OUT.with_name("slt-raw-rust-server.json")


def _daemon_argv(which: str, host: str, port: int, storage_dir: str) -> list[str]:
    if which == "rust":
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


#: Per-file wall-clock cap. The slowest included file (select3.test) is ~40s
#: locally; a hang (a regressed awaitless wait, a runaway join) gets cut well
#: before it stalls the gauge. Env-overridable because the 2-core CI runner
#: executes the same files 5-10x slower than a dev machine — the first weekly
#: CI run timed out ~15 postgres-extended files at 300s that pass locally in
#: ~25s each (validate.yml sets 900 for its slt lane).
FILE_TIMEOUT_SECONDS = float(os.environ.get("SECANTUS_SLT_FILE_TIMEOUT", "300"))


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


def _run_file(slt: str, test_file: Path, engine: str = "postgres") -> dict:
    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-slt-gauge-")
    daemon = subprocess.Popen(
        _daemon_argv(_which_server(), host, port, storage_dir),
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
                    engine,
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
    which = _which_server()
    raw_out = _raw_out(which)
    if which == "rust":
        if not RUST_BINARY.exists():
            print(
                f"{RUST_BINARY.relative_to(REPO_ROOT)} not built — run "
                "`uv run python -m invoke rust-pgserver-build`",
                file=sys.stderr,
            )
            return 2
        # Refuse a binary built from a different crates/ tree (the psycopg
        # gauge once published a number from a stale build).
        sys.path.insert(0, str(REPO_ROOT))
        from tools.provenance import require_fresh_pgserver

        require_fresh_pgserver(RUST_BINARY, repo_root=REPO_ROOT)

    if CORPUS.exists():
        shutil.rmtree(CORPUS)
    RAW_OUT.parent.mkdir(exist_ok=True)
    preprocess_files(VENDOR, CORPUS, INCLUDE)

    # Two protocol lanes from one corpus (sql-gauges-plan §3 G1):
    # sqllogictest-rs speaks the simple protocol as ``postgres`` and the
    # extended protocol (Parse/Bind/Execute) as ``postgres-extended``.
    results: dict[str, dict] = {}
    total = len(INCLUDE) * 2
    n = 0
    for engine in ("postgres", "postgres-extended"):
        for rel in INCLUDE:
            n += 1
            res = _run_file(slt, CORPUS / rel, engine=engine)
            res["engine"] = engine
            results[f"{engine}:{rel}"] = res
            status = "PASS" if res["ok"] else "FAIL"
            print(f"[{n}/{total}] {status} [{engine}] {rel} ({res['seconds']}s)", flush=True)
    raw_out.write_text(json.dumps(results, indent=1))
    npass = sum(1 for r in results.values() if r["ok"])
    print(f"\n{npass}/{len(results)} lane-files pass — raw results in {raw_out}")
    return 0


if __name__ == "__main__":
    os.environ.setdefault("PYTHONUNBUFFERED", "1")
    raise SystemExit(main())
