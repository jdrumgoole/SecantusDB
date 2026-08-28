"""Numeric and cursor command arguments are parse errors, not crashes.

The document-valued sweep landed first; extending it to other argument classes
found 24 more crashes. This covers them.

**mongod's strictness is per-slot, not per-class**, and that is the whole point
of this file. ``find.limit: {}`` is a type error, while the analogous
``delete.deletes.limit: {}`` is ACCEPTED and means "no limit". A blanket
"validate every numeric argument" rule would have fixed the first and broken the
second. Every expectation here was probed individually against mongod 6.0.16.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer

NOT_NUMBERS = [{}, "x", [1], True]


@pytest.fixture
def db(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    d = cli["argnum"]
    d.c.insert_one({"_id": 1, "a": 1})
    try:
        yield d
    finally:
        cli.close()
        srv.stop()


def _err(db, cmd):
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command(dict(cmd))
    assert exc.value.code != 1, f"crashed instead of parsing: {exc.value}"
    return exc.value


@pytest.mark.parametrize("bad", NOT_NUMBERS)
@pytest.mark.parametrize("field", ["limit", "skip", "batchSize"])
def test_find_numeric_slots(db, field, bad) -> None:
    """mongod reports these under its IDL name, `FindCommandRequest.limit`,
    not `find.limit` -- probed, not guessed."""
    err = _err(db, {"find": "c", field: bad})
    assert err.code == 14
    assert err.details["errmsg"] == (
        f"BSON field 'FindCommandRequest.{field}' is the wrong type "
        f"'{'bool' if isinstance(bad, bool) else {dict: 'object', str: 'string', list: 'array'}[type(bad)]}', "
        "expected types '[long, int, decimal, double']"
    )


@pytest.mark.parametrize("bad", NOT_NUMBERS)
def test_aggregate_cursor_batch_size(db, bad) -> None:
    err = _err(db, {"aggregate": "c", "pipeline": [], "cursor": {"batchSize": bad}})
    assert err.code == 14
    assert err.details["errmsg"].startswith("BSON field 'cursor.batchSize' is the wrong type")


@pytest.mark.parametrize("bad", ["x", [1], 5])
def test_aggregate_cursor_must_be_object(db, bad) -> None:
    """This slot has its own wording, not the BSON-field form."""
    err = _err(db, {"aggregate": "c", "pipeline": [], "cursor": bad})
    assert err.code == 14
    assert err.details["errmsg"] == "cursor field must be missing or an object"


@pytest.mark.parametrize("bad", ["x", [1], 5])
def test_list_indexes_cursor_must_be_object(db, bad) -> None:
    err = _err(db, {"listIndexes": "c", "cursor": bad})
    assert err.code == 14
    assert "BSON field 'listIndexes.cursor' is the wrong type" in err.details["errmsg"]
    assert "expected type 'object'" in err.details["errmsg"]


@pytest.mark.parametrize("bad", [5, "x", {}, True])
def test_create_indexes_indexes_must_be_array(db, bad) -> None:
    err = _err(db, {"createIndexes": "c", "indexes": bad})
    assert err.code == 14
    assert "BSON field 'createIndexes.indexes' is the wrong type" in err.details["errmsg"]
    assert "expected type 'array'" in err.details["errmsg"]


@pytest.mark.parametrize("bad", [5, "x", True])
def test_match_stage_spec_must_be_an_object(db, bad) -> None:
    err = _err(db, {"aggregate": "c", "pipeline": [{"$match": bad}], "cursor": {}})
    assert err.code == 15959
    assert err.details["errmsg"] == "the match filter must be an expression in an object"


@pytest.mark.parametrize("bad", [{}, "x", [1], 0, True])
def test_delete_limit_is_deliberately_tolerant(db, bad) -> None:
    """mongod does NOT type-check this slot: anything that isn't a numeric 1
    means "no limit". `true` counts as not-1 even though Python makes bool an
    int. We used to call int() on it and crash."""
    db.c.delete_many({})
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 1}])
    reply = db.command({"delete": "c", "deletes": [{"q": {"a": 1}, "limit": bad}]})
    assert reply["n"] == 2, f"limit={bad!r} should delete every match"
    assert db.c.count_documents({}) == 0


def test_delete_limit_one_still_limits(db) -> None:
    db.c.delete_many({})
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 1}])
    reply = db.command({"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 1}]})
    assert reply["n"] == 1
    assert db.c.count_documents({}) == 1


def test_valid_numeric_arguments_still_work(db) -> None:
    db.c.delete_many({})
    db.c.insert_many([{"_id": i} for i in range(5)])
    assert len(db.command({"find": "c", "limit": 2})["cursor"]["firstBatch"]) == 2
    assert len(db.command({"find": "c", "skip": 3})["cursor"]["firstBatch"]) == 2
    assert len(db.command({"find": "c", "batchSize": 2})["cursor"]["firstBatch"]) == 2
    # A float limit is accepted by mongod.
    assert len(db.command({"find": "c", "limit": 2.0})["cursor"]["firstBatch"]) == 2
    assert db.command({"aggregate": "c", "pipeline": [], "cursor": {"batchSize": 2}})["ok"] == 1
    assert db.command({"listIndexes": "c", "cursor": {}})["ok"] == 1
