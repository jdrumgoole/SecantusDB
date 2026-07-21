"""Regression: indexes on array fields are not marked `multiKey: true`
in `listIndexes` output, and `explain()` does not report `isMultiKey`.

Companion to `test_multikey_index_find.py`. That file tests the *query*
behavior; this one tests the *metadata* every driver and tooling relies
on to reason about the index. The two bugs share a root cause — no
multikey machinery for values inside arrays — but a fix has to satisfy
both to be MongoDB-compatible.

Expected on MongoDB after inserting a document with an array-valued
indexed field:

    db.wine_prices.getIndexes()[N].multiKey          == True
    db.wine_prices.find(...).explain().winningPlan
        ...IXSCAN.isMultiKey                          == True

On SecantusDB 0.6.0b0 both are absent. Drivers that consult `multiKey`
to pick a query plan or to skip covered-index paths will make wrong
decisions.

Fails against SecantusDB 0.6.0b0.
"""

from __future__ import annotations

import pytest
from bson import ObjectId
from pymongo import MongoClient
from secantus import SecantusDBServer


@pytest.fixture
def collection():
    with SecantusDBServer(port=0) as server:
        client = MongoClient(server.uri, directConnection=True)
        yield client["repro"]["wine_prices"]
        client.close()


def _find_index(collection, name):
    return next(ix for ix in collection.list_indexes() if ix["name"] == name)


def test_list_indexes_marks_dotted_array_index_as_multiKey(collection):
    """After inserting a document whose indexed field lives inside an
    array, `listIndexes` must report `multiKey: true` for that index.
    """
    owner = ObjectId()
    collection.insert_one(
        {"prices": [{"owner_id": owner, "price": 10.0}]}
    )
    collection.create_index([("prices.owner_id", 1)], name="prices_owner_id")

    ix = _find_index(collection, "prices_owner_id")
    assert ix.get("multiKey") is True, (
        "regression: after inserting an array-valued document, "
        "listIndexes reports "
        f"{ {k: v for k, v in ix.items() if k != 'v'} } — "
        "no `multiKey: true` flag. Every MongoDB driver relies on this "
        "flag to reason about the index."
    )


def test_index_marked_multiKey_when_created_before_insert(collection):
    """Order-independence: creating the index before the first array-valued
    insert must still promote the index to multikey once such a document
    lands.
    """
    collection.create_index([("prices.owner_id", 1)], name="prices_owner_id")
    collection.insert_one(
        {"prices": [{"owner_id": ObjectId(), "price": 10.0}]}
    )

    ix = _find_index(collection, "prices_owner_id")
    assert ix.get("multiKey") is True, (
        "index created before the first array-valued document is inserted "
        "must still be promoted to multikey after the insert"
    )


def test_explain_reports_isMultiKey_on_indexed_array_field(collection):
    """`explain()` on a query that uses an index over an array field must
    report `isMultiKey: true` under the IXSCAN stage. Query planners in
    higher-level tools (Compass, aggregation optimisers) use this signal.
    """
    owner = ObjectId()
    collection.insert_one(
        {"prices": [{"owner_id": owner, "price": 10.0}]}
    )
    collection.create_index([("prices.owner_id", 1)])

    plan = collection.find({"prices.owner_id": owner}).explain()
    winning = plan["queryPlanner"]["winningPlan"]
    # Descend to the IXSCAN stage — may be under FETCH.
    ixscan = winning if winning["stage"] == "IXSCAN" else winning.get("inputStage", {})
    assert ixscan.get("stage") == "IXSCAN", (
        "sanity: the planner chose an index scan for this query"
    )
    assert ixscan.get("isMultiKey") is True, (
        "regression: IXSCAN over a dotted-into-array field does not report "
        "isMultiKey: true. Full winningPlan for reference:\n"
        f"  {winning}"
    )
