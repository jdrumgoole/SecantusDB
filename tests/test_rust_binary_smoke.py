"""Smoke test for the standalone ``secantusdb`` Rust server binary (R7).

Launches the compiled binary on an ephemeral port, reads the bound address
from its stdout banner, drives a pymongo handshake + CRUD round-trip over
real TCP, then SIGTERMs it and asserts a clean exit.

Skipped unless the binary exists: build it with
``cargo build --manifest-path crates/secantusdb/Cargo.toml`` (WiredTiger
required — set SECANTUS_WT_INCLUDE / SECANTUS_WT_LIB), or point
``SECANTUSDB_BIN`` at a prebuilt binary. ``invoke rust-binary-test`` does
both.
"""

from __future__ import annotations

import os
import pathlib
import re
import signal
import subprocess
import sys

import pytest

pymongo = pytest.importorskip("pymongo")

_REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
_BANNER = re.compile(r"secantusdb listening on (\S+):(\d+)")


def _binary_path() -> pathlib.Path | None:
    env = os.environ.get("SECANTUSDB_BIN")
    if env:
        p = pathlib.Path(env)
        return p if p.exists() else None
    for profile in ("debug", "release"):
        p = _REPO_ROOT / "crates" / "secantusdb" / "target" / profile / "secantusdb"
        if sys.platform == "win32":
            p = p.with_suffix(".exe")
        if p.exists():
            return p
    return None


_BIN = _binary_path()
pytestmark = pytest.mark.skipif(
    _BIN is None,
    reason="secantusdb binary not built (cargo build --manifest-path "
    "crates/secantusdb/Cargo.toml, or set SECANTUSDB_BIN)",
)


@pytest.fixture
def daemon(tmp_path: pathlib.Path) -> subprocess.Popen[str]:
    assert _BIN is not None
    proc = subprocess.Popen(
        [str(_BIN), "--port", "0", "--storage-path", str(tmp_path / "data")],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        yield proc
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=10)


def _bound_address(proc: subprocess.Popen[str]) -> tuple[str, int]:
    assert proc.stdout is not None
    line = proc.stdout.readline()
    m = _BANNER.search(line)
    assert m, f"no listening banner in first stdout line: {line!r}"
    return m.group(1), int(m.group(2))


def test_binary_serves_pymongo_and_exits_cleanly(
    daemon: subprocess.Popen[str],
) -> None:
    host, port = _bound_address(daemon)
    client: pymongo.MongoClient = pymongo.MongoClient(
        host=host, port=port, serverSelectionTimeoutMS=10_000
    )
    try:
        assert client.admin.command("ping")["ok"] == 1.0
        hello = client.admin.command("hello")
        assert hello["setName"] == "secantus"

        col = client.smoke.things
        col.insert_many([{"_id": i, "n": i * 2} for i in range(5)])
        assert col.count_documents({}) == 5
        assert sorted(d["_id"] for d in col.find({"n": {"$gt": 4}})) == [3, 4]
        col.update_one({"_id": 0}, {"$set": {"n": 99}})
        assert col.find_one({"_id": 0})["n"] == 99
        (total,) = col.aggregate([{"$group": {"_id": None, "s": {"$sum": "$n"}}}])
        assert total["s"] == 99 + 2 + 4 + 6 + 8
    finally:
        client.close()

    daemon.send_signal(signal.SIGTERM)
    assert daemon.wait(timeout=15) == 0, daemon.stderr.read() if daemon.stderr else ""


def test_standalone_flag_drops_replica_set(tmp_path: pathlib.Path) -> None:
    assert _BIN is not None
    proc = subprocess.Popen(
        [
            str(_BIN),
            "--port",
            "0",
            "--storage-path",
            str(tmp_path / "data"),
            "--standalone",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        host, port = _bound_address(proc)
        client: pymongo.MongoClient = pymongo.MongoClient(
            host=host, port=port, serverSelectionTimeoutMS=10_000, directConnection=True
        )
        try:
            assert "setName" not in client.admin.command("hello")
        finally:
            client.close()
        proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=15) == 0
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=10)


def test_bad_args_exit_2() -> None:
    assert _BIN is not None
    res = subprocess.run([str(_BIN), "--bogus"], capture_output=True, text=True, timeout=30)
    assert res.returncode == 2
    assert "--bogus" in res.stderr


def test_help_exits_zero() -> None:
    assert _BIN is not None
    res = subprocess.run([str(_BIN), "--help"], capture_output=True, text=True, timeout=30)
    assert res.returncode == 0
    assert "--storage-path" in res.stdout
