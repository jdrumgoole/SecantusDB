"""A find/aggregate FIRST batch is byte-capped like mongod's, not just count-capped.

`getMore` already had mongod's 16MB reply budget; the first batch did not. `find`
with `batchSize: 25` over 1MB documents therefore assembled a **25MB** reply and
exhausted the cursor, where mongod returns 15MB and hands back a live cursor id.

The expectations here were measured against a real mongod 6.0.16 rather than read
off the spec: same 25 x 1MiB documents, same `batchSize: 25`, mongod answered
`firstBatch` of 15 documents (15.0 MiB) with a non-zero cursor id.
"""

from __future__ import annotations

import bson
import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer

ONE_MIB = 1024 * 1024
MONGOD_REPLY_CAP = 16 * ONE_MIB


@pytest.fixture
def client(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        try:
            yield mc
        finally:
            mc.close()


def batch_bytes(docs: list[dict]) -> int:
    return sum(len(bson.encode(d)) for d in docs)


def test_first_batch_stops_under_the_reply_cap(client: MongoClient) -> None:
    """25 x 1MiB documents with batchSize 25 must not produce a 25MB reply."""
    payload = "x" * ONE_MIB
    client.t.big.insert_many([{"_id": i, "p": payload} for i in range(25)])

    reply = client.t.command({"find": "big", "batchSize": 25})
    first = reply["cursor"]["firstBatch"]

    assert batch_bytes(first) <= MONGOD_REPLY_CAP
    assert len(first) < 25, "the count cap alone would have returned all 25"
    assert reply["cursor"]["id"] != 0, "the rest must stay behind a live cursor"


def test_the_cursor_still_drains_completely(client: MongoClient) -> None:
    """Capping the first batch must not lose documents."""
    payload = "x" * ONE_MIB
    client.t.big.insert_many([{"_id": i, "p": payload} for i in range(25)])

    seen = [d["_id"] for d in client.t.big.find({}, batch_size=25)]
    assert sorted(seen) == list(range(25))


def test_a_single_oversized_document_still_makes_progress(client: MongoClient) -> None:
    """Never return an empty batch with documents pending — that hangs a client.

    The budget takes at least one document even when it alone exceeds the cap,
    matching `CursorRegistry.next_batch`.
    """
    # 12 MiB each: two exceed the 16MB cap together, so the budget must still
    # hand back the first one alone rather than nothing.
    payload = "y" * (12 * ONE_MIB)
    client.t.huge.insert_many([{"_id": i, "p": payload} for i in range(2)])

    reply = client.t.command({"find": "huge", "batchSize": 2})
    first = reply["cursor"]["firstBatch"]
    assert len(first) == 1
    assert reply["cursor"]["id"] != 0


def test_small_documents_are_unaffected(client: MongoClient) -> None:
    """The common path must not change: the count cap still governs."""
    client.t.small.insert_many([{"_id": i, "n": i} for i in range(500)])

    reply = client.t.command({"find": "small", "batchSize": 101})
    assert len(reply["cursor"]["firstBatch"]) == 101
    assert reply["cursor"]["id"] != 0
