"""A durability gap found 2026-11-20 while explaining the "async stack" line
of the concurrency chart: `--oplog-nonlogged` on a store whose data tables
are not logged (the default) left nothing in WiredTiger's log, and a SIGKILL
lost every `j: true` write since the last checkpoint.

The reference is mongod's contract, not a measured answer: mongod journals
its oplog always, and a `j: true` write it acknowledged survives `kill -9`.
This server must either keep such a write or refuse the configuration that
cannot.
"""

from __future__ import annotations

import pathlib
import re
import signal
import subprocess

import pymongo
import pytest

from tests.backlog.conftest import RS_BINARY, drain_output

_BANNER = re.compile(r"secantusd-mdb listening on (\S+):(\d+)")


def _start(store: pathlib.Path, *flags: str) -> tuple[subprocess.Popen, int | None, str]:
    """The server on `store`; its port, or None and its output when it
    refused to start."""
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store), *flags],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    assert proc.stdout is not None
    seen = []
    for _ in range(50):
        line = proc.stdout.readline()
        seen.append(line)
        m = _BANNER.search(line)
        if m:
            drain_output(proc)
            return proc, int(m.group(2)), "".join(seen)
        if not line:
            break
    proc.wait(timeout=30)
    return proc, None, "".join(seen)


@pytest.mark.backlog_fixed("RM-731")
@pytest.mark.parametrize("flags", [("--oplog-nonlogged",), ("--oplog-async", "--oplog-nonlogged")])
def test_a_non_logged_oplog_keeps_or_refuses(tmp_path: pathlib.Path, flags: tuple) -> None:
    """On a default store a non-logged oplog is refused by name, or every
    `j: true` write it acknowledged survives SIGKILL -- never acknowledged
    and lost."""
    store = tmp_path / "store"
    proc, port, output = _start(store, *flags)
    if port is None:
        # Refused: it must say why, and acknowledge nothing.
        assert proc.returncode != 0
        assert "--data-logged" in output, output
        return
    client = pymongo.MongoClient(port=port, directConnection=True, serverSelectionTimeoutMS=10_000)
    coll = client.get_database("d", write_concern=pymongo.WriteConcern(j=True)).c
    acknowledged = 0
    for start in range(0, 2000, 100):
        coll.insert_many([{"_id": start + k} for k in range(100)])
        acknowledged += 100
    client.close()
    proc.send_signal(signal.SIGKILL)
    proc.wait(timeout=30)
    proc, port, output = _start(store, *flags)
    assert port is not None, output
    client = pymongo.MongoClient(port=port, directConnection=True, serverSelectionTimeoutMS=10_000)
    try:
        assert client.d.c.count_documents({}) == acknowledged
    finally:
        client.close()
        proc.terminate()
        proc.wait(timeout=60)
