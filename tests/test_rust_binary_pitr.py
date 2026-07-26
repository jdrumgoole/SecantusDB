"""Cross-server point-in-time recovery via the standalone binary (Phase R, R6b).

A database written through the **Python** server is restored by the **Rust**
``secantusdb restore`` subcommand — proving the native Rust applier reads the
Python server's on-disk WiredTiger + oplog and rebuilds the database identically
(the reverse direction of ``test_rust_pitr_cross_server.py``'s R6a).

Skipped unless the binary is built (``cargo build --manifest-path
crates/secantusdb/Cargo.toml``, or set ``SECANTUSDB_BIN``).
"""

from __future__ import annotations

import os
import pathlib
import re
import signal
import subprocess
import sys
import tempfile
from typing import Any

try:
    import fcntl  # POSIX-only; the binary tests skip on Windows anyway.
except ImportError:  # pragma: no cover - Windows
    fcntl = None  # type: ignore[assignment]

import pytest

pymongo = pytest.importorskip("pymongo")

from secantus import SecantusDBServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402

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
        p = _REPO_ROOT / "crates" / "secantusdb" / "target" / profile / "secantusd-rs"
        if sys.platform == "win32":
            p = p.with_suffix(".exe")
        if p.exists():
            return p
    return None


_BIN = _binary_path()
pytestmark = pytest.mark.skipif(_BIN is None, reason="secantusdb binary not built")

# Each test here spawns a full secantusd-rs server plus a restore subprocess. Run
# concurrently across xdist workers, several heavy restores contend hard enough
# that one can blow its subprocess timeout — a contention artifact, not a real
# restore failure (each passes in isolation). Serialize them machine-wide with an
# advisory file lock (the same pattern the crash-watchdog nested tests use), so
# only one binary test runs at a time regardless of xdist dist mode.
_BINARY_SERIAL_LOCK = pathlib.Path(tempfile.gettempdir()) / "secantus-rust-binary-serial.lock"


@pytest.fixture(autouse=True)
def _serialize_binary_tests():
    if fcntl is None:
        yield
        return
    with open(_BINARY_SERIAL_LOCK, "w") as fh:
        fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(fh.fileno(), fcntl.LOCK_UN)


def _docs(path: pathlib.Path, db: str, coll: str) -> list[dict[str, Any]]:
    s = Storage(str(path), enable_oplog=True)
    try:
        return sorted(s.find_matching(db, coll, {}), key=lambda d: d["_id"])
    finally:
        s.close()


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
        # just 15-35x slower. Size the timeout for that worst case; a genuine hang
        # still fails it by never completing.
        timeout=600,
    )


def test_rust_binary_restores_python_server_data(tmp_path: pathlib.Path) -> None:
    data = tmp_path / "pydata"
    with SecantusDBServer(port=0, storage_path=str(data)) as srv:
        coll = pymongo.MongoClient(srv.uri, directConnection=True)["app"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
        coll.update_one({"_id": 1}, {"$set": {"v": 100, "tag": "x"}})
        coll.delete_one({"_id": 2})
        coll.insert_one({"_id": 3, "v": 3})
    # Python server is stopped (WiredTiger lock released) on context exit.

    out = tmp_path / "restored"
    res = _restore(data, out)
    assert res.returncode == 0, f"stderr={res.stderr}\nstdout={res.stdout}"
    assert "Restored" in res.stdout
    assert _docs(out, "app", "c") == [{"_id": 1, "v": 100, "tag": "x"}, {"_id": 3, "v": 3}]


def test_rust_binary_restore_to_timestamp(tmp_path: pathlib.Path) -> None:
    data = tmp_path / "pydata"
    with SecantusDBServer(port=0, storage_path=str(data)) as srv:
        coll = pymongo.MongoClient(srv.uri, directConnection=True)["app"]["c"]
        coll.insert_many([{"_id": 1}, {"_id": 2}])

    # The timestamp after the two inserts, read from the stopped data dir.
    s = Storage(str(data), enable_oplog=True)
    try:
        tail = s.oplog_tail_seq()
        ts = s.read_oplog(start_seq=tail, limit=1)[0][1]["ts"]
    finally:
        s.close()

    out = tmp_path / "restored"
    res = _restore(data, out, "--to-timestamp", f"{ts.time},{ts.inc}")
    assert res.returncode == 0, res.stderr
    assert _docs(out, "app", "c") == [{"_id": 1}, {"_id": 2}]


def test_rust_binary_restore_missing_source_errors(tmp_path: pathlib.Path) -> None:
    res = _restore(tmp_path / "does-not-exist", tmp_path / "out")
    assert res.returncode != 0


_BANNER = re.compile(r"secantusd-rs listening on (\S+):(\d+)")


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
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert proc.stdout is not None
        m = _BANNER.search(proc.stdout.readline())
        assert m, "no listening banner"
        host, port = m.group(1), int(m.group(2))
        client = pymongo.MongoClient(
            host, port, directConnection=True, serverSelectionTimeoutMS=5000
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


def _restored_oplog_len(path: pathlib.Path) -> int:
    s = Storage(str(path), enable_oplog=True)
    try:
        return len(s.read_oplog(start_seq=1, limit=1000))
    finally:
        s.close()


def test_rust_binary_preserve_oplog(tmp_path: pathlib.Path) -> None:
    """`--preserve-oplog` carries the replayed oplog onto the restored dir; the
    default leaves it empty (Phase R, R5b)."""
    data = tmp_path / "pydata"
    with SecantusDBServer(port=0, storage_path=str(data)) as srv:
        coll = pymongo.MongoClient(srv.uri, directConnection=True)["app"]["c"]
        coll.insert_many([{"_id": 1}, {"_id": 2}, {"_id": 3}])

    carried = tmp_path / "carried"
    assert _restore(data, carried, "--preserve-oplog").returncode == 0
    assert _restored_oplog_len(carried) >= 3  # timeline preserved

    fresh = tmp_path / "fresh"
    assert _restore(data, fresh).returncode == 0
    assert _restored_oplog_len(fresh) == 0  # fresh timeline by default
