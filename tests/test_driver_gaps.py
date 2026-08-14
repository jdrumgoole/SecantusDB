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
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
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


# --- validationAction warn/off accepts violating writes --------------------


@pytest.mark.parametrize("action", ["warn", "off"])
def test_validation_action_warn_and_off_accept_violating_writes(db, action) -> None:
    """``warn`` / ``off`` mean "store it anyway" on every write path.

    Only ``validationAction: "error"`` (the default) rejects. Enforcing
    under ``warn`` would break the standard way of staging a validator
    against live traffic — writes would hard-fail with 121 while the
    operator believed they were merely being logged.
    """
    db.create_collection("staged", validator={"number": {"$gte": 5}}, validationAction=action)

    db.staged.insert_one({"_id": 1, "number": 1})
    assert db.staged.find_one({"_id": 1})["number"] == 1

    db.staged.update_one({"_id": 1}, {"$set": {"number": 2}})
    assert db.staged.find_one({"_id": 1})["number"] == 2

    db.staged.find_one_and_update({"_id": 1}, {"$set": {"number": 3}})
    assert db.staged.find_one({"_id": 1})["number"] == 3

    db.staged.replace_one({"_id": 1}, {"number": 4})
    assert db.staged.find_one({"_id": 1})["number"] == 4


def test_validation_action_error_still_rejects(db) -> None:
    """The default path must be untouched by the warn/off carve-out."""
    db.create_collection("strict", validator={"number": {"$gte": 5}}, validationAction="error")
    with pytest.raises(OperationFailure) as exc:
        db.strict.insert_one({"number": 1})
    assert exc.value.code == 121


def test_collmod_persists_validation_action_and_level(db) -> None:
    """``collMod`` must apply these, not accept-and-discard them.

    Previously the command replied ``ok: 1`` and dropped both options, so a
    caller relaxing enforcement got a success reply and unchanged behaviour.
    """
    db.create_collection("c", validator={"number": {"$gte": 5}})
    db.command({"collMod": "c", "validationAction": "warn", "validationLevel": "moderate"})

    opts = next(iter(db.list_collections(filter={"name": "c"})))["options"]
    assert opts["validationAction"] == "warn"
    assert opts["validationLevel"] == "moderate"

    # And the change is live, not merely recorded.
    db.c.insert_one({"_id": 1, "number": 1})
    assert db.c.find_one({"_id": 1}) is not None

    # Flipping back to error re-arms enforcement.
    db.command({"collMod": "c", "validationAction": "error"})
    with pytest.raises(OperationFailure) as exc:
        db.c.insert_one({"_id": 2, "number": 1})
    assert exc.value.code == 121


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


# --- validationLevel: off / moderate / strict ------------------------------


def test_validation_level_off_disables_validation(db) -> None:
    """``validationLevel: "off"`` means no validation, whatever the action says.

    The level was stored by ``create`` / ``collMod`` and then never consulted,
    so a collection explicitly opted OUT still had its validator enforced.
    """
    db.create_collection("lvl_off", validator={"n": {"$gte": 0}}, validationLevel="off")
    db.lvl_off.insert_one({"n": -5})
    assert db.lvl_off.count_documents({}) == 1


def test_validation_level_moderate_exempts_already_invalid_docs(db) -> None:
    """``moderate`` lets a pre-existing invalid document keep being updated.

    That is the level's whole purpose: add a validator to a collection holding
    legacy rows without freezing them. Under ``strict`` those rows become
    un-updatable, which is what we did before.
    """
    db.create_collection("lvl_mod", validationLevel="moderate")
    db.lvl_mod.insert_one({"_id": 1, "n": -5})  # legacy row, no validator yet
    db.command({"collMod": "lvl_mod", "validator": {"n": {"$gte": 0}}})

    db.lvl_mod.update_one({"_id": 1}, {"$set": {"tag": "x"}})
    assert db.lvl_mod.find_one({"_id": 1})["tag"] == "x"


def test_validation_level_moderate_still_protects_valid_docs(db) -> None:
    """A doc that currently SATISFIES the validator is still held to it.

    Without this, ``moderate`` would read as "validation off for updates" and
    an update could silently turn a valid document invalid.
    """
    db.create_collection("lvl_mod2", validator={"n": {"$gte": 0}}, validationLevel="moderate")
    db.lvl_mod2.insert_one({"_id": 1, "n": 5})
    with pytest.raises(OperationFailure) as exc:
        db.lvl_mod2.update_one({"_id": 1}, {"$set": {"n": -1}})
    assert exc.value.code == 121


def test_validation_level_moderate_still_validates_inserts(db) -> None:
    """``moderate`` exempts UPDATES of invalid docs, never inserts.

    mongod validates every insert at ``moderate``; only the update path
    consults the pre-image. An upsert-inserted document counts as an insert.
    """
    db.create_collection("lvl_mod3", validator={"n": {"$gte": 0}}, validationLevel="moderate")
    with pytest.raises(OperationFailure) as exc:
        db.lvl_mod3.insert_one({"n": -1})
    assert exc.value.code == 121


def test_validation_level_strict_is_the_default(db) -> None:
    """Omitting the level keeps the pre-existing strict behaviour."""
    db.create_collection("lvl_strict", validator={"n": {"$gte": 0}})
    db.lvl_strict.insert_one({"_id": 1, "n": 5})
    with pytest.raises(OperationFailure) as exc:
        db.lvl_strict.update_one({"_id": 1}, {"$set": {"n": -1}})
    assert exc.value.code == 121


def test_validation_level_moderate_on_multi_update(db) -> None:
    """The multi-document path enforces separately from the single-doc one.

    Storage routes ``multi: true`` through a chunked writer with its OWN
    validator check; patching only one path left single-document updates (the
    common case) still rejecting while the multi path looked correct.
    """
    db.create_collection("lvl_multi", validationLevel="moderate")
    db.lvl_multi.insert_many([{"_id": 1, "n": -5}, {"_id": 2, "n": -7}])
    db.command({"collMod": "lvl_multi", "validator": {"n": {"$gte": 0}}})

    db.lvl_multi.update_many({}, {"$set": {"tag": "x"}})
    assert db.lvl_multi.count_documents({"tag": "x"}) == 2
