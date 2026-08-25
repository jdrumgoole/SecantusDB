"""An explicit ``maxTimeMS: 0`` on a change-stream getMore must not block.

mongod distinguishes an explicit zero from an absent field: zero is a
non-blocking poll that returns whatever is ready right now, an absent field
means wait. The value alone cannot tell them apart -- ``doc.get("maxTimeMS", 0)``
yields ``0`` for both -- and conflating them made every "just check" poll wait a
full second.

That is not merely slow, it changes results. The Go driver's ``TryNext`` sends
``maxTimeMS: 0`` for exactly one non-blocking getMore; while our server waited,
the *next* thing to happen in the test (its own teardown dropping the watched
collection) landed inside the wait window and came back as a ``drop`` +
``invalidate`` -- an event the client had just been told did not exist. It
surfaced as an intermittent failure of mongo-go-driver's
``TestChangeStream_ReplicaSet/try_next/one_getMore_sent``.
"""

from __future__ import annotations

import threading
import time

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


def _open_change_stream(db, coll):
    reply = db.command({"aggregate": coll, "pipeline": [{"$changeStream": {}}], "cursor": {}})
    return reply["cursor"]["id"]


def test_explicit_max_time_ms_zero_returns_immediately(server):
    """A zero deadline polls once and returns, rather than waiting."""
    client = pymongo.MongoClient(
        f"mongodb://127.0.0.1:{server.port}",
        directConnection=True,
        serverSelectionTimeoutMS=5000,
    )
    try:
        db = client["t"]
        db.create_collection("c")
        cid = _open_change_stream(db, "c")

        started = time.perf_counter()
        reply = db.command({"getMore": cid, "collection": "c", "maxTimeMS": 0})
        elapsed_ms = (time.perf_counter() - started) * 1000

        assert reply["cursor"]["nextBatch"] == []
        # The old behaviour waited the full 1s default. Allow generous headroom
        # for a loaded CI box while still failing decisively on a real wait.
        assert elapsed_ms < 400, f"maxTimeMS=0 blocked for {elapsed_ms:.0f}ms"
    finally:
        client.close()


def test_explicit_zero_does_not_pick_up_a_later_event(server):
    """The regression itself: a non-blocking poll must not return an event that
    happens after it was issued."""
    client = pymongo.MongoClient(
        f"mongodb://127.0.0.1:{server.port}",
        directConnection=True,
        serverSelectionTimeoutMS=5000,
    )
    try:
        db = client["t"]
        db.create_collection("c")
        cid = _open_change_stream(db, "c")

        # Drop the watched collection shortly after the poll is issued: inside
        # the old 1s wait, comfortably after a conforming poll has returned.
        def drop_later():
            time.sleep(0.15)
            client["t"].drop_collection("c")

        thread = threading.Thread(target=drop_later)
        thread.start()
        try:
            reply = db.command({"getMore": cid, "collection": "c", "maxTimeMS": 0})
            assert reply["cursor"]["nextBatch"] == [], (
                "a non-blocking getMore returned an event that had not happened when it was issued"
            )
        finally:
            thread.join(timeout=5)
    finally:
        client.close()


def test_absent_max_time_ms_still_waits_and_delivers(server):
    """The blocking path must keep working: no maxTimeMS means wait for an event."""
    client = pymongo.MongoClient(
        f"mongodb://127.0.0.1:{server.port}",
        directConnection=True,
        serverSelectionTimeoutMS=5000,
    )
    try:
        db = client["t"]
        db.create_collection("c")
        cid = _open_change_stream(db, "c")

        def insert_later():
            time.sleep(0.15)
            db["c"].insert_one({"x": 1})

        thread = threading.Thread(target=insert_later)
        thread.start()
        try:
            reply = db.command({"getMore": cid, "collection": "c"})
            batch = reply["cursor"]["nextBatch"]
            assert [e["operationType"] for e in batch] == ["insert"]
        finally:
            thread.join(timeout=5)
    finally:
        client.close()
