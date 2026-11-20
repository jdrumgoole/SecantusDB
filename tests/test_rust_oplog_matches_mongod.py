"""`secantusd-mdb` logs what mongod 8.2.11 logs, field for field.

Phase 4b of ``tasks/mongod-storage-model-plan.md``: ``local.oplog.rs`` holds
mongod's entries. ``tools/probes/mongod_oplog_corpus.py`` ran 22 operations on
a one-member mongod 8.2.11 replica set and recorded the entries each wrote
(``tests/fixtures/storage_model/mongod-8.2.11-oplog.json``); this runs the same
operations against the standalone binary and holds what ``find`` on
``local.oplog.rs`` returns to the fixture: the same entries, their fields in the
same order, each of the same BSON type, with the same ``op`` and ``ns``; and,
inside ``o``, the same field names. The values that differ run to run -- the
timestamps, the wall clock, the UUIDs, the session -- are the only ones not
compared, and they are compared by type.

Steps not held to the fixture would be listed in ``_NOT_YET`` with what is lost;
none is.
"""

from __future__ import annotations

import importlib.util
import json
import os
import pathlib
import re
import subprocess
import sys
import threading
import time
from typing import Any

import pytest

pymongo = pytest.importorskip("pymongo")
from bson import Binary, Int64, Timestamp, json_util  # noqa: E402
from bson.objectid import ObjectId  # noqa: E402

_REPO = pathlib.Path(__file__).resolve().parent.parent
_FIXTURE = _REPO / "tests" / "fixtures" / "storage_model" / "mongod-8.2.11-oplog.json"
_BANNER = re.compile(r"secantusd-mdb listening on (\S+):(\d+)")


def _binary() -> pathlib.Path | None:
    env = os.environ.get("SECANTUSDB_BIN")
    if env:
        return pathlib.Path(env) if pathlib.Path(env).exists() else None
    for profile in ("debug", "release"):
        p = _REPO / "crates" / "secantusdb" / "target" / profile / "secantusd-mdb"
        if p.exists():
            return p
    return None


pytestmark = [
    pytest.mark.skipif(_binary() is None, reason="secantusd-mdb binary not built"),
    pytest.mark.skipif(sys.platform == "win32", reason="POSIX only"),
]

#: Steps whose entries are not yet mongod's, and what that loses. None: every
#: step of the corpus is held to the fixture.
_NOT_YET: dict[str, str] = {}


def _corpus() -> Any:
    spec = importlib.util.spec_from_file_location(
        "mongod_oplog_corpus", _REPO / "tools" / "probes" / "mongod_oplog_corpus.py"
    )
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _shape(value: Any) -> Any:
    """A value reduced to its field names, their order and the BSON type of each
    leaf; the values that change from run to run are not kept."""
    if isinstance(value, dict):
        return [(k, _shape(v)) for k, v in value.items()]
    if isinstance(value, list):
        return [_shape(v) for v in value]
    if isinstance(value, Binary):
        return f"binary{value.subtype}"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, Int64):
        return "long"
    if isinstance(value, int):
        return "int"
    if isinstance(value, float):
        return "double"
    if isinstance(value, Timestamp):
        return "timestamp"
    if isinstance(value, ObjectId):
        return "objectid"
    if value is None:
        return "null"
    if isinstance(value, str):
        return "string"
    return type(value).__name__


def _compare(entry: dict[str, Any]) -> Any:
    """What an entry is compared by: its shape, with its `op` and `ns` as well,
    and the names inside `o`."""
    out = _shape(entry)
    return {"op": entry.get("op"), "ns": entry.get("ns"), "shape": out}


def _fixture_steps() -> list[dict[str, Any]]:
    return json_util.loads(_FIXTURE.read_text())["steps"]


def _for_app(entries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Leave out what mongod wrote for its own start (the `config` collections
    its first step creates)."""
    return [e for e in entries if not str(e.get("ns", "")).startswith("config.$cmd")]


class _Server:
    def __init__(self, store: pathlib.Path) -> None:
        binary = _binary()
        assert binary is not None
        env = {k: v for k, v in os.environ.items() if k != "SECANTUS_TEST_FAST_STORAGE"}
        self.proc = subprocess.Popen(
            [str(binary), "--port", "0", "--storage-path", str(store)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            env=env,
        )
        assert self.proc.stdout is not None
        line, m = "", None
        for _ in range(50):
            line = self.proc.stdout.readline()
            m = _BANNER.search(line)
            if m or not line:
                break
        assert m, f"secantusd-mdb did not start: {line!r}"
        out = self.proc.stdout
        threading.Thread(target=lambda: [None for _ in out], daemon=True).start()
        self.host, self.port = m.group(1), int(m.group(2))

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)


def test_the_corpus_is_logged_as_mongod_logs_it(tmp_path: pathlib.Path) -> None:
    corpus = _corpus()
    expected = {s["name"]: _for_app(s["entries"]) for s in _fixture_steps()}
    server = _Server(tmp_path / "store")
    problems: list[str] = []
    try:
        client = pymongo.MongoClient(server.host, server.port, directConnection=True)
        oplog = client["local"]["oplog.rs"]
        for name, run in corpus.steps(client):
            last = next(oplog.find().sort("$natural", -1).limit(1))["ts"]
            run()
            time.sleep(0.2)
            got = [
                e
                for e in oplog.find({"ts": {"$gt": last}}).sort("$natural", 1)
                if not (e.get("op") == "n" and e.get("o", {}).get("msg") == "periodic noop")
            ]
            if name in _NOT_YET:
                continue
            want = expected[name]
            ours = [_compare(e) for e in got]
            theirs = [_compare(e) for e in want]
            if ours != theirs:
                problems.append(
                    f"{name}:\n  mongod: {json.dumps(theirs, default=str)[:1500]}\n"
                    f"  here:   {json.dumps(ours, default=str)[:1500]}"
                )
        client.close()
    finally:
        server.stop()
    assert not problems, "\n".join(problems)


def test_every_row_of_the_oplog_table_is_keyed_by_its_timestamp(tmp_path: pathlib.Path) -> None:
    server = _Server(tmp_path / "store")
    try:
        client = pymongo.MongoClient(server.host, server.port, directConnection=True)
        client.t.c.insert_many([{"_id": i} for i in range(5)])
        client.t.c.update_one({"_id": 1}, {"$set": {"a": 1}})
        rows = list(client["local"]["oplog.rs"].find({}, show_record_id=True))
        assert rows
        for row in rows:
            ts = row["ts"]
            assert row["$recordId"] == (ts.time << 32) | ts.inc
        client.close()
    finally:
        server.stop()


def test_a_change_stream_resumes_from_an_event_inside_a_transaction(
    tmp_path: pathlib.Path,
) -> None:
    """A transaction is one `applyOps` entry; its writes are separate events,
    told apart in their resume tokens by their place in the entry
    (`txnOpIndex`), and a stream resumed from any of them carries on from the
    next."""
    server = _Server(tmp_path / "store")
    try:
        client = pymongo.MongoClient(server.host, server.port, directConnection=True)
        db = client.txn
        db.create_collection("a")
        db.create_collection("b")
        with db.watch() as stream, client.start_session() as session:
            with session.start_transaction():
                db.a.insert_one({"_id": 1}, session=session)
                db.b.insert_one({"_id": 2}, session=session)
                db.a.insert_one({"_id": 3}, session=session)
            events = [stream.next() for _ in range(3)]
        assert [e["documentKey"]["_id"] for e in events] == [1, 2, 3]
        tokens = [e["_id"] for e in events]
        assert len({t["_data"] for t in tokens}) == 3, "each event has a token of its own"
        for i in range(3):
            with db.watch(resume_after=tokens[i], max_await_time_ms=500) as resumed:
                got = []
                while True:
                    ev = resumed.try_next()
                    if ev is None:
                        break
                    got.append(ev["documentKey"]["_id"])
            assert got == [1, 2, 3][i + 1 :], (i, got)
        client.close()
    finally:
        server.stop()
