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
_BANNER = re.compile(r"secantusd-rs listening on (\S+):(\d+)")


def _binary_path() -> pathlib.Path | None:
    env = os.environ.get("SECANTUSDB_BIN")
    if env:
        p = pathlib.Path(env)
        # Windows: the caller may hand us a path without the `.exe` suffix —
        # `command -v secantusd-rs` under Git Bash does exactly that.
        if not p.exists() and sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")
        if not p.exists():
            # Deliberately fatal, not a skip. Setting SECANTUSDB_BIN means "smoke
            # THIS artifact" — it is how CI points the suite at the binary it is
            # about to publish. Degrading to a skip there lets a release ship a
            # binary that was never exercised, with the step still green: exactly
            # what happened when the Windows lane was first enabled and pytest
            # reported `4 skipped` under a passing checkmark. An unresolvable
            # path is a caller bug, so fail loudly.
            raise RuntimeError(
                f"SECANTUSDB_BIN={env!r} does not exist"
                + (f" (nor {p})" if str(p) != env else "")
                + ". Point it at a built secantusd-rs binary, or unset it to let "
                "the suite discover one under crates/secantusdb/target/."
            )
        return p
    # Prefer the release binary: it is the shipped artifact and much faster than
    # a debug build under the full parallel suite. Fall back to debug.
    for profile in ("release", "debug"):
        p = _REPO_ROOT / "crates" / "secantusdb" / "target" / profile / "secantusd-rs"
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


# Graceful shutdown is signalled differently on Windows. `Popen.send_signal`
# maps SIGTERM to TerminateProcess there — an immediate kill that exits 1 and
# runs no handler, so the clean-exit these tests assert could never happen. The
# binary uses the `ctrlc` crate (with `termination`), which on Windows installs
# a console control handler, so CTRL_BREAK_EVENT reaches the same shutdown path
# SIGTERM takes on Unix. It is delivered to a process GROUP, hence the
# CREATE_NEW_PROCESS_GROUP flag below — without it the break would also hit the
# pytest process running the test.
_WINDOWS = sys.platform == "win32"
_SPAWN_KWARGS: dict[str, object] = (
    {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if _WINDOWS else {}
)


def _request_shutdown(proc: subprocess.Popen[str]) -> None:
    """Ask the daemon to stop the way a user would, per platform."""
    proc.send_signal(signal.CTRL_BREAK_EVENT if _WINDOWS else signal.SIGTERM)


@pytest.fixture
def daemon(tmp_path: pathlib.Path) -> subprocess.Popen[str]:
    assert _BIN is not None
    proc = subprocess.Popen(
        [str(_BIN), "--port", "0", "--storage-path", str(tmp_path / "data")],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        **_SPAWN_KWARGS,
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

    _request_shutdown(daemon)
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
        **_SPAWN_KWARGS,
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
        _request_shutdown(proc)
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
