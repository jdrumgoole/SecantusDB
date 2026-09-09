"""How a sort resolves a dotted path -- the two rules measured on mongod 8.2.11.

Companion to ``tools/probes/sort_path_resolution.py``. Both rules are about the
SORT walk specifically; the same paths in a filter, a ``$group`` ``_id`` or a
projection behave differently and are not asserted here.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture
def coll(tmp_path):
    server = SecantusDBServer(port=0, storage_path=str(tmp_path / "store"))
    server.start()
    host, port = server.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    try:
        yield client["sortpath"]["c"]
    finally:
        client.close()
        server.stop()


def _ids(coll, key, direction=1, agg=False):
    spec = [(key, direction), ("_id", 1)]
    if agg:
        rows = coll.aggregate([{"$sort": dict(spec)}, {"$project": {"_id": 1}}])
    else:
        rows = coll.find({}, {"_id": 1}).sort(spec)
    return [r["_id"] for r in rows]


# --- resolution ------------------------------------------------------------


@pytest.mark.parametrize("agg", [False, True], ids=["find", "aggregate"])
def test_index_component_does_not_descend_the_element(coll, agg):
    """``x.0`` over ``[[5]]`` sorts by the ARRAY ``[5]``, not by ``5``.

    An array reached by an explicit index is the sort key as it stands. Both
    servers descended it and ranked the document among the NUMBERS, which is
    wrong order -- and wrong results under a ``limit``.
    """
    coll.insert_many(
        [
            {"_id": 0, "x": [[5]]},
            {"_id": 1, "x": [6]},
            {"_id": 2, "x": ["zz"]},
            {"_id": 3, "x": [[4]]},
        ]
    )
    # number < string < array, and [4] < [5].
    assert _ids(coll, "x.0", agg=agg) == [1, 2, 3, 0]


@pytest.mark.parametrize("agg", [False, True], ids=["find", "aggregate"])
def test_field_component_descends_one_level(coll, agg):
    """``x.y`` over ``[{y: [1, 2]}]`` sorts by ``1`` -- the array IS descended."""
    coll.insert_many(
        [
            {"_id": 0, "x": [{"y": [1, 2]}]},
            {"_id": 1, "x": {"y": 6}},
            {"_id": 2, "x": {"y": "zz"}},
            {"_id": 3, "x": {"y": [4]}},
        ]
    )
    # probe 1, sentinel3 4 (also descended), sentinel1 6, sentinel2 "zz".
    assert _ids(coll, "x.y", agg=agg) == [0, 3, 1, 2]


@pytest.mark.parametrize("agg", [False, True], ids=["find", "aggregate"])
def test_one_level_only(coll, agg):
    """``x`` over ``[[5]]`` descends once to ``[5]`` and stops."""
    coll.insert_many(
        [{"_id": 0, "x": [[5]]}, {"_id": 1, "x": 6}, {"_id": 2, "x": "zz"}, {"_id": 3, "x": [4]}]
    )
    assert _ids(coll, "x", agg=agg) == [3, 1, 2, 0]


def test_descending_picks_the_other_representative(coll):
    """A multi-valued path sorts by max descending, min ascending."""
    coll.insert_many([{"_id": 0, "x": [{"y": 5}, {"y": 1}]}, {"_id": 1, "x": {"y": 3}}])
    assert _ids(coll, "x.y", direction=1) == [0, 1]  # 1 < 3
    assert _ids(coll, "x.y", direction=-1) == [0, 1]  # 5 > 3


# --- ambiguity -------------------------------------------------------------

AMBIGUOUS = [
    ([{"0": 5}], "x.0"),
    ([{"0": 5}, {"1": 6}], "x.0"),
    ([{"0": 5}, {"1": 6}], "x.1"),
    # The element carrying the key need not be the one at that index.
    ([{"a": 5}, {"0": 6}], "x.0"),
    ([5, {"0": 1}], "x.0"),
    ([{"1": 5}, {"a": 1}], "x.1"),
    # Nested: the inner array is the ambiguous one.
    ([[{"0": 5}]], "x.0.0"),
]

ALLOWED = [
    # Index 1 is past the end, so only the field reading exists.
    ([{"1": 5}], "x.1"),
    ([{"0": 5}], "x.1"),
    # "00" is not the key "0".
    ([{"00": 5}], "x.0"),
    ([{"-1": 5}], "x.0"),
    # No element carries the key at all.
    ([{"1": 5}, {"a": 1}], "x.0"),
    ([{"2": 5}, {"a": 1}, {"b": 2}], "x.0"),
    ([5, {"0": 1}], "x.1"),
    ([[5]], "x.0"),
]


@pytest.mark.parametrize("array,key", AMBIGUOUS)
@pytest.mark.parametrize("agg", [False, True], ids=["find", "aggregate"])
def test_ambiguous_sort_path_is_refused(coll, array, key, agg):
    coll.insert_one({"_id": 1, "x": array})
    with pytest.raises(pymongo.errors.OperationFailure) as excinfo:
        _ids(coll, key, agg=agg)
    assert excinfo.value.code == 16746
    errmsg = excinfo.value.details["errmsg"]
    assert "Ambiguous field name found in array" in errmsg
    assert f"field: '{key.split('.')[-1]}'" in errmsg
    # mongod's executor wrapper names the command and the namespace.
    if agg:
        assert errmsg.startswith(
            "Executor error during aggregate command on namespace: sortpath.c :: caused by :: "
        )
    else:
        assert errmsg.startswith("Executor error during find command: sortpath.c :: caused by :: ")


@pytest.mark.parametrize("array,key", ALLOWED)
@pytest.mark.parametrize("agg", [False, True], ids=["find", "aggregate"])
def test_unambiguous_sort_path_is_allowed(coll, array, key, agg):
    coll.insert_one({"_id": 1, "x": array})
    assert _ids(coll, key, agg=agg) == [1]


def test_ambiguity_is_per_document(coll):
    """Any document in the collection can trigger it, not just the first."""
    coll.insert_many([{"_id": 1, "x": [1, 2]}, {"_id": 2, "x": [{"0": 5}]}])
    with pytest.raises(pymongo.errors.OperationFailure) as excinfo:
        _ids(coll, "x.0")
    assert excinfo.value.code == 16746


def test_ambiguous_path_is_fine_in_a_filter(coll):
    """mongod refuses the path only for a SORT -- a filter resolves both."""
    coll.insert_one({"_id": 1, "x": [{"0": 5}]})
    assert [d["_id"] for d in coll.find({"x.0": 5})] == [1]
    assert [d["_id"] for d in coll.find({"x.0": {"0": 5}})] == [1]
