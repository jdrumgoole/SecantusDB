"""``$meta`` projection VALUES, not just its validation.

``recordId`` / ``sortKey`` / ``indexKey`` validated cleanly and then produced
nothing: the field was omitted from every document. mongod returns real values
for all three, and SecantusDB has the underlying data (a RecordId per row, the
sort spec, the chosen index) -- it was simply never plumbed to the projection.

Every expectation is mongod 8.2.11's own output, measured 2026-09-01.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def server(wt_home_module):
    with SecantusDBServer(port=0, storage_path=wt_home_module) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=5000)
    try:
        yield c
    finally:
        c.close()


@pytest.fixture
def coll(client, request):
    c = client["meta_db"][request.node.name[:60]]
    c.drop()
    c.insert_many([{"_id": 1, "a": 1, "b": "x", "arr": [3, 1]}, {"_id": 2, "a": 3, "b": "y"}])
    return c


def _one(coll, **kw):
    return list(coll.database.command({"find": coll.name, **kw})["cursor"]["firstBatch"])


def test_record_id_is_the_row_identifier(coll):
    """Unique per row, and ASCENDING in insertion order.

    The literal NUMBERS deliberately are not asserted: SecantusDB's RecordId is
    a store-wide insertion counter, while mongod restarts it per collection, so
    a second collection in the same store starts at 3 where mongod starts at 1
    (measured 2026-09-01). Matching the numbers would mean a per-collection
    counter, which is the doc table's KEY -- a storage-format change, and one
    the Rust server shares byte-for-byte. What a caller can rely on is what is
    asserted here.
    """
    rows = _one(coll, filter={}, projection={"m": {"$meta": "recordId"}})
    ids = [d["m"] for d in rows]
    assert len(set(ids)) == len(ids)
    assert ids == sorted(ids)


def test_record_id_survives_a_sort(coll):
    """It identifies the ROW, so re-ordering the output must not renumber it."""
    unsorted = {
        d["_id"]: d["m"] for d in _one(coll, filter={}, projection={"m": {"$meta": "recordId"}})
    }
    rows = _one(coll, filter={}, sort={"a": -1}, projection={"m": {"$meta": "recordId"}})
    assert [d["_id"] for d in rows] == [2, 1]
    assert {d["_id"]: d["m"] for d in rows} == unsorted


def test_meta_does_not_turn_the_projection_into_an_inclusion(coll):
    """The whole document still comes back -- ``$meta`` is a value re-shaper."""
    rows = _one(coll, filter={"_id": 1}, projection={"m": {"$meta": "recordId"}})
    assert set(rows[0]) == {"_id", "a", "b", "arr", "m"}


def test_sort_key_is_the_sort_fields_in_order(coll):
    rows = _one(coll, filter={}, sort={"a": 1, "b": -1}, projection={"m": {"$meta": "sortKey"}})
    assert [d["m"] for d in rows] == [[1, "x"], [3, "y"]]


def test_sort_key_of_a_missing_field_is_null(coll):
    rows = _one(coll, filter={}, sort={"zzz": 1}, projection={"m": {"$meta": "sortKey"}})
    assert [d["m"] for d in rows] == [[None], [None]]


def test_sort_key_of_an_array_is_the_element_the_sort_used(coll):
    """Ascending takes the array's minimum, which is what the sort itself
    compares -- so the reported key explains the order."""
    rows = _one(coll, filter={"_id": 1}, sort={"arr": 1}, projection={"m": {"$meta": "sortKey"}})
    assert rows[0]["m"] == [1]


def test_sort_key_without_a_sort_is_rejected(coll):
    with pytest.raises(OperationFailure) as exc:
        _one(coll, filter={}, projection={"m": {"$meta": "sortKey"}})
    assert exc.value.details["code"] == 2
    assert exc.value.details["errmsg"] == "cannot use sortKey $meta projection without a sort"


def test_index_key_is_emitted_only_for_a_secondary_index_scan(coll):
    coll.create_index([("a", 1), ("b", -1)], name="ab")
    rows = _one(coll, filter={"a": 1}, hint="ab", projection={"m": {"$meta": "indexKey"}})
    assert rows[0]["m"] == {"a": 1, "b": "x"}
    # A collection scan has no index key, and mongod OMITS the field rather
    # than reporting null.
    rows = _one(coll, filter={"zzz": 1}, projection={"m": {"$meta": "indexKey"}})
    assert rows == []
    rows = _one(coll, filter={}, projection={"m": {"$meta": "indexKey"}})
    assert all("m" not in d for d in rows)
    # ... and neither does the `_id` fast path.
    rows = _one(coll, filter={"_id": 1}, projection={"m": {"$meta": "indexKey"}})
    assert "m" not in rows[0]


def test_unknown_meta_field_uses_mongods_wording(coll):
    with pytest.raises(OperationFailure) as exc:
        _one(coll, filter={}, projection={"m": {"$meta": "zzz"}})
    assert exc.value.details["code"] == 17308
    assert exc.value.details["errmsg"] == "Unsupported $meta field: zzz"


def test_text_score_without_a_text_query_still_rejects(coll):
    with pytest.raises(OperationFailure) as exc:
        _one(coll, filter={}, projection={"m": {"$meta": "textScore"}})
    assert exc.value.details["code"] == 40218


def test_a_meta_field_a_server_cannot_compute_is_omitted(coll):
    """Graceful degradation for the metadata SecantusDB has no machinery for
    (text scoring, Atlas Search, $geoNear) -- validated, then absent."""
    rows = _one(coll, filter={}, projection={"m": {"$meta": "searchScore"}})
    assert all("m" not in d for d in rows)
