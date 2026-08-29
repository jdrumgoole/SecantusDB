"""Phase A′ hard-kill recovery harness: log-only-the-oplog mode.

Under ``SECANTUS_DATA_NONLOGGED=1`` the Rust server's data tables are
checkpoint-durable only; acknowledged writes survive a hard crash via
replay of the (WAL-logged) oplog from the stable-checkpoint marker at the
next open. These tests prove that contract end to end: a subprocess drives
acknowledged inserts through pymongo and is ``SIGKILL``ed mid-load; the
store is then reopened in-process and every acknowledged ``_id`` must be
present, exactly once. Env is subprocess-scoped, so the default suite's
stores are untouched.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

pymongo = pytest.importorskip("pymongo")
pytest.importorskip("_secantus_server")

#: Driver script run in a subprocess: opens the store in data-nonlogged mode,
#: inserts docs one acked batch at a time, appending each acked batch's ids to
#: ACK_FILE only AFTER the insert_many returned (an ack the server sent).
_WRITER = """
import sys
import _secantus_server, pymongo

path, ack_file, port_file = sys.argv[1], sys.argv[2], sys.argv[3]
# sync_on_commit: every WT commit fsyncs the WAL, so an ACK implies the
# oplog entry is durably on disk — making "every acked write survives
# kill -9" an exact contract. (Without it, acks ride the j:false default
# and a hard kill can lose the unsynced WAL tail in ANY mode — the
# pre-existing durability contract, not an A-prime property.)
srv = _secantus_server.RustServer(
    path, 0, host="127.0.0.1", replica_set_name="secantus", sync_on_commit=True
)
host, port = srv.address
with open(port_file, "w") as f:
    f.write(f"{host}:{port}")
client = pymongo.MongoClient(host, port, directConnection=True)
coll = client["crashdb"]["c"]
i = 0
with open(ack_file, "a", buffering=1) as acks:
    while True:
        batch = [{"_id": i + k, "v": (i + k) * 2} for k in range(50)]
        coll.insert_many(batch, ordered=True)
        acks.write(f"{i}:{i + 50}\\n")  # acked half-open range
        i += 50
"""


def _run_killed_load(tmp_path: Path, *, run_seconds: float, checkpoint_seconds: str) -> list[range]:
    """Drive the writer subprocess, SIGKILL it mid-load, return acked ranges."""
    store = tmp_path / "wt"
    ack_file = tmp_path / "acks.txt"
    port_file = tmp_path / "port.txt"
    env = {
        **os.environ,
        "SECANTUS_DATA_NONLOGGED": "1",
        "SECANTUS_CHECKPOINT_SECONDS": checkpoint_seconds,
    }
    proc = subprocess.Popen(
        [sys.executable, "-c", _WRITER, str(store), str(ack_file), str(port_file)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    try:
        deadline = time.monotonic() + 30
        while not port_file.exists() and time.monotonic() < deadline:
            if proc.poll() is not None:
                raise AssertionError(
                    f"writer died during startup: {proc.stderr.read().decode()[-2000:]}"
                )
            time.sleep(0.1)
        assert port_file.exists(), "server never came up"
        time.sleep(run_seconds)  # let acked load accumulate (and checkpoints fire)
        # Load floor: the fixed sleep alone is a race on a saturated box — the
        # 12-worker suite (or a second session's run) can starve the writer so
        # badly that the sleep elapses before a single acked batch reaches the
        # ack file, and the `assert ranges` below fires with no bug anywhere
        # (seen once as a release-gate flake, 2026-07-31). Keep the sleep (the
        # checkpoint-cadence tests need wall time for the periodic checkpoint
        # to fire), then hold the kill until real acks exist.
        floor_deadline = time.monotonic() + 60
        while time.monotonic() < floor_deadline:
            if ack_file.exists() and ack_file.read_text().count("\n") >= 5:
                break
            if proc.poll() is not None:
                raise AssertionError(f"writer died mid-load: {proc.stderr.read().decode()[-2000:]}")
            time.sleep(0.1)
    finally:
        proc.send_signal(signal.SIGKILL)
        proc.wait(timeout=10)

    ranges = []
    if ack_file.exists():
        for line in ack_file.read_text().splitlines():
            lo, hi = line.split(":")
            ranges.append(range(int(lo), int(hi)))
    assert ranges, (
        "no acknowledged batches before the kill — writer starved past the "
        f"60s load floor; stderr: {proc.stderr.read().decode()[-2000:]}"
    )
    return ranges


def _verify_all_present(tmp_path: Path, ranges: list[range]) -> None:
    """Reopen the killed store (replay-on-open) and check every acked _id."""
    import _secantus_server

    env_backup = os.environ.get("SECANTUS_DATA_NONLOGGED")
    os.environ["SECANTUS_DATA_NONLOGGED"] = "1"
    try:
        srv = _secantus_server.RustServer(
            str(tmp_path / "wt"), 0, host="127.0.0.1", replica_set_name="secantus"
        )
        try:
            host, port = srv.address
            client = pymongo.MongoClient(host, port, directConnection=True)
            coll = client["crashdb"]["c"]
            acked = [i for r in ranges for i in r]
            # Chunked verification: one giant $in over ~100k ids is an abusive
            # query shape (and slow regardless of server); 1000-id chunks ride
            # the _id index and keep the harness fast.
            present: set[int] = set()
            for lo in range(0, len(acked), 1000):
                chunk = acked[lo : lo + 1000]
                present.update(d["_id"] for d in coll.find({"_id": {"$in": chunk}}, {"_id": 1}))
            missing = [i for i in acked if i not in present]
            if missing:
                # Self-diagnose before failing: is the oplog tail absent at
                # reopen (WAL loss) or present-but-not-replayed (replay bug)?
                diag: dict[str, object] = {"n_missing": len(missing)}
                try:
                    diag["count_documents"] = coll.count_documents({})
                    op = client["local"]["oplog.rs"]
                    diag["oplog_rows"] = op.count_documents({})
                    last = list(op.find({}).sort("$natural", -1).limit(3))
                    diag["oplog_tail"] = [
                        {k: e.get(k) for k in ("op", "ns", "ts")}
                        | {"o_id": e.get("o", {}).get("_id")}
                        for e in last
                    ]
                    first_missing_in_oplog = op.count_documents({"o._id": missing[0]})
                    diag["first_missing_in_oplog"] = first_missing_in_oplog
                    diag["max_present"] = max(present) if present else None
                except Exception as exc:  # diagnostics must never mask the assert
                    diag["diag_error"] = repr(exc)
                assert not missing, (
                    f"{len(missing)} acknowledged docs lost after hard kill + replay "
                    f"(first: {missing[:10]}); diag={diag}"
                )
            # Exactly-once: no duplicate _ids possible in one collection, but the
            # doc bodies must match what was acked (replay applied cleanly).
            sample = coll.find_one({"_id": acked[-1]})
            assert sample["v"] == acked[-1] * 2
            client.close()
        finally:
            srv.stop()
    finally:
        if env_backup is None:
            os.environ.pop("SECANTUS_DATA_NONLOGGED", None)
        else:
            os.environ["SECANTUS_DATA_NONLOGGED"] = env_backup


def test_hard_kill_mid_load_recovers_every_acked_write(tmp_path):
    """SIGKILL mid-load with frequent checkpoints: the replay gap is small and
    every acknowledged write survives."""
    ranges = _run_killed_load(tmp_path, run_seconds=4.0, checkpoint_seconds="1")
    _verify_all_present(tmp_path, ranges)


def test_hard_kill_before_any_checkpoint_recovers_from_genesis(tmp_path):
    """SIGKILL before the first periodic checkpoint ever fires: the stable
    marker is absent/zero, the data tables recover empty, and the ENTIRE load
    must come back from oplog replay alone."""
    ranges = _run_killed_load(tmp_path, run_seconds=3.0, checkpoint_seconds="3600")
    _verify_all_present(tmp_path, ranges)


def test_clean_close_reopens_complete_without_replay_gap(tmp_path):
    """A clean stop in data-nonlogged mode must checkpoint (even under the
    fast-storage test env) so the reopen finds everything with an empty gap."""
    import _secantus_server

    env_backup = os.environ.get("SECANTUS_DATA_NONLOGGED")
    os.environ["SECANTUS_DATA_NONLOGGED"] = "1"
    try:
        store = str(tmp_path / "wt")
        srv = _secantus_server.RustServer(store, 0, host="127.0.0.1", replica_set_name="secantus")
        host, port = srv.address
        client = pymongo.MongoClient(host, port, directConnection=True)
        client["crashdb"]["c"].insert_many([{"_id": i} for i in range(500)])
        client.close()
        srv.stop()

        srv2 = _secantus_server.RustServer(store, 0, host="127.0.0.1", replica_set_name="secantus")
        try:
            host, port = srv2.address
            client = pymongo.MongoClient(host, port, directConnection=True)
            assert client["crashdb"]["c"].count_documents({}) == 500
            client.close()
        finally:
            srv2.stop()
    finally:
        if env_backup is None:
            os.environ.pop("SECANTUS_DATA_NONLOGGED", None)
        else:
            os.environ["SECANTUS_DATA_NONLOGGED"] = env_backup
