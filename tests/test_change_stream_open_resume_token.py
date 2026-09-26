"""A change stream's OPEN reply must not hand out a token past unsent events.

Opening (or resuming) a stream polls every event already waiting, returns at
most ``batchSize`` of them and holds the rest for ``getMore``. The Rust server
set ``postBatchResumeToken`` from the position AFTER the whole poll, so a
driver that resumed after draining the first batch skipped the held-back
events -- silently. pymongo's unified "Test consecutive resume" lost two of
three inserts that way (2026-09-25). The token must be the last SENT event's
``_id``. Run on BOTH servers; the Python server already had it right.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture(params=["python", "rust"])
def client(request, tmp_path) -> Iterator[MongoClient]:
    if request.param == "python":
        with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
            mc = MongoClient(srv.uri, serverSelectionTimeoutMS=3000)
            try:
                yield mc
            finally:
                mc.close()
        return
    _server = pytest.importorskip("_secantus_server")
    srv = _server.RustServer(
        storage_path=str(tmp_path / "wt"), port=0, host="127.0.0.1", replica_set_name="secantus"
    )
    try:
        host, port = srv.address
        mc = MongoClient(host, port, serverSelectionTimeoutMS=3000)
        try:
            yield mc
        finally:
            mc.close()
    finally:
        srv.stop()


def _open(db, resume_after, batch_size):
    return db.command(
        {
            "aggregate": "c",
            "pipeline": [{"$changeStream": {"resumeAfter": resume_after}}],
            "cursor": {"batchSize": batch_size},
        }
    )["cursor"]


def test_open_reply_token_does_not_skip_held_back_events(client: MongoClient) -> None:
    db = client["cs_open_token"]
    db.create_collection("c")
    start = db.command({"aggregate": "c", "pipeline": [{"$changeStream": {}}], "cursor": {}})[
        "cursor"
    ]["postBatchResumeToken"]
    for x in (1, 2, 3):
        db["c"].insert_one({"x": x})

    # Resume with batchSize 1: one event sent, two held back for getMore.
    cur = _open(db, start, 1)
    assert [e["fullDocument"]["x"] for e in cur["firstBatch"]] == [1]
    assert cur["postBatchResumeToken"] == cur["firstBatch"][-1]["_id"]

    # Resuming from that token -- what a driver does after a dropped getMore --
    # must still deliver both held-back events.
    again = _open(db, cur["postBatchResumeToken"], 10)
    assert [e["fullDocument"]["x"] for e in again["firstBatch"]] == [2, 3]


def test_open_reply_with_batch_size_zero_keeps_the_start_token(client: MongoClient) -> None:
    db = client["cs_open_token_zero"]
    db.create_collection("c")
    start = db.command({"aggregate": "c", "pipeline": [{"$changeStream": {}}], "cursor": {}})[
        "cursor"
    ]["postBatchResumeToken"]
    for x in (1, 2):
        db["c"].insert_one({"x": x})

    cur = _open(db, start, 0)
    assert cur["firstBatch"] == []
    # Nothing was sent, so resuming from the reply's token must still see both.
    again = _open(db, cur["postBatchResumeToken"], 10)
    assert [e["fullDocument"]["x"] for e in again["firstBatch"]] == [1, 2]
