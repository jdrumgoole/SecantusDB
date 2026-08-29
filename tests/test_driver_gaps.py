"""Integration tests for conformance gaps surfaced by the C / C++ / C# /
PHP driver gauges (tasks/backlog.md §5). Each reproduces a divergence a
specific upstream driver test exercised, driven through pymongo."""

from __future__ import annotations

import pytest
from bson.decimal128 import Decimal128
from pymongo import MongoClient
from pymongo.errors import DuplicateKeyError, OperationFailure, WriteError

from secantus import SecantusDBServer


@pytest.fixture
def server(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield mc
    finally:
        mc.close()


@pytest.fixture
def db(client: MongoClient):
    return client["gapdb"]


# --- $out / $merge must be the final stage (mongo-cxx-driver) ---------------


def test_out_not_last_rejected(db) -> None:
    db.src.insert_one({"x": 1})
    with pytest.raises(OperationFailure) as exc:
        list(db.src.aggregate([{"$project": {"x": 1}}, {"$out": "dest"}, {"$sample": {"size": 1}}]))
    assert exc.value.code == 40601


def test_merge_not_last_rejected(db) -> None:
    db.src.insert_one({"x": 1})
    with pytest.raises(OperationFailure) as exc:
        list(db.src.aggregate([{"$merge": {"into": "dest"}}, {"$limit": 1}]))
    assert exc.value.code == 40601


def test_out_last_still_works(db) -> None:
    db.src.insert_many([{"x": 1}, {"x": 2}])
    list(db.src.aggregate([{"$match": {"x": {"$gte": 1}}}, {"$out": "dest"}]))
    assert db.dest.count_documents({}) == 2


# --- Decimal128 batchSize is honoured (mongo-c-driver) ---------------------


def test_decimal128_batch_size_find(db) -> None:
    db.nums.insert_many([{"i": i} for i in range(10)])
    cur = db.nums.find({}, batch_size=2)
    # Force a Decimal128 batchSize onto the wire via a raw command.
    res = db.command({"find": "nums", "filter": {}, "batchSize": Decimal128("2")})
    assert res["ok"] == 1.0
    assert len(res["cursor"]["firstBatch"]) == 2
    assert list(cur)  # the driver-level cursor still drains fully
    assert db.nums.count_documents({}) == 10


def test_decimal128_batch_size_aggregate(db) -> None:
    db.nums.insert_many([{"i": i} for i in range(10)])
    res = db.command(
        {"aggregate": "nums", "pipeline": [], "cursor": {"batchSize": Decimal128("3")}}
    )
    assert res["ok"] == 1.0
    assert len(res["cursor"]["firstBatch"]) == 3


# --- over-long database name rejected (mongo-c-driver) ---------------------


def test_long_database_name_rejected(client: MongoClient) -> None:
    long_db = client["d" * 64]
    with pytest.raises(OperationFailure) as exc:
        long_db.things.insert_one({"i": 1})
    assert exc.value.code == 73


def test_max_length_database_name_allowed(client: MongoClient) -> None:
    ok_db = client["d" * 63]
    ok_db.things.insert_one({"i": 1})
    assert ok_db.things.count_documents({}) == 1


# --- document-validation errInfo details (mongo-csharp-driver) ------------


def test_validation_errinfo_details(db) -> None:
    db.create_collection("validated", validator={"x": {"$type": "string"}})
    with pytest.raises(WriteError) as exc:
        db.validated.insert_one({"x": 1})
    err = exc.value
    assert err.code == 121
    details = err.details["errInfo"]["details"]
    assert details == {
        "operatorName": "$type",
        "specifiedAs": {"x": {"$type": "string"}},
        "reason": "type did not match",
        "consideredValue": 1,
        "consideredType": "int",
    }
    # the failing doc's _id is surfaced
    assert "failingDocumentId" in err.details["errInfo"]


def test_validation_passes_for_valid_doc(db) -> None:
    db.create_collection("validated", validator={"x": {"$type": "string"}})
    db.validated.insert_one({"x": "hello"})
    assert db.validated.count_documents({}) == 1


# --- change stream with invalid pipeline errors at open (mongo-cxx-driver) -


def test_change_stream_invalid_pipeline_errors_on_open(db) -> None:
    db.create_collection("watched")
    with pytest.raises(OperationFailure):
        # mongocxx asserts this throws at .begin() (aggregate time), not
        # at the first getMore.
        db.watched.watch([{"$match": {"$foo": -1}}])


def test_change_stream_valid_pipeline_opens(db) -> None:
    db.create_collection("watched")
    with db.watched.watch([{"$match": {"operationType": "insert"}}]) as stream:
        db.watched.insert_one({"x": 1})
        change = stream.next()
        assert change["operationType"] == "insert"


# --- $out enforces target validator unless bypassed (mongo-c-driver) -------


def test_out_enforces_target_validator(db) -> None:
    db.create_collection("guarded", validator={"number": {"$gte": 5}}, validationAction="error")
    db.src.insert_one({"number": 1})
    with pytest.raises(OperationFailure) as exc:
        list(db.src.aggregate([{"$out": "guarded"}]))
    assert exc.value.code == 121


def test_out_bypass_skips_target_validator(db) -> None:
    db.create_collection("guarded", validator={"number": {"$gte": 5}}, validationAction="error")
    db.src.insert_one({"number": 1})
    list(db.src.aggregate([{"$out": "guarded"}], bypassDocumentValidation=True))
    assert db.guarded.count_documents({}) == 1


# --- collMod prepareUnique -> unique violations (mongo-c-driver) -----------


def test_collmod_prepare_unique_then_unique_violations(db) -> None:
    db.test.insert_many([{"_id": 1, "x": 1}, {"_id": 2, "x": 1}])
    db.test.create_index([("x", 1)])

    # prepareUnique arms the index: a NEW duplicate insert is now rejected,
    # even though the two pre-existing duplicates remain.
    db.command("collMod", "test", index={"keyPattern": {"x": 1}, "prepareUnique": True})
    with pytest.raises(DuplicateKeyError):
        db.test.insert_one({"_id": 3, "x": 1})

    # Converting to unique is refused with code 359 + the offending ids.
    with pytest.raises(OperationFailure) as exc:
        db.command("collMod", "test", index={"keyPattern": {"x": 1}, "unique": True})
    assert exc.value.code == 359
    assert exc.value.details["violations"] == [{"ids": [1, 2]}]


def test_collmod_convert_unique_succeeds_without_duplicates(db) -> None:
    db.test.insert_many([{"_id": 1, "x": 1}, {"_id": 2, "x": 2}])
    db.test.create_index([("x", 1)])
    db.command("collMod", "test", index={"keyPattern": {"x": 1}, "unique": True})
    # Now the unique constraint is live.
    with pytest.raises(DuplicateKeyError):
        db.test.insert_one({"_id": 3, "x": 1})


# --- getMore errors after the collection is dropped (mongo-c-driver) -------


def test_getmore_errors_after_collection_dropped(db) -> None:
    db.things.insert_many([{"i": i} for i in range(10)])
    cur = db.things.find({}, batch_size=2)
    next(cur)  # drains the first batch entry, registers a server cursor
    db.things.drop()
    # The cursor's server side is gone; draining the rest must raise.
    with pytest.raises(OperationFailure) as exc:
        list(cur)
    assert exc.value.code == 43


# --- an unrecognised string index-key value is rejected (mongo-c-driver) ----


def test_unknown_index_plugin_rejected(db) -> None:
    """A string index-key value names an index plugin; an unrecognised one is
    rejected with CannotCreateIndex (67) "Unknown index plugin '<value>'", like
    mongod. mongo-c-driver's /Collection/index_w_write_concern creates an index
    ``{abc: "hallo thar"}`` and asserts the server rejects it."""
    with pytest.raises(OperationFailure) as exc:
        db.command(
            "createIndexes",
            "c",
            indexes=[{"key": {"abc": "hallo thar"}, "name": "abc_bad"}],
        )
    assert exc.value.code == 67
    assert "Unknown index plugin" in str(exc.value)
    # The valid geo plugins and numeric directions are still accepted.
    db.command("createIndexes", "c", indexes=[{"key": {"loc": "2dsphere"}, "name": "loc_2d"}])
    db.command("createIndexes", "c", indexes=[{"key": {"n": -1}, "name": "n_-1"}])
