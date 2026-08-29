"""Cursor exhaustion matches mongod's round-trip count.

mongod closes a cursor only when it KNOWS the result is finished. A batch that
exactly fills the requested ``batchSize`` proves nothing about what follows, so
the cursor stays OPEN and the client spends one more ``getMore`` to see an empty
batch. SecantusDB buffers the whole result and so used to close early -- fewer
round trips, but a count drivers observe directly (mongo-go-driver's
``verifyOneGetmoreSent`` asserts on exactly that).

Every expectation here was probed against a real mongod 8.3.4. Two of them are
the ones a plausible-sounding rule gets wrong:

* the rule is NOT uniform across commands -- ``listIndexes`` / ``listCollections``
  close on an exact-fill batch (they enumerate a known catalog) while ``find`` /
  ``aggregate`` do not;
* a non-positive ``batchSize`` on ``getMore`` means "server default", i.e. no
  size was requested, and mongod then drains AND closes.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture
def client(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli
    finally:
        cli.close()
        srv.stop()


def _walk(db, cmd, batch_size, coll="c"):
    """Return the per-batch sizes, firstBatch first."""
    reply = db.command(cmd)
    cur = reply["cursor"]
    sizes = [len(cur["firstBatch"])]
    cursor_id = cur["id"]
    guard = 0
    while cursor_id and guard < 20:
        more = db.command({"getMore": cursor_id, "collection": coll, "batchSize": batch_size})
        cursor_id = more["cursor"]["id"]
        sizes.append(len(more["cursor"]["nextBatch"]))
        guard += 1
    return sizes


def _seed(db, n):
    db.c.drop()
    if n:
        db.c.insert_many([{"x": i} for i in range(n)])
    else:
        db.create_collection("c")


@pytest.mark.parametrize(
    "ndocs,batch,expected",
    [
        (4, 2, [2, 2, 0]),  # exact multiple -> trailing empty batch
        (4, 4, [4, 0]),  # firstBatch alone drains it -> still open
        (1, 1, [1, 0]),
        (10, 5, [5, 5, 0]),
        (5, 2, [2, 2, 1]),  # short final batch -> closes, no extra trip
        (7, 3, [3, 3, 1]),
        (4, 10, [4]),  # never fills -> closes immediately
        (0, 2, [0]),  # empty collection
    ],
)
def test_find_round_trips_match_mongod(client, ndocs, batch, expected) -> None:
    db = client["curp"]
    _seed(db, ndocs)
    assert _walk(db, {"find": "c", "batchSize": batch}, batch) == expected


@pytest.mark.parametrize(
    "ndocs,batch,expected",
    [(4, 2, [2, 2, 0]), (4, 4, [4, 0]), (5, 2, [2, 2, 1])],
)
def test_aggregate_round_trips_match_mongod(client, ndocs, batch, expected) -> None:
    db = client["curp_agg"]
    _seed(db, ndocs)
    cmd = {"aggregate": "c", "pipeline": [], "cursor": {"batchSize": batch}}
    assert _walk(db, cmd, batch) == expected


@pytest.mark.parametrize(
    "ndocs,batch,limit,expected",
    [
        (4, 2, 4, [2, 2]),  # limit makes exhaustion knowable -> no extra trip
        (6, 3, 6, [3, 3]),
        (6, 2, 5, [2, 2, 1]),
    ],
)
def test_limit_closes_without_a_trailing_batch(client, ndocs, batch, limit, expected) -> None:
    db = client["curp_lim"]
    _seed(db, ndocs)
    assert _walk(db, {"find": "c", "batchSize": batch, "limit": limit}, batch) == expected


def test_single_batch_closes(client) -> None:
    db = client["curp_sb"]
    _seed(db, 4)
    reply = db.command({"find": "c", "batchSize": 2, "limit": 2, "singleBatch": True})
    assert reply["cursor"]["id"] == 0
    assert len(reply["cursor"]["firstBatch"]) == 2


def test_getmore_without_batch_size_drains_and_closes(client) -> None:
    """A non-positive/absent batchSize means 'server default': mongod returns
    everything left and closes, rather than holding the cursor open."""
    db = client["curp_zero"]
    _seed(db, 5)
    reply = db.command({"find": "c", "batchSize": 2})
    cursor_id = reply["cursor"]["id"]
    assert cursor_id != 0
    more = db.command({"getMore": cursor_id, "collection": "c"})
    assert len(more["cursor"]["nextBatch"]) == 3
    assert more["cursor"]["id"] == 0, "cursor must close when no size was requested"


def test_list_indexes_closes_on_exact_fill(client) -> None:
    """Catalog commands enumerate a KNOWN set, so mongod closes them on an
    exact-fill batch -- the opposite of find/aggregate. Assuming one uniform
    rule across commands would silently break this."""
    db = client["curp_li"]
    _seed(db, 1)
    db.c.create_index("x")
    db.c.create_index("q")  # _id_ + 2 == 3 indexes
    reply = db.command({"listIndexes": "c", "cursor": {"batchSize": 3}})
    assert len(reply["cursor"]["firstBatch"]) == 3
    assert reply["cursor"]["id"] == 0


def test_list_collections_closes_on_exact_fill(client) -> None:
    db = client["curp_lc"]
    _seed(db, 1)
    names = db.list_collection_names()
    reply = db.command({"listCollections": 1, "cursor": {"batchSize": len(names)}})
    assert len(reply["cursor"]["firstBatch"]) == len(names)
    assert reply["cursor"]["id"] == 0


def test_documents_are_not_lost_or_duplicated(client) -> None:
    """The extra round trip must not change WHAT comes back."""
    db = client["curp_all"]
    _seed(db, 20)
    got = sorted(d["x"] for d in db.c.find({}).batch_size(4))
    assert got == list(range(20))
