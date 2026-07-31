"""Run the pgbench + psql smoke against a fresh SecantusPGServer daemon.

Writes ``.validation/sqlstress-raw.json`` (per-lane tps / status) which
``generate_report.py`` renders into ``docs/validation-report-sqlstress.md``.
Exit 0 only when every lane runs error-free.
"""

from __future__ import annotations

import json
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RAW_OUT = REPO_ROOT / ".validation" / "sqlstress-raw.json"

LANE_TIMEOUT_SECONDS = 600.0

#: The psql catalog smoke: every command must succeed (`ON_ERROR_STOP=1`).
PSQL_SCRIPT = "\\dt\n\\d pgbench_accounts\n\\di\n\\l\n\\dn\nSELECT count(*) FROM pgbench_accounts;\n"


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
            f"(version(): {version!r}) — refusing to run the smoke against it"
        )


def _tps(output: str) -> float | None:
    m = re.search(r"tps = ([0-9.]+)", output)
    return float(m.group(1)) if m else None


def main() -> int:
    for tool in ("pgbench", "psql"):
        if shutil.which(tool) is None:
            print(f"the sql-stress smoke requires `{tool}` on PATH", file=sys.stderr)
            return 2

    RAW_OUT.parent.mkdir(exist_ok=True)
    host = "127.0.0.1"
    port = _pick_ephemeral_port()
    storage_dir = tempfile.mkdtemp(prefix="secantus-sqlstress-")
    conn_args = ["-h", host, "-p", str(port), "-U", "postgres", "-d", "postgres"]

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
    lanes: list[dict] = []
    try:
        _wait_for_listener(host, port)
        _verify_secantus_identity(host, port)

        def run_lane(name: str, cmd: list[str], stdin: str | None = None) -> bool:
            proc = subprocess.run(
                cmd,
                input=stdin,
                capture_output=True,
                text=True,
                timeout=LANE_TIMEOUT_SECONDS,
            )
            out = proc.stdout + proc.stderr
            ok = proc.returncode == 0 and "fatal" not in out.lower()
            lanes.append(
                {
                    "lane": name,
                    "ok": ok,
                    "tps": _tps(out),
                    "detail": None if ok else out.strip().splitlines()[-1][:200],
                }
            )
            print(f"{'ok ' if ok else 'FAIL'} {name}" + (f"  tps={_tps(out)}" if _tps(out) else ""))
            return ok

        all_ok = run_lane("init (-i -s 1)", ["pgbench", *conn_args, "-i", "-s", "1", "-q"])
        for mode in ("simple", "extended", "prepared"):
            all_ok &= run_lane(
                f"tpcb -M {mode} (c=1 t=50)",
                ["pgbench", *conn_args, "-n", "-c", "1", "-t", "50", "-M", mode],
            )
        all_ok &= run_lane(
            "select-only (c=4 t=100)",
            ["pgbench", *conn_args, "-n", "-S", "-c", "4", "-t", "100"],
        )
        all_ok &= run_lane(
            "psql catalog smoke",
            ["psql", *conn_args, "-v", "ON_ERROR_STOP=1", "-q"],
            stdin=PSQL_SCRIPT,
        )
        RAW_OUT.write_text(json.dumps({"lanes": lanes}, indent=2))
        return 0 if all_ok else 1
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()


if __name__ == "__main__":
    raise SystemExit(main())
