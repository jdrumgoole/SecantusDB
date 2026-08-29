"""Argument slots we used to accept silently where mongod errors.

The third and last tranche of the wrong-typed-argument sweep. The first fixed
document-valued arguments, the second the 24 slots that crashed as "internal
server error"; this one covers the slots where a wrong-typed value was taken
without complaint -- the worst failure mode of the three, because a driver's bug
sails straight through and the caller is told the operation succeeded.

**Six message families for nine slots, and no two rules the same.** Every
expectation here was probed individually against mongod 6.0.16; none was
inferred from a neighbouring slot. The three that would have been got wrong by
reasoning:

* ``findAndModify.upsert`` accepts a bool OR any number -- ``upsert: 1`` and
  even ``upsert: 1.5`` are fine -- while the adjacent ``update.updates.multi``
  is a strict bool and rejects ``multi: 1``.
* ``find.let`` is reported as ``FindCommandRequest.let``, mongod's internal IDL
  name, while ``update.let`` / ``delete.let`` / ``findAndModify.let`` /
  ``aggregate.let`` use their command name.
* ``find.maxTimeMS`` is the only slot in the sweep that is not a TypeMismatch:
  code 2, with three distinct messages depending on how the value is wrong.

An explicit ``null`` is its own axis: six slots accept it, three reject it.
"""

from __future__ import annotations

import pymongo
import pytest
from bson import Decimal128

from secantus import SecantusDBServer

NOT_OBJECTS = [5, "x", True, [1]]


def _bson_type_name(value: object) -> str:
    """mongod's type vocabulary for the values this file feeds in."""
    if isinstance(value, bool):
        return "bool"
    return {dict: "object", str: "string", list: "array", int: "int"}[type(value)]


@pytest.fixture
def db(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    d = cli["argslots"]
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


# --------------------------------------------------------------------------
# The ``BSON field '<path>' is the wrong type '<t>', expected type 'object'``
# family. Each slot reports its own path, and an explicit null is ACCEPTED.
# --------------------------------------------------------------------------

OBJECT_SLOTS = [
    ("create.storageEngine", lambda v: {"create": "fresh", "storageEngine": v}),
    ("collMod.index", lambda v: {"collMod": "c", "index": v}),
    ("aggregate.let", lambda v: {"aggregate": "c", "pipeline": [], "cursor": {}, "let": v}),
    ("FindCommandRequest.let", lambda v: {"find": "c", "let": v}),
    (
        "update.let",
        lambda v: {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}}], "let": v},
    ),
    ("delete.let", lambda v: {"delete": "c", "deletes": [{"q": {}, "limit": 1}], "let": v}),
    (
        "findAndModify.let",
        lambda v: {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "let": v},
    ),
]


@pytest.mark.parametrize("bad", NOT_OBJECTS)
@pytest.mark.parametrize("path,build", OBJECT_SLOTS, ids=[p for p, _ in OBJECT_SLOTS])
def test_object_slots_are_type_errors(db, path, build, bad) -> None:
    err = _err(db, build(bad))
    assert err.code == 14
    assert err.details["errmsg"] == (
        f"BSON field '{path}' is the wrong type '{_bson_type_name(bad)}', expected type 'object'"
    )


@pytest.mark.parametrize("path,build", OBJECT_SLOTS, ids=[p for p, _ in OBJECT_SLOTS])
def test_object_slots_accept_an_explicit_null(db, path, build) -> None:
    """mongod accepts ``let: null`` on every one of these. Only the
    ``Expected field`` family and the boolean slots reject it."""
    db.command(build(None))


# --------------------------------------------------------------------------
# ``find.collation``: the ``Expected field <name>to be of type object`` family
# (mongod's own missing space), where an explicit null IS rejected.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("bad", NOT_OBJECTS)
def test_find_collation_is_a_type_error(db, bad) -> None:
    err = _err(db, {"find": "c", "collation": bad})
    assert err.code == 14
    assert err.details["errmsg"] == "Expected field collationto be of type object"


@pytest.mark.parametrize("field", ["filter", "sort", "projection", "collation"])
def test_expected_field_family_rejects_an_explicit_null(db, field) -> None:
    """Absent is fine, ``null`` is not -- and telling those apart needs a
    membership test, not ``doc.get(...)``. We accepted the null form until this
    was probed."""
    err = _err(db, {"find": "c", field: None})
    assert err.code == 14
    assert err.details["errmsg"] == f"Expected field {field}to be of type object"


def test_find_without_the_optional_fields_is_fine(db) -> None:
    assert db.command({"find": "c"})["cursor"]["firstBatch"] == [{"_id": 1, "a": 1}]


# --------------------------------------------------------------------------
# The two boolean slots that disagree with each other, and the one that is a
# third family again.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("bad", [{}, [1], 1, "x", None])
def test_find_single_batch_wants_a_real_bool(db, bad) -> None:
    """A fourth message family, and it rejects null as ``found: null``."""
    err = _err(db, {"find": "c", "singleBatch": bad, "limit": 1})
    assert err.code == 14
    expected = "null" if bad is None else _bson_type_name(bad)
    assert err.details["errmsg"] == (
        f"Field 'singleBatch' should be a boolean value, but found: {expected}"
    )


@pytest.mark.parametrize("bad", [{}, [1], 1, "x"])
def test_update_multi_is_a_strict_bool(db, bad) -> None:
    err = _err(db, {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "multi": bad}]})
    assert err.code == 14
    assert err.details["errmsg"] == (
        f"BSON field 'update.updates.multi' is the wrong type "
        f"'{_bson_type_name(bad)}', expected type 'bool'"
    )


@pytest.mark.parametrize("field", ["upsert", "new", "remove"])
@pytest.mark.parametrize("bad", [{}, [1], "x"])
def test_find_and_modify_bool_flags_reject_non_numbers(db, field, bad) -> None:
    """The closing quote sits INSIDE the bracket -- ``double']``, not
    ``double]'``. Re-probed at byte level against mongod 6.0.16 on 2026-08-29
    after this assertion and the server disagreed; the server was matching the
    sensible form and mongod emits the odd one.

    ``new`` and ``remove`` take the same rule as ``upsert`` and are covered
    here too: ``new`` was not type-checked at all, so a string went through
    Python truthiness and ``new: "no"`` returned the POST-image.
    """
    err = _err(
        db,
        {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, field: bad},
    )
    assert err.code == 14
    assert err.details["errmsg"] == (
        f"BSON field 'findAndModify.{field}' is the wrong type "
        f"'{_bson_type_name(bad)}', expected types '[bool, long, int, decimal, double']"
    )


@pytest.mark.parametrize("ok", [1, 0, 1.5, True, False, None])
def test_find_and_modify_upsert_accepts_numbers(db, ok) -> None:
    """The asymmetry that makes this class per-slot: ``upsert: 1.5`` is
    accepted here while ``multi: 1`` two screens away is a type error."""
    db.command(
        {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"a": 2}}, "upsert": ok}
    )


def test_update_multi_accepts_an_explicit_null(db) -> None:
    db.command({"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "multi": None}]})


# --------------------------------------------------------------------------
# ``find.maxTimeMS``: code 2, three messages.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("bad", [{}, "x", [1], True, None])
def test_max_time_ms_must_be_a_number(db, bad) -> None:
    err = _err(db, {"find": "c", "maxTimeMS": bad})
    assert err.code == 2
    assert err.details["errmsg"] == "maxTimeMS must be a number"


def test_max_time_ms_must_be_integral(db) -> None:
    err = _err(db, {"find": "c", "maxTimeMS": 1.5})
    assert err.code == 2
    assert err.details["errmsg"] == "maxTimeMS has non-integral value"


def test_max_time_ms_must_not_be_negative(db) -> None:
    err = _err(db, {"find": "c", "maxTimeMS": -1})
    assert err.code == 2
    assert err.details["errmsg"] == "-1 value for maxTimeMS is out of range"


@pytest.mark.parametrize("ok", [0, 5, 5.0, Decimal128("5")])
def test_max_time_ms_accepts_integral_numbers(db, ok) -> None:
    db.command({"find": "c", "maxTimeMS": ok})


# --------------------------------------------------------------------------
# aggregate's cursor slot: same null asymmetry as the Expected-field family.
# --------------------------------------------------------------------------


def test_aggregate_cursor_rejects_an_explicit_null(db) -> None:
    """The message says "missing or an object" and means it."""
    err = _err(db, {"aggregate": "c", "pipeline": [], "cursor": None})
    assert err.code == 14
    assert err.details["errmsg"] == "cursor field must be missing or an object"


# --------------------------------------------------------------------------
# The counterexample this whole class exists for.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("value", [{}, "x", [1], 0])
def test_delete_limit_is_still_not_type_checked(db, value) -> None:
    """mongod ACCEPTS every one of these in ``delete.deletes.limit``, all
    meaning "no limit", while the analogous ``find.limit`` is a type error.
    A blanket rule over this sweep would break it -- so it is pinned here,
    next to the slots that ARE checked."""
    db.c.insert_one({"_id": 2, "a": 1})
    reply = db.command({"delete": "c", "deletes": [{"q": {}, "limit": value}]})
    assert reply["n"] == 2
