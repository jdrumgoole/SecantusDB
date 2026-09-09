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


# --- $setWindowFields output order ----------------------------------------


def test_setwindowfields_emits_in_partition_then_sort_order(coll):
    """NOT the input order. mongod emits partition by partition, in first-seen
    partition order, and within each partition in `sortBy` order.

    The docstring used to claim the opposite -- "original input order is
    preserved, the partition / sort dance happens only to compute the new
    fields" -- and the code implemented that. Wrong order is wrong RESULTS as
    soon as a `$limit` follows. Measured 8.2.11, 2026-09-09.
    """
    coll.insert_many(
        [{"_id": i, "g": g, "v": v} for i, (g, v) in enumerate([("a", 3), ("b", 1), ("a", 2)])]
    )
    rows = coll.aggregate(
        [
            {
                "$setWindowFields": {
                    "partitionBy": "$g",
                    "sortBy": {"v": 1},
                    "output": {"n": {"$sum": 1}},
                }
            }
        ]
    )
    # Partition "a" (first seen) sorted by v -> _id 2 then 0; then "b" -> 1.
    assert [r["_id"] for r in rows] == [2, 0, 1]


def test_setwindowfields_without_a_sortby_keeps_input_order_within_partitions(coll):
    coll.insert_many([{"_id": i, "g": g} for i, g in enumerate(["a", "b", "a", "b"])])
    rows = coll.aggregate(
        [{"$setWindowFields": {"partitionBy": "$g", "output": {"n": {"$sum": 1}}}}]
    )
    assert [r["_id"] for r in rows] == [0, 2, 1, 3]


# --- $count as an accumulator ---------------------------------------------


def test_count_is_an_accumulator_in_group_and_setwindowfields(coll):
    """`{$count: {}}` answered `168 Unrecognized expression` in a `$group`.

    The accumulator existed and the engine evaluated it correctly all along --
    nothing ever reached it. The constant FOLDER got there first: `{$count: {}}`
    reads no field, so it looked like a constant expression and was handed to
    `evaluate`, which does not know `$count` as an expression.
    """
    coll.insert_many([{"_id": 1, "g": "a"}, {"_id": 2, "g": "a"}])
    grouped = list(coll.aggregate([{"$group": {"_id": "$g", "n": {"$count": {}}}}]))
    assert grouped == [{"_id": "a", "n": 2}]
    windowed = list(coll.aggregate([{"$setWindowFields": {"output": {"n": {"$count": {}}}}}]))
    assert [r["n"] for r in windowed] == [2, 2]


@pytest.mark.parametrize("stage", ["$project", "$addFields"])
def test_count_is_not_an_expression(coll, stage):
    coll.insert_one({"_id": 1, "g": "a"})
    with pytest.raises(pymongo.errors.OperationFailure) as excinfo:
        list(coll.aggregate([{stage: {"n": {"$count": {}}}}]))
    assert excinfo.value.code == 168


def test_the_count_stage_still_works_including_inside_facet(coll):
    """`{$count: "<field>"}` is the STAGE, not the accumulator -- the value's
    type is what separates them. Gating on the name alone rejected this."""
    coll.insert_many([{"_id": 1}, {"_id": 2}])
    assert list(coll.aggregate([{"$count": "n"}])) == [{"n": 2}]
    assert list(coll.aggregate([{"$facet": {"t": [{"$count": "n"}]}}])) == [{"t": [{"n": 2}]}]


# --- $project's surviving parent ------------------------------------------


@pytest.mark.parametrize(
    "sub,expected",
    [
        # The parent is a document: it survives, EMPTY, when the leaf is absent.
        ({}, {}),
        ({"j": 2}, {}),
        ({"k": 1}, {"k": 1}),
        # An array of documents is pruned element-wise; scalars become `[]`.
        ([{"k": 1}, {"j": 2}], [{"k": 1}, {}]),
        ([], []),
        ([1, 2], []),
    ],
)
def test_a_dotted_project_emits_the_surviving_parent(coll, sub, expected):
    """`find`'s projection had this right; the `$project` STAGE was a separate
    implementation that only checked the leaf, so the parent vanished. It now
    delegates, so there is one implementation of the rule."""
    coll.delete_many({})
    coll.insert_one({"_id": 1, "sub": sub})
    assert list(coll.aggregate([{"$project": {"sub.k": 1}}])) == [{"_id": 1, "sub": expected}]


@pytest.mark.parametrize("sub", [5, None])
def test_a_non_document_parent_is_dropped(coll, sub):
    coll.delete_many({})
    coll.insert_one({"_id": 1, "sub": sub})
    assert list(coll.aggregate([{"$project": {"sub.k": 1}}])) == [{"_id": 1}]


# --- $bucket / $bucketAuto over the numeric types -------------------------


def _numbers(coll):
    coll.delete_many({})
    coll.insert_many(
        [
            {"_id": "int1", "v": 1},
            {"_id": "int2", "v": 2},
            {"_id": "dbl15", "v": 1.5},
            {"_id": "negzero", "v": -0.0},
            {"_id": "dec15", "v": Decimal128("1.5")},
            {"_id": "long", "v": 2**40},
            {"_id": "nan", "v": float("nan")},
            {"_id": "inf", "v": float("inf")},
        ]
    )


def test_bucket_places_a_decimal_with_the_other_numerics(coll):
    """`Decimal128("1.5")` landed in `default` because `1 <= Decimal128("1.5")`
    raises `TypeError` in Python and the placement loop swallowed it."""
    _numbers(coll)
    rows = coll.aggregate(
        [
            {
                "$bucket": {
                    "groupBy": "$v",
                    "boundaries": [0, 1, 2, 100],
                    "default": "other",
                    "output": {"ids": {"$push": "$_id"}},
                }
            }
        ]
    )
    buckets = {repr(r["_id"]): sorted(r["ids"]) for r in rows}
    assert buckets["1"] == ["dbl15", "dec15", "int1"]
    assert buckets["'other'"] == ["inf", "long", "nan"]


@pytest.mark.parametrize(
    "buckets,expected",
    [
        # Equal values never straddle a boundary -- `1.5` and
        # `Decimal128("1.5")` are the SAME value, which Python's `==` denies --
        # and the remainder goes to the EARLIER buckets (8 into 3 is 3/3/2).
        (2, [5, 3]),
        (3, [3, 3, 2]),
        (4, [2, 3, 2, 1]),
    ],
)
def test_bucketauto_keeps_equal_values_together(coll, buckets, expected):
    _numbers(coll)
    rows = list(coll.aggregate([{"$bucketAuto": {"groupBy": "$v", "buckets": buckets}}]))
    assert [r["count"] for r in rows] == expected


def test_bucketauto_max_is_the_next_buckets_first_value(coll):
    """Computed from the EXTENDED chunk: a bucket that grew to keep equal values
    together used to report the value it had just absorbed."""
    _numbers(coll)
    rows = list(coll.aggregate([{"$bucketAuto": {"groupBy": "$v", "buckets": 2}}]))
    assert rows[0]["_id"]["max"] == 2
