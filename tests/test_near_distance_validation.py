"""``$near`` / ``$nearSphere`` distance bounds are validated like mongod's.

``_opt_number`` could not tell "key absent" from "key present with null", so
``{$near: {..., $minDistance: null}}`` ran unbounded instead of being rejected,
and negative bounds were accepted outright. Strings and bools were already
refused, which is why this looked covered.

Every expectation was probed against mongod 8.3.4 with a 2dsphere index.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer

GEOM = {"type": "Point", "coordinates": [1.01, 1.01]}


@pytest.fixture
def coll(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    db = cli["neard"]
    db.c.create_index([("geo", "2dsphere")])
    db.c.insert_one({"_id": 1, "geo": {"type": "Point", "coordinates": [1.0, 1.0]}})
    try:
        yield db.c
    finally:
        cli.close()
        srv.stop()


@pytest.mark.parametrize("op", ["$near", "$nearSphere"])
@pytest.mark.parametrize("key", ["$minDistance", "$maxDistance"])
@pytest.mark.parametrize("bad", [None, "x", True])
def test_non_numeric_bound_is_rejected(coll, op, key, bad) -> None:
    """``null`` included -- an explicit null is NOT the same as an absent key."""
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(coll.find({"geo": {op: {"$geometry": GEOM, key: bad}}}))
    assert exc.value.code == 2
    assert f"{key} must be a number" in str(exc.value)


@pytest.mark.parametrize("op", ["$near", "$nearSphere"])
@pytest.mark.parametrize("key", ["$minDistance", "$maxDistance"])
def test_negative_bound_is_rejected(coll, op, key) -> None:
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(coll.find({"geo": {op: {"$geometry": GEOM, key: -1}}}))
    assert exc.value.code == 2
    assert f"{key} must be non-negative" in str(exc.value)


@pytest.mark.parametrize(
    "spec",
    [
        {"$geometry": GEOM},  # no bounds at all
        {"$geometry": GEOM, "$minDistance": 0},  # zero is valid
        {"$geometry": GEOM, "$maxDistance": 10000},
        {"$geometry": GEOM, "$minDistance": 0, "$maxDistance": 10000},
    ],
)
def test_valid_bounds_still_work(coll, spec) -> None:
    """An ABSENT key must keep meaning 'unbounded' -- the fix distinguishes it
    from an explicit null rather than rejecting both."""
    assert [d["_id"] for d in coll.find({"geo": {"$near": spec}})] == [1]


def test_bound_still_filters(coll) -> None:
    """The validation must not accidentally disable the bound itself."""
    coll.insert_one({"_id": 2, "geo": {"type": "Point", "coordinates": [40.0, 40.0]}})
    near = {"$geometry": GEOM, "$maxDistance": 10000}
    assert [d["_id"] for d in coll.find({"geo": {"$near": near}})] == [1]


@pytest.fixture
def legacy_coll(wt_home):
    """The legacy pair form needs a 2d index and `[x, y]` document geometry."""
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    db = cli["nearlegacy"]
    db.c.create_index([("geo", "2d")])
    db.c.insert_one({"_id": 1, "geo": [1.0, 1.0]})
    try:
        yield db.c
    finally:
        cli.close()
        srv.stop()


@pytest.mark.parametrize(
    "key,code",
    [
        # The legacy sibling form has its OWN codes, not the nested form's
        # BadValue (2). Probed on mongod 8.3.4.
        ("$maxDistance", 16895),
        ("$minDistance", 16893),
    ],
)
@pytest.mark.parametrize("bad", [None, "x", True])
def test_legacy_sibling_bounds_use_their_own_codes(legacy_coll, key, code, bad) -> None:
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(legacy_coll.find({"geo": {"$near": [1.01, 1.01], key: bad}}))
    assert exc.value.code == code, f"{key}={bad!r}"
    assert f"{key} must be a number" in str(exc.value)


def test_legacy_sibling_numeric_bound_still_works(legacy_coll) -> None:
    assert [
        d["_id"] for d in legacy_coll.find({"geo": {"$near": [1.01, 1.01], "$maxDistance": 10}})
    ] == [1]
