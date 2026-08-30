"""Cursor / getMore / killCursors argument handling matches mongod's.

Phase 2 of ``tasks/remaining-work-plan.md``: 51 cursor shapes probed against a
live mongod 6.0.16, of which **22 diverged**. Four were crash-class -- a
malformed argument reached a bare ``int()`` and the ``ValueError`` /
``TypeError`` escaped as ``internal server error`` (code 1):

* ``getMore`` with a string cursor id or a string ``batchSize``;
* ``killCursors`` with a non-array ``cursors``, or a wrong-typed element.

The rest were arguments accepted and ignored where mongod rejects them, or
``CursorNotFound`` (43) returned for what mongod reports as a parse error
before it ever looks a cursor up -- a plausible-looking lie, since the cursor
in question existed.

Every expectation here was probed against mongod 6.0.16.
"""

from __future__ import annotations

import pymongo
import pytest
from bson import Decimal128, Int64

from secantus import SecantusDBServer


@pytest.fixture
def db(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        d = cli["cur"]
        d.c.insert_many([{"_id": i} for i in range(1, 11)])
        yield d
    finally:
        cli.close()
        srv.stop()


def _open(db, batch_size=2):
    return db.command({"find": "c", "batchSize": batch_size})["cursor"]["id"]


def _err(db, cmd):
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command(cmd)
    return exc.value


NUMERIC_TYPES = "expected types '[decimal, int, double, long]'"


# --- the crashes ------------------------------------------------------------


@pytest.mark.parametrize(
    "cmd",
    [
        {"getMore": "x", "collection": "c"},
        {"killCursors": "c", "cursors": 5},
        {"killCursors": "c", "cursors": ["x"]},
    ],
)
def test_malformed_cursor_arguments_do_not_crash(db, cmd) -> None:
    """Each of these raised a bare ValueError / TypeError that escaped as
    ``internal server error``."""
    err = _err(db, cmd)
    assert err.code != 1, "must not be an unhandled exception"
    assert "internal server error" not in str(err)
    assert err.code == 14


def test_getmore_batchsize_string_does_not_crash(db) -> None:
    err = _err(db, {"getMore": _open(db), "collection": "c", "batchSize": "x"})
    assert err.code == 14
    assert err.details["errmsg"] == (
        f"BSON field 'getMore.batchSize' is the wrong type 'string', {NUMERIC_TYPES}"
    )


# --- negative sizing values -------------------------------------------------


@pytest.mark.parametrize("field", ["batchSize", "limit", "skip"])
def test_find_rejects_negative_sizing(db, field) -> None:
    """Silently accepted before: a negative ``batchSize`` fell through
    ``or DEFAULT`` and became the default, and a negative ``limit`` returned
    the whole collection."""
    err = _err(db, {"find": "c", field: -3})
    assert err.code == 2
    assert err.details["codeName"] == "BadValue"
    # The bare field name, not the IDL path the type error uses.
    assert err.details["errmsg"] == f"BSON field '{field}' value must be >= 0, actual value '-3'"


def test_getmore_rejects_negative_batchsize(db) -> None:
    err = _err(db, {"getMore": _open(db), "collection": "c", "batchSize": -1})
    assert err.code == 2


def test_aggregate_rejects_negative_batchsize(db) -> None:
    err = _err(db, {"aggregate": "c", "pipeline": [], "cursor": {"batchSize": -1}})
    assert err.code == 2


@pytest.mark.parametrize("value", [0, 3, Int64(3), 3.0, 2.5, Decimal128("3"), None])
def test_non_negative_numbers_are_still_accepted(db, value) -> None:
    """The range check must not narrow the accepted TYPES: mongod takes int,
    long, double, decimal and null here, and truncates a fractional double."""
    reply = db.command({"find": "c", "batchSize": value})
    assert reply["ok"] == 1.0


def test_fractional_batch_size_truncates(db) -> None:
    reply = db.command({"find": "c", "batchSize": 2.5})
    assert len(reply["cursor"]["firstBatch"]) == 2


# --- getMore's required / typed fields --------------------------------------


def test_getmore_id_must_be_a_long(db) -> None:
    """An int32 cursor id is refused -- the same int64 strictness the Go and C
    drivers enforce on the reply side."""
    err = _err(db, {"getMore": 5, "collection": "c"})
    assert err.code == 14
    assert err.details["errmsg"] == (
        "BSON field 'getMore.getMore' is the wrong type 'int', expected type 'long'"
    )


def test_getmore_requires_a_collection(db) -> None:
    err = _err(db, {"getMore": _open(db)})
    assert err.code == 40414
    assert err.details["errmsg"] == (
        "BSON field 'getMore.collection' is missing but a required field"
    )


def test_getmore_collection_must_be_a_string(db) -> None:
    err = _err(db, {"getMore": _open(db), "collection": 5})
    assert err.code == 14
    assert "expected type 'string'" in err.details["errmsg"]


def test_getmore_rejects_an_unknown_field(db) -> None:
    err = _err(db, {"getMore": _open(db), "collection": "c", "zz": 1})
    assert err.code == 40415
    assert err.details["errmsg"] == "BSON field 'getMore.zz' is an unknown field."


def test_getmore_parse_errors_beat_the_cursor_lookup(db) -> None:
    """All four shapes above used to answer ``CursorNotFound`` (43) -- for a
    cursor that existed. mongod parses first."""
    cid = _open(db)
    for cmd in (
        {"getMore": cid},
        {"getMore": cid, "collection": 5},
        {"getMore": cid, "collection": "c", "zz": 1},
    ):
        assert _err(db, cmd).code != 43


def test_maxtimems_rejected_on_a_non_awaitdata_cursor(db) -> None:
    """It is the awaitData wait budget, so mongod refuses it on a cursor that
    cannot wait. We accepted and ignored it, which hides a client bug: a caller
    who thinks it bounded a blocking read has bounded nothing."""
    err = _err(db, {"getMore": _open(db), "collection": "c", "maxTimeMS": 10})
    assert err.code == 2
    assert err.details["errmsg"] == (
        "cannot set maxTimeMS on getMore command for a non-awaitData cursor"
    )


def test_maxtimems_rejected_on_a_tailable_non_awaitdata_cursor(db) -> None:
    """Tailable is not enough -- probed both ways on 6.0.16."""
    db.create_collection("cap", capped=True, size=100_000)
    db.cap.insert_many([{"_id": i} for i in range(5)])
    cid = db.command({"find": "cap", "batchSize": 2, "tailable": True})["cursor"]["id"]
    err = _err(db, {"getMore": cid, "collection": "cap", "maxTimeMS": 10})
    assert err.code == 2


def test_maxtimems_accepted_on_an_awaitdata_cursor(db) -> None:
    db.create_collection("cap", capped=True, size=100_000)
    db.cap.insert_many([{"_id": i} for i in range(5)])
    cid = db.command({"find": "cap", "batchSize": 2, "tailable": True, "awaitData": True})[
        "cursor"
    ]["id"]
    reply = db.command({"getMore": cid, "collection": "cap", "maxTimeMS": 10})
    assert reply["ok"] == 1.0


def test_a_normal_getmore_still_works(db) -> None:
    cid = _open(db)
    reply = db.command({"getMore": cid, "collection": "c", "batchSize": 3})
    assert [d["_id"] for d in reply["cursor"]["nextBatch"]] == [3, 4, 5]


# --- killCursors ------------------------------------------------------------


def test_killcursors_requires_the_cursors_field(db) -> None:
    """We answered a cheerful all-empty success reply."""
    err = _err(db, {"killCursors": "c"})
    assert err.code == 40414
    assert err.details["errmsg"] == (
        "BSON field 'killCursors.cursors' is missing but a required field"
    )


def test_killcursors_cursors_must_be_an_array(db) -> None:
    err = _err(db, {"killCursors": "c", "cursors": {}})
    assert err.code == 14
    assert err.details["errmsg"] == (
        "BSON field 'killCursors.cursors' is the wrong type 'object', expected type 'array'"
    )


@pytest.mark.parametrize("bad,type_name", [(5, "int"), (5.0, "double"), ("x", "string")])
def test_killcursors_elements_must_be_longs(db, bad, type_name) -> None:
    err = _err(db, {"killCursors": "c", "cursors": [bad]})
    assert err.code == 14
    assert err.details["errmsg"] == (
        f"BSON field 'killCursors.cursors.0' is the wrong type '{type_name}', expected type 'long'"
    )


def test_killcursors_null_element_is_skipped(db) -> None:
    """mongod answers an all-empty success reply rather than rejecting it."""
    reply = db.command({"killCursors": "c", "cursors": [None]})
    assert reply["cursorsKilled"] == []
    assert reply["cursorsNotFound"] == []


def test_killcursors_null_cursors_reads_as_a_missing_field(db) -> None:
    """6.0 sent an explicit null down an older path (10065); 8.x treats it as
    the field not being sent at all."""
    err = _err(db, {"killCursors": "c", "cursors": None})
    assert err.code == 40414
    assert "is missing but a required field" in str(err)


def test_killcursors_rejects_an_unknown_field(db) -> None:
    err = _err(db, {"killCursors": "c", "cursors": [], "zz": 1})
    assert err.code == 40415


def test_killcursors_still_kills(db) -> None:
    cid = _open(db)
    reply = db.command({"killCursors": "c", "cursors": [cid]})
    assert reply["cursorsKilled"] == [cid]
    assert reply["cursorsNotFound"] == []
    assert _err(db, {"getMore": cid, "collection": "c"}).code == 43


# --- aggregate's cursor spec ------------------------------------------------


def test_aggregate_requires_a_cursor_option(db) -> None:
    """We ran the pipeline and answered a cursor anyway, so a client that
    forgot the option never learned it had."""
    err = _err(db, {"aggregate": "c", "pipeline": []})
    assert err.code == 9
    assert err.details["errmsg"] == (
        "The 'cursor' option is required, except for aggregate with the explain argument"
    )


def test_aggregate_without_a_cursor_is_fine_when_explaining(db) -> None:
    reply = db.command({"aggregate": "c", "pipeline": [], "explain": True})
    assert reply["ok"] == 1.0


def test_aggregate_cursor_rejects_an_unknown_key(db) -> None:
    err = _err(db, {"aggregate": "c", "pipeline": [], "cursor": {"zz": 1}})
    assert err.code == 40415
    assert err.details["errmsg"] == "BSON field 'cursor.zz' is an unknown field."


def test_aggregate_cursor_still_works(db) -> None:
    reply = db.command({"aggregate": "c", "pipeline": [], "cursor": {"batchSize": 3}})
    assert len(reply["cursor"]["firstBatch"]) == 3


# --- awaitData without tailable ---------------------------------------------


def test_awaitdata_without_tailable_is_rejected(db) -> None:
    """We accepted it and ran an ordinary find, so a client that asked to block
    got a plain batch back with no indication its option had been dropped."""
    err = _err(db, {"find": "c", "awaitData": True})
    assert err.code == 9
    assert err.details["errmsg"] == "Cannot set 'awaitData' without also setting 'tailable'"
