"""Cross-server point-in-time recovery via the standalone binary (Phase R, R6b).

A database written through the **Python** server is restored by the **Rust**
``secantusdb restore`` subcommand — proving the native Rust applier reads the
Python server's on-disk WiredTiger + oplog and rebuilds the database identically
(the reverse direction of ``test_rust_pitr_cross_server.py``'s R6a).

Skipped unless the binary is built (``cargo build --manifest-path
crates/secantusdb/Cargo.toml``, or set ``SECANTUSDB_BIN``).
"""

from __future__ import annotations

import contextlib
import os
import pathlib
import re
import signal
import subprocess
import sys
import tempfile
from collections.abc import Iterator
from typing import Any

try:
    import fcntl  # POSIX-only; the binary tests skip on Windows anyway.
except ImportError:  # pragma: no cover - Windows
    fcntl = None  # type: ignore[assignment]

import pytest

from tests.server_pipes import drain

# These tests are disk-I/O bound end-to-end (real durable WiredTiger in the
# binary, snapshot + restore): ~8s solo, but the documented contention factor
# under a full parallel suite is 15-35x (see _restore), and with TWO suites on
# one machine the global 600s deadline was reachable — pytest-timeout's
# thread-method kill then took the whole worker down (the "worker death"
# cluster). Budget for the measured worst case; genuine hangs still die, just
# later, and the banner read above fails fast with a reason.
# NOTE: applied in the combined ``pytestmark`` list below — a second bare
# ``pytestmark =`` assignment silently OVERWRITES the first (module attribute,
# last write wins), which is exactly how this timeout mark was lost and the
# global 600s thread-method timeout came back to os._exit workers
# ("Not properly terminated", no signal trace, no faulthandler dump).
# method="signal" needs SIGALRM (POSIX-only); Windows falls back to the
# thread method at the same 1200s budget — still overriding the global 600s.
_TIMEOUT_MARK = pytest.mark.timeout(
    1200, method="signal" if hasattr(signal, "SIGALRM") else "thread"
)
# method="signal": these tests run subprocess/pymongo calls on the worker's
# main thread, so SIGALRM can interrupt them — the timeout then FAILS THE
# TEST with a traceback instead of the global thread-method's os._exit,
# which kills the whole xdist worker ("Not properly terminated") and cost
# four "worker death" investigations before the mechanism was pinned.

pymongo = pytest.importorskip("pymongo")


_REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent


def _binary_path() -> pathlib.Path | None:
    env = os.environ.get("SECANTUSDB_BIN")
    if env:
        p = pathlib.Path(env)
        return p if p.exists() else None
    # Prefer the release binary: it is the shipped artifact, and a debug-build
    # restore is pathologically slow under the full parallel suite (~4x slower),
    # which can blow even a generous subprocess timeout. Fall back to debug.
    for profile in ("release", "debug"):
        p = _REPO_ROOT / "crates" / "secantusdb" / "target" / profile / "secantusd-mdb"
        if sys.platform == "win32":
            p = p.with_suffix(".exe")
        if p.exists():
            return p
    return None


_BIN = _binary_path()
pytestmark = [
    _TIMEOUT_MARK,
    pytest.mark.skipif(_BIN is None, reason="secantusdb binary not built"),
    # All of this file's tests schedule on ONE worker (--dist=loadgroup), so
    # the machine-wide serialization flock never contends WITHIN a suite —
    # cross-worker queuing on it is what starved tests to the fixture's 480s
    # deadline under full-suite disk contention. The flock stays to guard
    # against a parallel worktree session's suite.
    pytest.mark.xdist_group("rust_binary_serial"),
]

# Each test here spawns a full secantusd-mdb server plus a restore subprocess. Run
# concurrently across xdist workers, several heavy restores contend hard enough
# that one can blow its subprocess timeout — a contention artifact, not a real
# restore failure (each passes in isolation). Serialize them machine-wide with an
# advisory file lock (the same pattern the crash-watchdog nested tests use), so
# only one binary test runs at a time regardless of xdist dist mode.
_BINARY_SERIAL_LOCK = pathlib.Path(tempfile.gettempdir()) / "secantus-rust-binary-serial.lock"


@pytest.fixture(autouse=True)
def _serialize_binary_tests():
    # BOUNDED, non-blocking acquisition. The original blocking flock was the
    # xdist "worker death" root cause: with two suites sharing this
    # machine-wide lock, workers queued here for over 600s — and the GLOBAL
    # thread-method pytest-timeout (which governs fixture waits; the file's
    # 1200s signal marker does not) killed them via os._exit with the dump
    # swallowed by capture: "Not properly terminated", no traceback, no
    # crash report. Occurrences 1-6 in tasks/backlog.md all match — every
    # victim had been in this file's tests for exactly ~600s. A bounded
    # poll turns a starved wait into a NAMED failure at 480s, far inside
    # every timeout budget, and keeps the worker alive.
    if fcntl is None:
        yield
        return
    import time as _time

    deadline = _time.monotonic() + 480.0
    with open(_BINARY_SERIAL_LOCK, "w") as fh:
        while True:
            try:
                fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if _time.monotonic() > deadline:
                    raise AssertionError(
                        "timed out waiting 480s for the machine-wide rust-binary "
                        "test lock — another pytest run (parallel suite?) is "
                        "holding it; rerun when the machine is quieter"
                    ) from None
                _time.sleep(0.5)
        try:
            yield
        finally:
            fcntl.flock(fh.fileno(), fcntl.LOCK_UN)


@contextlib.contextmanager
def _rust_server(data: pathlib.Path, *extra: str) -> Iterator[pymongo.MongoClient]:
    """A `secantusd-mdb` on `data`, stopped (SIGTERM, a clean close) on exit.

    These tests used to write their data with the deprecated Python server;
    since the storage-model change (tasks/mongod-storage-model-plan.md,
    phase 1) a Rust server refuses a store in the earlier layout, which is
    the one the Python server writes. The restore paths they cover are the
    Rust binary's, so the data now comes from it too. `--data-logged`: the
    reads below are the Python `Storage`'s, which writes without timestamps
    (see the v2 test below).
    """
    assert _BIN is not None
    proc = subprocess.Popen(
        [str(_BIN), "--port", "0", "--storage-path", str(data), "--data-logged", *extra],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        line = _read_banner_line(proc)
        drain(proc)
        m = _BANNER.search(line)
        assert m, f"no listening banner in: {line!r}"
        client = pymongo.MongoClient(
            m.group(1),
            int(m.group(2)),
            directConnection=True,
            serverSelectionTimeoutMS=5000,
            socketTimeoutMS=120000,
        )
        try:
            yield client
        finally:
            client.close()
    finally:
        proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=30)


def _docs(path: pathlib.Path, db: str, coll: str) -> list[dict[str, Any]]:
    """`db.coll`'s documents in the store at `path`, read through the Rust
    binary (the deprecated Python `Storage` does not read mongod's layout)."""
    with _rust_server(path) as client:
        return sorted(client[db][coll].find({}), key=lambda d: d["_id"])


def _restore(
    source: pathlib.Path, target: pathlib.Path, *extra: str
) -> subprocess.CompletedProcess:
    assert _BIN is not None
    return subprocess.run(
        [str(_BIN), "restore", "--source", str(source), "--target-dir", str(target), *extra],
        capture_output=True,
        text=True,
        # The restore is disk-I/O bound (~7s with the release binary solo). Under
        # the full parallel suite it contends for disk I/O with the other xdist
        # workers' WiredTiger activity and sits in uninterruptible I/O wait —
        # always progressing (never hung; verified by sampling the process state),
        # historically 15-35x slower, with a measured ~90x outlier (600s cap
        # exceeded, 2026-08-17; the same run passed solo in 6.9s). Size for that
        # tail while staying inside the file's 1200s timeout marker; a genuine
        # hang still fails it by never completing.
        timeout=900,
    )


def test_rust_binary_restores_server_data(tmp_path: pathlib.Path) -> None:
    data = tmp_path / "data"
    with _rust_server(data) as client:
        coll = client["app"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
        coll.update_one({"_id": 1}, {"$set": {"v": 100, "tag": "x"}})
        coll.delete_one({"_id": 2})
        coll.insert_one({"_id": 3, "v": 3})
    # The server is stopped (WiredTiger lock released) on context exit.

    out = tmp_path / "restored"
    res = _restore(data, out)
    assert res.returncode == 0, f"stderr={res.stderr}\nstdout={res.stdout}"
    assert "Restored" in res.stdout
    assert _docs(out, "app", "c") == [{"_id": 1, "v": 100, "tag": "x"}, {"_id": 3, "v": 3}]


def test_rust_binary_restore_to_timestamp(tmp_path: pathlib.Path) -> None:
    data = tmp_path / "data"
    with _rust_server(data) as client:
        # Two statements, each its own oplog entry: a restore to the
        # timestamp of a batched insert's single `applyOps` entry applies
        # only its first document (RM-727).
        client["app"]["c"].insert_one({"_id": 1})
        client["app"]["c"].insert_one({"_id": 2})
        # The timestamp of the inserts: the last oplog entry carrying them.
        ts = [e for e in client["local"]["oplog.rs"].find() if _app_c_ops(e)][-1]["ts"]

    out = tmp_path / "restored"
    res = _restore(data, out, "--to-timestamp", f"{ts.time},{ts.inc}")
    assert res.returncode == 0, res.stderr
    assert _docs(out, "app", "c") == [{"_id": 1}, {"_id": 2}]


def test_rust_binary_restore_missing_source_errors(tmp_path: pathlib.Path) -> None:
    res = _restore(tmp_path / "does-not-exist", tmp_path / "out")
    assert res.returncode != 0


_BANNER = re.compile(r"secantusd-mdb listening on (\S+):(\d+)")


def _read_banner_line(proc: subprocess.Popen, timeout: float = 120.0) -> str:
    """Read the server's banner line with a deadline.

    A bare ``proc.stdout.readline()`` blocks forever if the binary wedges at
    startup, and the per-test 600s pytest-timeout then kills the WORKER
    (``method = "thread"`` exits via ``os._exit`` → xdist "Not properly
    terminated", no traceback, no crash report) — the 2026-08-14/15
    "worker death" cluster. Fail inside the test instead, with the reason.
    """
    import threading

    result: dict[str, str] = {}

    def _read() -> None:
        assert proc.stdout is not None
        result["line"] = proc.stdout.readline()

    t = threading.Thread(target=_read, daemon=True)
    t.start()
    t.join(timeout)
    if "line" not in result:
        raise AssertionError(
            f"secantusd-mdb printed no banner within {timeout}s "
            f"(pid={proc.pid}, alive={proc.poll() is None})"
        )
    return result["line"]


def test_rust_binary_v2_archive_base_snapshot_and_restore(tmp_path: pathlib.Path) -> None:
    """End-to-end PITR v2 on the Rust server: a server started with
    --oplog-archive-dir takes a base snapshot via secantusAdmin.archiveBaseSnapshot,
    and `secantusdb restore` auto-detects the archive directory and rebuilds the
    database (Phase R, R5c)."""
    assert _BIN is not None
    archive = tmp_path / "archive"
    proc = subprocess.Popen(
        [
            str(_BIN),
            "--port",
            "0",
            "--storage-path",
            str(tmp_path / "data"),
            "--oplog-archive-dir",
            str(archive),
            # The restore and the reads below are the Python server's, which
            # writes without timestamps: a store whose data tables are not
            # logged (secantusd-mdb's default since RM-210) refuses that.
            "--data-logged",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert proc.stdout is not None
        line = _read_banner_line(proc)
        drain(proc)
        m = _BANNER.search(line)
        assert m, f"no listening banner in: {line!r}"
        host, port = m.group(1), int(m.group(2))
        client = pymongo.MongoClient(
            host,
            port,
            directConnection=True,
            serverSelectionTimeoutMS=5000,
            # A wedged server must surface as a NAMED network timeout, not an
            # unbounded hang the per-test timeout kills silently (the last
            # un-fixed worker-death shape: the flock HOLDER dying mid-test).
            socketTimeoutMS=120000,
        )
        client["app"]["c"].insert_many([{"_id": 1}, {"_id": 2}, {"_id": 3}])
        reply = client["admin"].command(
            {"secantusAdmin.archiveBaseSnapshot": 1, "archiveDir": str(archive)}
        )
        assert reply["ok"] == 1.0
        assert "path" in reply
    finally:
        proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=30)

    # The archive dir holds a base snapshot; restore auto-detects it (v2).
    out = tmp_path / "restored"
    res = _restore(archive, out)
    assert res.returncode == 0, res.stderr
    assert _docs(out, "app", "c") == [{"_id": 1}, {"_id": 2}, {"_id": 3}]


def _app_c_ops(entry: dict[str, Any]) -> int:
    """How many operations on `app.c` an oplog entry carries: a batched insert
    is one `applyOps` entry holding each document's operation, as mongod
    8.2 logs it."""
    if entry.get("ns") == "app.c":
        return 1
    inner = entry.get("o", {}).get("applyOps", []) if entry.get("op") == "c" else []
    return sum(_app_c_ops(e) for e in inner)


def _restored_oplog_len(path: pathlib.Path) -> int:
    """How many operations on `app.c` the restored store's oplog holds, read
    through the Rust binary."""
    with _rust_server(path) as client:
        return sum(_app_c_ops(e) for e in client["local"]["oplog.rs"].find())


def test_rust_binary_preserve_oplog(tmp_path: pathlib.Path) -> None:
    """`--preserve-oplog` carries the replayed oplog onto the restored dir; the
    default leaves it empty (Phase R, R5b)."""
    data = tmp_path / "data"
    with _rust_server(data) as client:
        client["app"]["c"].insert_many([{"_id": 1}, {"_id": 2}, {"_id": 3}])

    carried = tmp_path / "carried"
    assert _restore(data, carried, "--preserve-oplog").returncode == 0
    assert _restored_oplog_len(carried) >= 3  # timeline preserved

    fresh = tmp_path / "fresh"
    assert _restore(data, fresh).returncode == 0
    assert _restored_oplog_len(fresh) == 0  # fresh timeline by default


def test_rust_binary_restore_keeps_a_capped_collections_options(tmp_path: pathlib.Path) -> None:
    """A capped collection with a validator, created through the Rust
    server, keeps its options and documents through `secantusd-mdb restore`.
    (This check went through the deprecated Python restore tool until
    2026-11-20; see tests/test_rust_pitr_cross_server.py.)"""
    data = tmp_path / "data"
    validator = {"$jsonSchema": {"required": ["k"]}}
    with _rust_server(data) as client:
        db = client["app"]
        db.create_collection("capped", capped=True, size=4096, max=3, validator=validator)
        db["capped"].insert_many([{"_id": i, "k": i} for i in range(5)])
    out = tmp_path / "restored"
    res = _restore(data, out)
    assert res.returncode == 0, res.stderr
    with _rust_server(out) as client:
        info = client["app"].command("listCollections", filter={"name": "capped"})["cursor"][
            "firstBatch"
        ]
        options = info[0]["options"]
        assert (options["capped"], options["size"], options["max"]) == (True, 4096, 3)
        assert options["validator"] == validator
        docs = sorted(d["_id"] for d in client["app"]["capped"].find())
    assert docs == [2, 3, 4]
