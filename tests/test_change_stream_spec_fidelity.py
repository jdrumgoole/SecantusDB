"""`$changeStream` spec validation and event shape match mongod's.

Phase 2 of ``tasks/remaining-work-plan.md``, fifth and last surface: 13 change
-stream shapes probed against a real replica-set mongod 6.0.16 (change streams
need one, so the probe spawns ``--replSet`` and initiates it). **All 13
diverged.**

The dominant failure was arguments accepted and IGNORED, which is a worse shape
here than almost anywhere else in the server: the caller *believes* they asked
for something. ``parse_spec`` guarded every field with ``isinstance`` and
silently skipped a wrong-typed value, so a client that asked for
``fullDocument: "updateLookup"`` -- or to resume from a token -- got a stream
that quietly did neither and reported success.

Probed against mongod 6.0.16.
"""

from __future__ import annotations

import pymongo
import pytest
from bson import Timestamp

from secantus import SecantusDBServer


@pytest.fixture
def db(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        d = cli["cs"]
        d.c.insert_one({"_id": 0, "seed": 1})
        yield d
    finally:
        cli.close()
        srv.stop()


def _watch(db, spec):
    return db.command({"aggregate": "c", "pipeline": [{"$changeStream": spec}], "cursor": {}})


def _err(db, spec):
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _watch(db, spec)
    return exc.value


# --- the crash ---------------------------------------------------------------


def test_a_valid_hex_but_invalid_bson_token_does_not_crash(db) -> None:
    """``{"_data": "aa"}`` is two hex digits -- valid hex, one byte, not a BSON
    document. ``bson.decode`` raised ``InvalidBSON`` straight out of the handler
    and escaped as "internal server error" (code 1)."""
    err = _err(db, {"resumeAfter": {"_data": "aa"}})
    assert err.code != 1
    assert "internal server error" not in str(err)


def test_a_non_hex_token_reports_mongods_wording(db) -> None:
    """We wrapped Python's ``fromhex()`` complaint in our own prefix."""
    err = _err(db, {"resumeAfter": {"_data": "zzzz"}})
    assert err.code == 9
    assert err.details["errmsg"] == "resume token string was not a valid hex string"


# --- accepted and ignored ----------------------------------------------------


@pytest.mark.parametrize("value", ["nope", "UpdateLookup", ""])
def test_unknown_full_document_mode_is_rejected(db, value) -> None:
    """The one that matters most: a client asking for ``updateLookup`` and
    misspelling it got a stream that silently used the default."""
    err = _err(db, {"fullDocument": value})
    assert err.code == 2
    assert err.details["errmsg"] == (
        f"Enumeration value '{value}' for field '$changeStream.fullDocument' is not a valid value."
    )


def test_unknown_full_document_before_change_mode_is_rejected(db) -> None:
    err = _err(db, {"fullDocumentBeforeChange": "nope"})
    assert err.code == 2
    assert "fullDocumentBeforeChange" in err.details["errmsg"]


@pytest.mark.parametrize("mode", ["default", "updateLookup", "whenAvailable", "required"])
def test_valid_full_document_modes_are_still_accepted(db, mode) -> None:
    assert _watch(db, {"fullDocument": mode})["ok"] == 1.0


@pytest.mark.parametrize("mode", ["off", "whenAvailable", "required"])
def test_valid_before_change_modes_are_still_accepted(db, mode) -> None:
    assert _watch(db, {"fullDocumentBeforeChange": mode})["ok"] == 1.0


def test_unknown_spec_field_is_rejected(db) -> None:
    err = _err(db, {"zz": 1})
    assert err.code == 40415
    assert err.details["errmsg"] == "BSON field '$changeStream.zz' is an unknown field."


@pytest.mark.parametrize("field", ["resumeAfter", "startAfter"])
def test_resume_token_must_be_a_document(db, field) -> None:
    """Accepted and ignored: the stream started from the beginning instead of
    the requested position, and said it had succeeded."""
    err = _err(db, {field: 5})
    assert err.code == 14
    assert err.details["errmsg"] == (
        f"BSON field '$changeStream.{field}' is the wrong type 'int', expected type 'object'"
    )


def test_start_at_operation_time_must_be_a_timestamp(db) -> None:
    err = _err(db, {"startAtOperationTime": "x"})
    assert err.code == 14
    assert "expected type 'timestamp'" in err.details["errmsg"]


def test_a_real_start_at_operation_time_is_accepted(db) -> None:
    assert _watch(db, {"startAtOperationTime": Timestamp(1, 1)})["ok"] == 1.0


def test_spec_must_be_a_document(db) -> None:
    err = _err(db, 5)
    assert err.code == 6188500
    assert err.details["errmsg"] == (
        "$changeStream must take a nested object but found: $changeStream: 5"
    )


def test_changestream_must_be_the_first_stage(db) -> None:
    """Anywhere else we built an ordinary aggregation and answered an exhausted
    cursor -- a "stream" that never yields an event and never says why."""
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command(
            {
                "aggregate": "c",
                "pipeline": [{"$match": {}}, {"$changeStream": {}}],
                "cursor": {},
            }
        )
    assert exc.value.code == 40602
    assert "only valid as the first stage" in str(exc.value)


# --- event shape -------------------------------------------------------------


def _events(db, **kw):
    with db.c.watch(max_await_time_ms=800, **kw) as stream:
        db.c.insert_one({"_id": 1, "v": 1})
        db.c.update_one({"_id": 1}, {"$set": {"v": 2}})
        out = []
        for _ in range(2):
            ev = stream.try_next()
            if ev is None:
                break
            out.append(ev)
    return out


# These two used to assert that ``fullDocument`` sits immediately after
# ``operationType``. That was never measured -- this file drives our own
# server, and mongod refuses change streams on a standalone, so nothing here
# could check it. Measured against a real single-node replica set (mongod
# 6.0.16, 2026-08-29, tools/probes/change_streams.py), the event is:
#
#   ['_id', 'operationType', 'clusterTime', 'wallTime', 'fullDocument',
#    'ns', 'documentKey', 'updateDescription']
#
# so ``fullDocument`` follows ``wallTime`` at index 4, and hoisting it to
# index 2 also pushed ``_id`` out of first position. They now pin the measured
# order, which is why they are written as an exact key sequence rather than as
# a relative index: a relative assertion is what let the wrong claim survive.


def test_insert_event_field_order_matches_mongod(db) -> None:
    """The event's field order is part of what a driver reads off the wire."""
    events = _events(db)
    assert events, "expected at least the insert event"
    assert list(events[0]) == [
        "_id",
        "operationType",
        "clusterTime",
        "wallTime",
        "fullDocument",
        "ns",
        "documentKey",
    ]


def test_update_lookup_event_field_order_matches_mongod(db) -> None:
    events = _events(db, full_document="updateLookup")
    update = next(e for e in events if e["operationType"] == "update")
    assert list(update) == [
        "_id",
        "operationType",
        "clusterTime",
        "wallTime",
        "fullDocument",
        "ns",
        "documentKey",
        "updateDescription",
    ]


def test_the_events_themselves_are_unchanged(db) -> None:
    """The reordering must not disturb what the events say."""
    events = _events(db)
    insert = events[0]
    assert insert["operationType"] == "insert"
    assert insert["fullDocument"] == {"_id": 1, "v": 1}
    assert insert["documentKey"] == {"_id": 1}
    assert insert["ns"]["coll"] == "c"
