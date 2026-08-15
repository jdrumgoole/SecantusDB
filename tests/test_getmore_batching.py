"""getMore default-batch semantics, pinned to mongod's over the wire.

mongod's 101-document default applies only to a find/aggregate FIRST batch;
an unspecified ``batchSize`` on getMore fills the batch up to 16MB. Before
this was fixed both servers reused the 101 default on every getMore, so a
full collection scan paid ``count / 101`` round trips instead of ~2 — the
entirety of the benchmark's 2.2x find_all gap to mongod.

Both servers are pinned here through real pymongo wire traffic; the Rust
variants skip when the ``_secantus_server`` extension isn't built.
"""

from __future__ import annotations

import contextlib

import bson
import pymongo
import pytest
from bson import Int64, decode_all

from secantus import SecantusDBServer


@contextlib.contextmanager
def _python_client(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        client = pymongo.MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            yield client
        finally:
            client.close()


@contextlib.contextmanager
def _rust_client(tmp_path):
    _server = pytest.importorskip("_secantus_server")
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        client = pymongo.MongoClient(
            host, port, directConnection=True, serverSelectionTimeoutMS=5000
        )
        try:
            yield client
        finally:
            client.close()
    finally:
        srv.stop()


_CLIENTS = {"python": _python_client, "rust": _rust_client}


@pytest.fixture(params=["python", "rust"])
def client(request, tmp_path):
    with _CLIENTS[request.param](tmp_path) as c:
        yield c


def test_default_getmore_drains_in_one_batch(client) -> None:
    """No batchSize anywhere: firstBatch is 101 docs, the single getMore
    returns everything else (it fits in 16MB) — mongod's exact shape."""
    coll = client["db"]["c"]
    coll.insert_many({"_id": i} for i in range(500))
    batches = [len(decode_all(bytes(b))) for b in coll.find_raw_batches({})]
    assert batches == [101, 399]


def test_explicit_getmore_batch_size_still_honored(client) -> None:
    """An explicit batchSize keeps its per-batch document count."""
    coll = client["db"]["c"]
    coll.insert_many({"_id": i} for i in range(250))
    batches = [len(decode_all(bytes(b))) for b in coll.find_raw_batches({}, batch_size=100)]
    assert batches == [100, 100, 50]


def test_default_getmore_respects_16mb_byte_budget(client) -> None:
    """With ~1MB documents a default getMore batch stops before 16MB and the
    cursor survives to serve the remainder across further getMores."""
    db = client["db"]
    pad = "x" * (1024 * 1024)
    db.c.insert_many({"_id": i, "pad": pad} for i in range(40))

    first = db.command("find", "c", batchSize=1)
    assert len(first["cursor"]["firstBatch"]) == 1
    cursor_id = first["cursor"]["id"]
    assert cursor_id != 0

    seen = 1
    batches = []
    while cursor_id != 0:
        reply = db.command("getMore", Int64(cursor_id), collection="c")
        batch = reply["cursor"]["nextBatch"]
        batches.append(len(batch))
        # Every batch stays under the 16MB budget (plus one doc of slack for
        # the always-make-progress rule).
        assert sum(len(bson.encode(d)) for d in batch) <= 16 * 1024 * 1024
        seen += len(batch)
        cursor_id = reply["cursor"]["id"]
    assert seen == 40
    # The drain took more than one getMore (byte cap engaged) but far fewer
    # than one-per-doc, and every full batch carried multiple documents.
    assert 2 <= len(batches) <= 5
    assert batches[0] > 1
