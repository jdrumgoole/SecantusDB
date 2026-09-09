"""What aggregation stages EMIT, versus what they reject.

`aggregation_stage_specs.py` and its tests cover the errors. These cover the
documents, which is where a silently wrong answer lives — the pipeline
succeeds, the shape looks right, and the rows are wrong. Found by
`tools/probes/aggregation_stage_results.py` on its first run (2026-09-09).
"""

from __future__ import annotations

import pymongo
import pytest
from bson import Decimal128, MaxKey, MinKey

from secantus import SecantusDBServer


@pytest.fixture
def coll(tmp_path):
    server = SecantusDBServer(port=0, storage_path=str(tmp_path / "store"))
    server.start()
    host, port = server.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    try:
        yield client["aggres"]["c"]
    finally:
        client.close()
        server.stop()


# --- $group keys: a bool is not a number ----------------------------------


def test_a_bool_group_key_does_not_join_the_numbers(coll):
    """`True` is its own bucket, `1` and `1.0` share one, and the signed zeros
    share another.

    Python's `hash(True) == hash(1)` and `True == 1`, so the bucketing dict
    merged them: six documents collapsed into four buckets where mongod gives
    four different ones. The Rust server was worse — it keyed by TRUTHINESS,
    giving two. Measured 8.2.11, 2026-09-09.
    """
    coll.insert_many(
        [
            {"_id": 1, "v": True},
            {"_id": 2, "v": 1},
            {"_id": 3, "v": False},
            {"_id": 4, "v": 0},
            {"_id": 5, "v": 1.0},
            {"_id": 6, "v": 0.0},
            {"_id": 7, "v": -0.0},
        ]
    )
    buckets = {
        repr(r["_id"]): sorted(r["ids"])
        for r in coll.aggregate([{"$group": {"_id": "$v", "ids": {"$push": "$_id"}}}])
    }
    assert buckets == {
        "True": [1],
        "1": [2, 5],
        "False": [3],
        "0": [4, 6, 7],
    }


def test_a_bool_group_key_keeps_its_type_in_the_output(coll):
    """The emitted `_id` is the bool, not `1` — it used to be coerced."""
    coll.insert_many([{"_id": 1, "v": True}, {"_id": 2, "v": False}])
    ids = [r["_id"] for r in coll.aggregate([{"$group": {"_id": "$v", "n": {"$sum": 1}}}])]
    assert sorted(ids, key=repr) == [False, True]
    assert all(isinstance(i, bool) for i in ids)


def test_the_numeric_types_still_share_a_bucket(coll):
    """The fix must not separate int from double, which mongod does merge."""
    coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 1.0}, {"_id": 3, "v": Decimal128("1")}])
    rows = list(coll.aggregate([{"$group": {"_id": "$v", "n": {"$sum": 1}}}]))
    assert len(rows) == 1 and rows[0]["n"] == 3


# --- $sort over values the sort gate used to refuse ------------------------


@pytest.mark.parametrize(
    "value,label",
    [(float("nan"), "nan"), (MinKey(), "minkey"), (MaxKey(), "maxkey")],
)
def test_sorting_a_collection_holding_these_does_not_fail(coll, value, label):
    """A plain `{$sort: {v: 1}}` used to answer `2 ... not supported` on the
    Rust server for any collection holding one of these.

    Ordinary data, ordinary query, whole pipeline refused — and it took
    `$group`, `$bucket` and `$topN` down with it, since they sort too.
    """
    coll.insert_many([{"_id": 1, "v": value}, {"_id": 2, "v": 0}])
    rows = list(coll.aggregate([{"$sort": {"v": 1}}]))
    assert len(rows) == 2


def test_the_sort_order_of_those_values_is_mongods(coll):
    """NaN and MinKey rank below a number; MaxKey ranks above."""
    coll.insert_many(
        [
            {"_id": "zero", "v": 0},
            {"_id": "nan", "v": float("nan")},
            {"_id": "minkey", "v": MinKey()},
            {"_id": "maxkey", "v": MaxKey()},
        ]
    )
    order = [r["_id"] for r in coll.aggregate([{"$sort": {"v": 1, "_id": 1}}])]
    assert order == ["minkey", "nan", "zero", "maxkey"]
