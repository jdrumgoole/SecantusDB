"""Acknowledged `j: true` writes survive a SIGKILL mid-load, in the default mode.

`tests/test_crash_recovery.py` hard-kills a writer, but only with
`SECANTUS_DATA_NONLOGGED=1` (data tables recovered by oplog replay), which
is not how `secantusd-rs` runs by default. The one default-mode kill test,
RM-186's regression test in `tests/backlog/`, kills an IDLE server after 50
sequential inserts. This one kills the standalone binary while four clients
are inserting, updating and deleting concurrently, every write with
`{w: 1, j: true}`: mongod's contract is that a `j: true` acknowledgement
means the write is in the synced journal, so it survives any crash.

After the restart every document must hold exactly the value its last
acknowledged write gave it. The one write each client had in flight at the
kill may have landed or not, but only whole: its document holds either the
value before it or the value after it.
"""

from __future__ import annotations

import os
import pathlib
import random
import re
import subprocess
import sys
import threading
import time
from collections.abc import Iterator
from typing import Any

import pytest

pymongo = pytest.importorskip("pymongo")
from pymongo.write_concern import WriteConcern  # noqa: E402

_REPO = pathlib.Path(__file__).resolve().parent.parent
_BANNER = re.compile(r"secantusd-rs listening on (\S+):(\d+)")


def _binary() -> pathlib.Path | None:
    env = os.environ.get("SECANTUSDB_BIN")
    if env:
        return pathlib.Path(env) if pathlib.Path(env).exists() else None
    for profile in ("debug", "release"):
        p = _REPO / "crates" / "secantusdb" / "target" / profile / "secantusd-rs"
        if p.exists():
            return p
    return None


_BIN = _binary()
pytestmark = [
    pytest.mark.skipif(_BIN is None, reason="secantusd-rs binary not built"),
    pytest.mark.skipif(sys.platform == "win32", reason="SIGKILL semantics: POSIX only"),
]

_CLIENTS = 4
_SEED = 200  # ids 0.._SEED-1 exist at the start; client k owns ids with id % _CLIENTS == k


class _Server:
    def __init__(self, store: pathlib.Path) -> None:
        assert _BIN is not None
        self.proc = subprocess.Popen(
            [str(_BIN), "--port", "0", "--storage-path", str(store)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        assert self.proc.stdout is not None
        # A store that recovers says what it replayed before it listens.
        line, m = "", None
        for _ in range(20):
            line = self.proc.stdout.readline()
            m = _BANNER.search(line)
            if m or not line:
                break
        assert m, f"secantusd-rs did not start: {line!r}"
        # Keep draining: an unread pipe fills at 64 KiB and blocks the server.
        out = self.proc.stdout
        threading.Thread(target=lambda: [None for _ in out], daemon=True).start()
        self.host, self.port = m.group(1), int(m.group(2))

    def client(self, **kw: Any) -> Any:
        return pymongo.MongoClient(
            self.host, self.port, directConnection=True, serverSelectionTimeoutMS=5_000, **kw
        )

    def kill(self) -> None:
        self.proc.kill()  # SIGKILL: no shutdown, no checkpoint, no log flush
        self.proc.wait(timeout=20)

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)


class _Writer(threading.Thread):
    """Client `k`: random inserts, updates and deletes on the ids it owns,
    recording each one only once the server has acknowledged it."""

    def __init__(self, k: int, coll: Any, stop: threading.Event) -> None:
        super().__init__(daemon=True)
        self.k, self.coll, self.stop = k, coll, stop
        # id -> value after the last acknowledged write (None: deleted).
        self.acked: dict[int, int | None] = {i: 0 for i in range(k, _SEED, _CLIENTS)}
        # (id, value before, value after) of the write not yet acknowledged.
        self.inflight: tuple[int, int | None, int | None] | None = None
        self.next_id = _SEED + k
        self.n_acked = 0
        self.rng = random.Random(k)

    def _one(self) -> None:
        live = [i for i, v in self.acked.items() if v is not None]
        op = self.rng.choice(["update", "update", "insert", "delete"] if live else ["insert"])
        if op == "insert":
            i, self.next_id = self.next_id, self.next_id + _CLIENTS
            self.inflight = (i, None, 0)
            self.coll.insert_one({"_id": i, "v": 0})
        elif op == "update":
            i = self.rng.choice(live)
            old = self.acked[i]
            assert old is not None
            self.inflight = (i, old, old + 1)
            self.coll.update_one({"_id": i}, {"$set": {"v": old + 1}})
        else:
            i = self.rng.choice(live)
            self.inflight = (i, self.acked[i], None)
            self.coll.delete_one({"_id": i})
        self.acked[i] = self.inflight[2]
        self.inflight = None
        self.n_acked += 1

    def run(self) -> None:
        while not self.stop.is_set():
            try:
                self._one()
            except pymongo.errors.PyMongoError:
                return  # the server is gone; `inflight` names what was pending


@pytest.fixture
def store(tmp_path: pathlib.Path) -> Iterator[pathlib.Path]:
    yield tmp_path / "store"


def test_acknowledged_journaled_writes_survive_sigkill_mid_load(store: pathlib.Path) -> None:
    server = _Server(store)
    try:
        # retryWrites off: a retry after the kill would re-send the in-flight
        # write to nothing and blur which attempt the ack belonged to.
        client = server.client(retryWrites=False, socketTimeoutMS=10_000)
        coll = client.crash.c.with_options(write_concern=WriteConcern(w=1, j=True))
        coll.insert_many([{"_id": i, "v": 0} for i in range(_SEED)])
        stop = threading.Event()
        writers = [_Writer(k, coll, stop) for k in range(_CLIENTS)]
        for w in writers:
            w.start()
        # Kill once every client has had a good run of acknowledged writes, so
        # the kill lands in the middle of steady concurrent load.
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline and min(w.n_acked for w in writers) < 50:
            assert server.proc.poll() is None, "secantusd-rs exited under load"
            time.sleep(0.05)
        assert min(w.n_acked for w in writers) >= 50, "load never got going"
        server.kill()
        stop.set()
        for w in writers:
            w.join(timeout=30)
            assert not w.is_alive(), f"writer {w.k} did not notice the server die"
        client.close()
    finally:
        server.stop()

    server = _Server(store)
    try:
        client = server.client()
        found = {d["_id"]: d["v"] for d in client.crash.c.find({}, {"v": 1})}
        client.close()
    finally:
        server.stop()

    wrong = []
    owned: set[int] = set()
    for w in writers:
        owned.update(w.acked)
        if w.inflight is not None:
            owned.add(w.inflight[0])
        for i, value in w.acked.items():
            allowed = {value}
            if w.inflight is not None and w.inflight[0] == i:
                allowed = {w.inflight[1], w.inflight[2]}
            if found.get(i) not in allowed:
                wrong.append((i, found.get(i), sorted(allowed, key=repr)))
        if w.inflight is not None and w.inflight[0] not in w.acked:
            # An insert in flight: absent, or present with its value.
            i = w.inflight[0]
            if found.get(i) not in (None, 0):
                wrong.append((i, found.get(i), [None, 0]))
    strays = sorted(set(found) - owned)
    acked_total = sum(w.n_acked for w in writers)
    assert not wrong, (
        f"{len(wrong)} documents do not hold their last acknowledged value after "
        f"SIGKILL and restart (of {acked_total} acknowledged writes); "
        f"(id, found, allowed): {wrong[:20]}"
    )
    assert not strays, f"documents no acknowledged write created: {strays[:20]}"
