"""Regression: multikey index *metadata* for values inside arrays.

Companion to `test_multikey_index_find.py`. That file tests the *query*
behavior; this one tests the metadata a planner reads to reason about
the index. Both bugs shared a root cause — no multikey machinery for a
dotted path that descends into an array.

The expectations here were probed against a real `mongod` 6.0.16 rather
than assumed. After inserting a document with an array-valued indexed
field:

    db.wp.getIndexes()
        [{v: 2, key: {_id: 1}, name: "_id_"},
         {v: 2, key: {"prices.owner_id": 1}, name: "prices_owner_id"}]

    db.wp.find({"prices.owner_id": X}).explain().winningPlan
        {stage: "FETCH",
         inputStage: {stage: "IXSCAN", isMultiKey: true, ...}}

Note what `listIndexes` does *not* carry: there is no `multiKey` field.
mongod keeps the multikey flag in its durable catalog and surfaces it to
clients only through `explain`'s `isMultiKey`, so that — not
`getIndexes()` — is what this file pins.
"""

from __future__ import annotations

import pytest
from bson import ObjectId
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture
def collection(tmp_path):
    """``port=0`` plus a per-test ``storage_path`` — without the latter every
    xdist worker opens the same default ``./secantus-data`` and the runs
    collide."""
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as server:
        client = MongoClient(server.uri, directConnection=True)
        yield client["repro"]["wine_prices"]
        client.close()


def _find_index(collection, name):
    return next(ix for ix in collection.list_indexes() if ix["name"] == name)


def _ixscan_stage(plan):
    winning = plan["queryPlanner"]["winningPlan"]
    return winning if winning["stage"] == "IXSCAN" else winning.get("inputStage", {})


def test_list_indexes_matches_mongod_field_set(collection):
    """`listIndexes` must describe an index over an array field exactly
    as mongod does — `{v, key, name}` and nothing else.

    In particular the internal multikey flag must not leak onto the
    wire: mongod has no such field, and a driver that round-trips a
    listIndexes entry back into `createIndexes` would be handing the
    server an option it never emitted.
    """
    collection.insert_one({"prices": [{"owner_id": ObjectId(), "price": 10.0}]})
    collection.create_index([("prices.owner_id", 1)], name="prices_owner_id")

    ix = _find_index(collection, "prices_owner_id")
    assert dict(ix) == {"v": 2, "key": {"prices.owner_id": 1}, "name": "prices_owner_id"}, (
        f"listIndexes must match mongod's shape for a multikey index — got {dict(ix)}"
    )


def test_explain_reports_isMultiKey_on_indexed_array_field(collection):
    """`explain()` on a query that uses an index over an array field must
    report `isMultiKey: true` under the IXSCAN stage. Query planners in
    higher-level tools (Compass, aggregation optimisers) use this signal.
    """
    owner = ObjectId()
    collection.insert_one({"prices": [{"owner_id": owner, "price": 10.0}]})
    collection.create_index([("prices.owner_id", 1)])

    ixscan = _ixscan_stage(collection.find({"prices.owner_id": owner}).explain())
    assert ixscan.get("stage") == "IXSCAN", "sanity: the planner chose an index scan"
    assert ixscan.get("isMultiKey") is True, (
        "regression: IXSCAN over a dotted-into-array field does not report "
        f"isMultiKey: true. IXSCAN stage for reference:\n  {ixscan}"
    )


def test_index_becomes_multikey_when_created_before_insert(collection):
    """Order-independence: creating the index before the first array-valued
    insert must still promote the index to multikey once such a document
    lands.
    """
    owner = ObjectId()
    collection.create_index([("prices.owner_id", 1)])
    collection.insert_one({"prices": [{"owner_id": owner, "price": 10.0}]})

    ixscan = _ixscan_stage(collection.find({"prices.owner_id": owner}).explain())
    assert ixscan.get("isMultiKey") is True, (
        "index created before the first array-valued document is inserted "
        "must still be promoted to multikey after the insert"
    )


def test_scalar_only_index_is_not_multikey(collection):
    """The flag must discriminate: an index over a scalar field reports
    `isMultiKey: false`, not a blanket true.
    """
    collection.insert_one({"wine_name": "Chateau Test", "vintage": 2019})
    collection.create_index([("vintage", 1)])

    ixscan = _ixscan_stage(collection.find({"vintage": 2019}).explain())
    assert ixscan.get("stage") == "IXSCAN", "sanity: the planner chose an index scan"
    assert ixscan.get("isMultiKey") is False
