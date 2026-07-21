"""Regression: dotted-path find on an indexed array-of-subdocuments field
returned no results, because SecantusDB generated no multikey index
entries for values inside an array.

Discovered while running the WineBox pre-deploy test suite against
SecantusDB 0.6.0b0. WineBox stores prices as an embedded array of
subdocuments on each `wine_prices` document:

    {
        "wine_name": "...",
        "prices": [
            {"owner_id": ObjectId(...), "price": 10.0, ...},
            ...
        ],
    }

and lists a user's prices with

    WinePrice.find({"prices.owner_id": current_user.id})

which is Beanie sugar for `db.wine_prices.find({"prices.owner_id": X})`.
On MongoDB the ascending index `{"prices.owner_id": 1}` is automatically
marked multikey on first insert of an array-valued document, so IXSCAN
returns the enclosing document. On SecantusDB 0.6.0b0 the same index
yielded `nReturned: 0` because no keys were ever generated for values
inside the array; forcing a collection scan (`hint=[("$natural", 1)]`)
returned the document, proving the docs were stored correctly and only
the index path was broken.

Fixed by making index-key generation walk *through* arrays the way
mongod's does — one key per element's leaf value.
"""

from __future__ import annotations

import pytest
from bson import ObjectId
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture
def collection(tmp_path):
    """Fresh collection on an ephemeral SecantusDB instance.

    ``port=0`` plus a per-test ``storage_path`` — without the latter every
    xdist worker opens the same default ``./secantus-data`` and the runs
    collide.
    """
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as server:
        client = MongoClient(server.uri, directConnection=True)
        yield client["repro"]["wine_prices"]
        client.close()


def test_dotted_path_into_array_matches_via_index_scan(collection):
    """`find({"array.field": X})` must return the document when an
    ascending index on `array.field` exists and the document has a
    subdocument in `array` with `field == X`.

    IXSCAN currently returns nothing; only a collection scan finds it.
    """
    owner = ObjectId()
    collection.insert_one(
        {
            "wine_name": "Chateau Test",
            "prices": [
                {"owner_id": owner, "price": 10.0, "currency": "EUR"},
            ],
        }
    )
    collection.create_index([("prices.owner_id", 1)])

    via_scan = list(collection.find({"prices.owner_id": owner}).hint([("$natural", 1)]))
    via_index = list(collection.find({"prices.owner_id": owner}))

    assert len(via_scan) == 1, (
        "sanity: the document should be findable at all — collection scan must return it"
    )
    assert len(via_index) == 1, (
        "regression: default query plan uses the index on prices.owner_id "
        "and returns 0 documents, while the identical scan finds 1. "
        "multikey index entries are missing for values inside the array."
    )


def test_dotted_path_matches_when_target_is_not_first_element(collection):
    """Every element of the array must be indexed, not just the first.
    Documents where `array.field == X` matches a non-leading element must
    still be returned by the index scan.
    """
    owner = ObjectId()
    other_owner_a = ObjectId()
    other_owner_b = ObjectId()
    collection.insert_one(
        {
            "wine_name": "Chateau Test",
            "prices": [
                {"owner_id": other_owner_a, "price": 8.0},
                {"owner_id": other_owner_b, "price": 9.0},
                {"owner_id": owner, "price": 10.0},
            ],
        }
    )
    collection.create_index([("prices.owner_id", 1)])

    result = list(collection.find({"prices.owner_id": owner}))
    assert len(result) == 1, (
        "index scan should locate the document via any element of the array, not only the first"
    )


def test_dotted_path_with_in_operator_matches_via_index(collection):
    """`$in` over an indexed array field must return matching documents.
    Common ODM pattern: `WinePrice.find({"prices.owner_id": {"$in": user_ids}})`.
    """
    owner = ObjectId()
    collection.insert_one(
        {
            "wine_name": "Chateau Test",
            "prices": [{"owner_id": owner, "price": 10.0}],
        }
    )
    collection.create_index([("prices.owner_id", 1)])

    result = list(collection.find({"prices.owner_id": {"$in": [owner]}}))
    assert len(result) == 1, (
        "$in over an indexed array-of-subdocuments field returns nothing "
        "for the same reason a bare-equality query does"
    )


def test_dotted_path_matches_after_indexing_a_pre_existing_document(collection):
    """Index creation over a collection that already contains array-valued
    documents must build multikey entries for those documents.

    Ordering: insert first, then create_index. This is the ODM startup
    pattern — Beanie calls `create_indexes()` at app startup, well after
    the first migration/deploy has populated the collection.
    """
    owner = ObjectId()
    collection.insert_one(
        {
            "wine_name": "Chateau Test",
            "prices": [{"owner_id": owner, "price": 10.0}],
        }
    )
    # Index built after the document exists.
    collection.create_index([("prices.owner_id", 1)])

    result = list(collection.find({"prices.owner_id": owner}))
    assert len(result) == 1, (
        "index built after the document must still index the values inside its array"
    )
