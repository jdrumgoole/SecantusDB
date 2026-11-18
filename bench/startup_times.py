"""Startup time: mongod vs secantusd-mdb, PostgreSQL vs secantusd-pg.

Measures how long each server takes from process spawn until a real client
gets an answer, in two scenarios:

- **cold**: an empty data directory. For PostgreSQL this includes
  ``initdb``, because ``secantusd-pg`` creates its own store on first start
  and ``postgres`` cannot start without one. ``initdb`` is also recorded on
  its own.
- **warm**: a restart over a data directory a previous start created and
  shut down cleanly.

"Ready" means:

- **MongoDB servers:** a fresh pymongo client gets ``hello`` with
  ``isWritablePrimary: true``. ``secantusd-mdb`` runs in its default mode, a
  one-member replica set. ``mongod`` is measured twice: as a one-member
  replica set (the like-for-like row; a cold start includes
  ``replSetInitiate`` and the election) and standalone.
- **PostgreSQL servers:** a fresh psycopg connection runs ``SELECT 1``.

Each row is the median of ``--reps`` runs, with min and max. With
``--record`` the run is appended as one JSON line to
``bench/results/startup_history.jsonl``, with the date, the host, each
binary's version and the repository commit, so the numbers can be tracked
over time. Compare history rows only from the same host.

    uv run python -m bench.startup_times
    uv run python -m bench.startup_times --reps 10 --record

Binaries: ``--mdb-bin`` / ``--pg-bin`` (or ``SECANTUSD_MDB_BIN`` /
``SECANTUSD_PG_BIN``); otherwise the release builds under
``crates/*/target/release``. ``mongod``, ``postgres`` and ``initdb`` come
from ``PATH`` or ``--mongod-bin`` / ``--pg-bindir``.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
HISTORY = REPO_ROOT / "bench" / "results" / "startup_history.jsonl"
DEFAULT_REPS = 5
TIMEOUT_S = 60.0
POLL_S = 0.005


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _find_binary(env: str, explicit: str | None, crate: str, name: str) -> str | None:
    for cand in (explicit, os.environ.get(env)):
        if cand:
            return cand
    for p in (
        REPO_ROOT / "crates" / crate / "target" / "release" / name,
        REPO_ROOT / "crates" / "target" / "release" / name,
    ):
        if p.exists():
            return str(p)
    return shutil.which(name)


def _version(argv: list[str]) -> str:
    try:
        out = subprocess.run(argv, capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.TimeoutExpired):
        return "unknown"
    return out.strip().splitlines()[0] if out.strip() else "unknown"


def _wait(check: Callable[[], bool], proc: subprocess.Popen, what: str) -> None:
    deadline = time.perf_counter() + TIMEOUT_S
    while time.perf_counter() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"{what} exited with {proc.returncode} before it was ready")
        try:
            if check():
                return
        except Exception:  # noqa: BLE001 - not ready yet; any client error means retry
            pass
        time.sleep(POLL_S)
    raise RuntimeError(f"{what} not ready after {TIMEOUT_S:.0f}s")


def _stop(proc: subprocess.Popen) -> None:
    """Shut down cleanly (SIGTERM), so a warm start reopens a clean store."""
    if proc.poll() is None:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=TIMEOUT_S)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
            raise RuntimeError(f"pid {proc.pid} ignored SIGTERM for {TIMEOUT_S:.0f}s") from None
    if proc.returncode not in (0, -signal.SIGTERM):
        raise RuntimeError(f"pid {proc.pid} exited with {proc.returncode} on shutdown")


# -- readiness checks ---------------------------------------------------------


def _mongo_primary(port: int) -> Callable[[], bool]:
    """One client, created once the port listens, polled with ``hello``.

    A client per attempt is wrong here: pymongo waits ~500 ms before retrying
    a refused connection, which put a 500 ms step into every result.
    """
    from pymongo import MongoClient

    client: list[MongoClient] = []

    def check() -> bool:
        if not client:
            client.append(
                MongoClient(
                    f"mongodb://127.0.0.1:{port}/?directConnection=true",
                    serverSelectionTimeoutMS=TIMEOUT_S * 1000,
                    heartbeatFrequencyMS=500,
                )
            )
        try:
            ok = bool(client[0].admin.command("hello").get("isWritablePrimary"))
        except Exception:
            client.pop().close()
            raise
        if ok:
            client.pop().close()
        return ok

    return check


def _pg_select1(port: int) -> Callable[[], bool]:
    import psycopg

    def check() -> bool:
        with psycopg.connect(
            host="127.0.0.1", port=port, user="postgres", dbname="postgres", connect_timeout=1
        ) as c:
            return c.execute("SELECT 1").fetchone() == (1,)

    return check


# -- one start of each server ---------------------------------------------------


def _listening(port: int) -> Callable[[], bool]:
    def check() -> bool:
        with socket.create_connection(("127.0.0.1", port), timeout=0.2):
            return True

    return check


def _start(
    argv: list[str],
    port: int,
    log: Path,
    ready: Callable[[], bool],
    what: str,
    after_listen: Callable[[], None] | None = None,
) -> tuple[float, subprocess.Popen]:
    t0 = time.perf_counter()
    with open(log, "ab") as fh:
        proc = subprocess.Popen(argv, stdout=fh, stderr=subprocess.STDOUT)
    try:
        _wait(_listening(port), proc, what)
        if after_listen is not None:
            after_listen()
        _wait(ready, proc, what)
    except BaseException:
        proc.kill()
        proc.wait()
        raise
    return (time.perf_counter() - t0) * 1000, proc


def run_mdb(binary: str, data: Path, cold: bool, port: int) -> float:
    argv = [binary, "--port", str(port), "--storage-path", str(data)]
    ms, proc = _start(argv, port, data.parent / "mdb.log", _mongo_primary(port), "secantusd-mdb")
    _stop(proc)
    return ms


def run_mongod(binary: str, data: Path, cold: bool, replset: bool, port: int) -> float:
    data.mkdir(exist_ok=True)
    argv = [
        binary,
        "--bind_ip",
        "127.0.0.1",
        "--port",
        str(port),
        "--dbpath",
        str(data),
        "--logpath",
        str(data.parent / "mongod.log"),
        "--logappend",
    ]
    if replset:
        argv += ["--replSet", "startup"]
    initiate = None
    if replset and cold:
        from pymongo import MongoClient

        def initiate() -> None:
            deadline = time.perf_counter() + TIMEOUT_S
            while True:
                try:
                    with MongoClient(
                        f"mongodb://127.0.0.1:{port}/?directConnection=true",
                        serverSelectionTimeoutMS=200,
                    ) as c:
                        c.admin.command(
                            "replSetInitiate",
                            {
                                "_id": "startup",
                                "members": [{"_id": 0, "host": f"127.0.0.1:{port}"}],
                            },
                        )
                        return
                except Exception:  # noqa: BLE001 - not listening yet
                    if time.perf_counter() > deadline:
                        raise
                    time.sleep(POLL_S)

    ms, proc = _start(
        argv, port, data.parent / "mongod.log", _mongo_primary(port), "mongod", initiate
    )
    _stop(proc)
    return ms


def run_pg(binary: str, data: Path, cold: bool, port: int) -> float:
    argv = [binary, str(data), f"127.0.0.1:{port}"]
    ms, proc = _start(argv, port, data.parent / "pg.log", _pg_select1(port), "secantusd-pg")
    _stop(proc)
    return ms


def run_postgres(bindir: Path, data: Path, cold: bool, port: int) -> tuple[float, float]:
    """Return (total ms, initdb ms). initdb is 0 on a warm start."""
    t0 = time.perf_counter()
    init_ms = 0.0
    if cold:
        subprocess.run(
            [
                str(bindir / "initdb"),
                "-D",
                str(data),
                "-U",
                "postgres",
                "--auth=trust",
                "--no-sync",
                "-E",
                "UTF8",
                "--locale=C",
            ],
            check=True,
            capture_output=True,
        )
        init_ms = (time.perf_counter() - t0) * 1000
    argv = [
        str(bindir / "postgres"),
        "-D",
        str(data),
        "-p",
        str(port),
        "-c",
        "listen_addresses=127.0.0.1",
        "-k",
        "",
    ]
    ms, proc = _start(argv, port, data.parent / "postgres.log", _pg_select1(port), "postgres")
    _stop(proc)
    return ms + init_ms, init_ms


# -- driver ---------------------------------------------------------------------


def _summary(xs: list[float]) -> dict:
    return {
        "median_ms": round(statistics.median(xs), 1),
        "min_ms": round(min(xs), 1),
        "max_ms": round(max(xs), 1),
        "runs_ms": [round(x, 1) for x in xs],
    }


def measure(
    reps: int, mdb: str | None, pg: str | None, mongod: str | None, pg_bindir: Path | None
) -> dict:
    servers: dict[str, Callable[[Path, bool, int], float | tuple[float, float]]] = {}
    versions: dict[str, str] = {}
    if mongod:
        servers["mongod (replica set)"] = lambda d, c, p: run_mongod(mongod, d, c, True, p)
        servers["mongod (standalone)"] = lambda d, c, p: run_mongod(mongod, d, c, False, p)
        versions["mongod"] = _version([mongod, "--version"])
    if mdb:
        servers["secantusd-mdb"] = lambda d, c, p: run_mdb(mdb, d, c, p)
        versions["secantusd-mdb"] = _version([mdb, "--version"])
    if pg_bindir:
        servers["postgres"] = lambda d, c, p: run_postgres(pg_bindir, d, c, p)
        versions["postgres"] = _version([str(pg_bindir / "postgres"), "--version"])
    if pg:
        servers["secantusd-pg"] = lambda d, c, p: run_pg(pg, d, c, p)
        versions["secantusd-pg"] = _version([pg, "--version"])

    results: dict[str, dict] = {name: {} for name in servers}
    initdb: list[float] = []
    # Interleave servers inside each rep, so drift on the machine lands on all.
    raw: dict[str, dict[str, list[float]]] = {n: {"cold": [], "warm": []} for n in servers}
    for rep in range(reps):
        for name, fn in servers.items():
            with tempfile.TemporaryDirectory(prefix="startup-") as tmp:
                data = Path(tmp) / "data"
                # The same port for cold and warm: a replica set's config
                # names its member by host:port.
                port = _free_port()
                for scenario in ("cold", "warm"):
                    out = fn(data, scenario == "cold", port)
                    if isinstance(out, tuple):
                        out, init_ms = out
                        if scenario == "cold":
                            initdb.append(init_ms)
                    raw[name][scenario].append(out)
        print(f"  rep {rep + 1}/{reps} done", file=sys.stderr)
    for name in servers:
        results[name] = {s: _summary(v) for s, v in raw[name].items()}
    if initdb:
        results["postgres"]["initdb"] = _summary(initdb)
    return {"servers": results, "versions": versions}


def _git_commit() -> str:
    try:
        return subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def _host() -> str:
    """CPU model and core count, not the machine's name, so history can be shared."""
    cpu = platform.processor() or platform.machine()
    if platform.system() == "Darwin":
        cpu = _version(["sysctl", "-n", "machdep.cpu.brand_string"])
    elif Path("/proc/cpuinfo").exists():
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    return f"{platform.system()} {platform.machine()}, {cpu}, {os.cpu_count()} cores"


def _table(run: dict) -> str:
    rows = [
        "| server | cold start (median, min–max) | warm start (median, min–max) |",
        "| --- | ---: | ---: |",
    ]
    for name, r in run["servers"].items():
        cells = []
        for s in ("cold", "warm"):
            x = r[s]
            cells.append(f"{x['median_ms']:.0f} ms ({x['min_ms']:.0f}–{x['max_ms']:.0f})")
        rows.append(f"| {name} | {cells[0]} | {cells[1]} |")
    if "initdb" in run["servers"].get("postgres", {}):
        rows.append(
            f"\npostgres cold start includes initdb: median "
            f"{run['servers']['postgres']['initdb']['median_ms']:.0f} ms"
        )
    return "\n".join(rows)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--reps", type=int, default=DEFAULT_REPS)
    ap.add_argument("--mdb-bin")
    ap.add_argument("--pg-bin")
    ap.add_argument("--mongod-bin", default=shutil.which("mongod"))
    ap.add_argument("--pg-bindir", help="directory holding postgres and initdb")
    ap.add_argument("--record", action="store_true", help=f"append the run to {HISTORY.name}")
    ap.add_argument("--json", action="store_true", help="print the run as JSON")
    args = ap.parse_args(argv)

    mdb = _find_binary("SECANTUSD_MDB_BIN", args.mdb_bin, "secantusdb", "secantusd-mdb")
    pg = _find_binary("SECANTUSD_PG_BIN", args.pg_bin, "secantus-pg", "secantusd-pg")
    pg_bindir = Path(args.pg_bindir) if args.pg_bindir else None
    if pg_bindir is None and shutil.which("postgres"):
        pg_bindir = Path(shutil.which("postgres")).parent
    missing = [
        n
        for n, v in (
            ("secantusd-mdb", mdb),
            ("secantusd-pg", pg),
            ("mongod", args.mongod_bin),
            ("postgres", pg_bindir),
        )
        if not v
    ]
    if missing:
        print(f"not found, so not measured: {', '.join(missing)}", file=sys.stderr)

    load_before = round(os.getloadavg()[0], 2)
    run = measure(args.reps, mdb, pg, args.mongod_bin, pg_bindir)
    run = {
        "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "host": _host(),
        "load_avg_1m": {"before": load_before, "after": round(os.getloadavg()[0], 2)},
        "commit": _git_commit(),
        "reps": args.reps,
        **run,
    }
    print(json.dumps(run, indent=2) if args.json else _table(run))
    if args.record:
        with HISTORY.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(run, separators=(",", ":")) + "\n")
        print(f"\nrecorded in {HISTORY.relative_to(REPO_ROOT)}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
