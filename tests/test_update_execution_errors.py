"""Update errors that depend on the stored document match mongod's.

mongod distinguishes two kinds of update error and wraps only one of them:

* **execution-time** — discoverable only while applying the update to a
  particular document (``$inc`` on a non-numeric *field*, an array operator on a
  non-array *field*). mongod 8.3.4 wraps these in ``Plan executor error during
  update :: caused by :: ``; 6.0.16 does not, and the codes and bodies are the
  same either way. We advertise 7.0 and emit the bare body.
* **parse-time** — determinable from the update spec alone (a path conflict, a
  self-rename, an unknown operator, ``$inc`` with a non-numeric *argument*).
  These stay plain.

We emitted the bodies without the wrapper, used our own codes and wording for
``$push`` / ``$addToSet``, and -- worst -- treated ``$pop`` on a non-array as a
silent no-op, reporting success for an update mongod refuses.

Every expectation was probed against mongod 8.3.4.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer

# mongod 8.3.4 wraps these in "Plan executor error during update :: caused by ::";
# 6.0.16 does not, and the bodies/codes are identical either way. We advertise 7.0
# and the differential gate runs PATH mongod (6.0.16), so we emit the bare body.
PREFIX = ""


@pytest.fixture
def db(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli["exec_err"]
    finally:
        cli.close()
        srv.stop()


def _apply(db, seed, update):
    db.c.drop()
    doc = dict(seed)
    doc["_id"] = 1
    db.c.insert_one(doc)
    reply = db.command({"update": "c", "updates": [{"q": {"_id": 1}, "u": update}]})
    errs = reply.get("writeErrors") or []
    return (errs[0] if errs else None), db.c.find_one({"_id": 1})


@pytest.mark.parametrize(
    "seed,update,code,body",
    [
        (
            {"a": "x"},
            {"$inc": {"a": 1}},
            14,
            "Cannot apply $inc to a value of non-numeric type. {_id: 1} has the field 'a' "
            "of non-numeric type string",
        ),
        (
            {"a": "x"},
            {"$mul": {"a": 2}},
            14,
            "Cannot apply $mul to a value of non-numeric type. {_id: 1} has the field 'a' "
            "of non-numeric type string",
        ),
        (
            {"a": 5},
            {"$push": {"a": 1}},
            2,
            "The field 'a' must be an array but is of type int in document {_id: 1}",
        ),
        (
            {"a": 5},
            {"$addToSet": {"a": 1}},
            2,
            "Cannot apply $addToSet to non-array field. Field named 'a' has non-array type int",
        ),
        ({"a": 5}, {"$pull": {"a": 1}}, 2, "Cannot apply $pull to a non-array value"),
        (
            {"a": 5},
            {"$pop": {"a": 1}},
            14,
            "Path 'a' contains an element of non-array type 'int'",
        ),
    ],
)
def test_execution_errors_are_wrapped_like_mongod(db, seed, update, code, body) -> None:
    err, _ = _apply(db, seed, update)
    assert err is not None, f"{update} should have errored"
    assert err["code"] == code
    assert err["errmsg"] == PREFIX + body


def test_pop_on_a_non_array_no_longer_reports_success(db) -> None:
    """The regression: this used to return n=1 with no writeError and leave the
    document untouched, i.e. an invalid update reported as applied."""
    err, doc = _apply(db, {"a": 5}, {"$pop": {"a": 1}})
    assert err is not None and err["code"] == 14
    assert doc == {"_id": 1, "a": 5}


@pytest.mark.parametrize(
    "seed,update",
    [
        ({"b": 1}, {"$pop": {"a": 1}}),  # missing field
        ({"a": []}, {"$pop": {"a": 1}}),  # empty array
        ({"b": 1}, {"$pull": {"a": 1}}),
        ({"a": []}, {"$pull": {"a": 1}}),
    ],
)
def test_missing_or_empty_stay_no_ops(db, seed, update) -> None:
    """Only a PRESENT non-array errors -- a missing field or an empty array are
    no-ops on mongod, and the fix must not turn them into errors."""
    err, doc = _apply(db, seed, update)
    assert err is None, err
    assert doc == {"_id": 1, **seed}


@pytest.mark.parametrize(
    "seed,update",
    [
        ({"a": 1}, {"$set": {"a": 2}, "$inc": {"a": 1}}),  # path conflict
        ({"a": 1}, {"$rename": {"a": "a"}}),  # self-rename
        ({"a": 1}, {"$inc": {"a": "x"}}),  # non-numeric ARGUMENT
    ],
)
def test_parse_time_errors_still_report(db, seed, update) -> None:
    """Parse-time errors are a separate class and are unaffected by this slice."""
    err, _ = _apply(db, seed, update)
    assert err is not None
    assert "Plan executor" not in err["errmsg"], err["errmsg"]


def test_valid_array_operations_still_work(db) -> None:
    err, doc = _apply(db, {"a": [1]}, {"$push": {"a": 2}})
    assert err is None
    assert doc["a"] == [1, 2]
