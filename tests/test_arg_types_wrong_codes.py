"""Wrong-typed stage specs and cursor bounds answer mongod's code, not ours.

The last tranche of the wrong-typed-argument sweep, and the one where we already
returned an error -- just not the right one. #1078 fixed the slots that took a
wrong type silently, #1080 the ones that crashed, and the tranche before this
one the slots that were accepted; these answered a plausible-looking code that
no real client would see from MongoDB.

Codes matter more than they look. A driver branches on them: `$lookup`'s
FailedToParse (9) is a *parse* failure a client can report against the query
text, while TypeMismatch (14) says an argument had the wrong BSON type. We gave
14 for both.

The two shapes that were conflated are worth naming, because both were a single
condition doing two jobs:

* ``$sort`` answered 15976 ("must have at least one sort key") for a spec that
  was not an object at all, which is 15973. One `if` covered `not a Mapping or
  empty`; mongod distinguishes them.
* ``$unwind`` answered 28808 ("expected a string as the path") for a spec with
  no ``path`` at all, which is 28812, and 28818 (the missing-`$` message) for an
  empty path, which is also 28812.

Every code and message here was probed against mongod 6.0.16.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer

NOT_OBJECTS = [5, "x", True, [1]]


def _bson_type_name(value: object) -> str:
    if isinstance(value, bool):
        return "bool"
    if value is None:
        return "null"
    return {dict: "object", str: "string", list: "array", int: "int"}[type(value)]


@pytest.fixture
def db(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    d = cli["argcodes"]
    d.c.insert_one({"_id": 1, "a": [1, 2]})
    d.c.create_index([("a", 1)])
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


def _agg(stage):
    return {"aggregate": "c", "pipeline": [stage], "cursor": {}}


# --------------------------------------------------------------------------
# find's index bounds: the same "Expected field <name>to be of type object"
# family as filter / sort / projection / collation, not a bound-check error.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("bad", [*NOT_OBJECTS, None])
@pytest.mark.parametrize("field", ["min", "max"])
def test_find_min_max_are_parse_errors(db, field, bad) -> None:
    """We reached the index bound-checker and answered 51174. mongod
    type-checks at parse time, BEFORE any hint validation, and answers 14."""
    err = _err(db, {"find": "c", field: bad, "hint": {"a": 1}})
    assert err.code == 14
    assert err.details["errmsg"] == f"Expected field {field}to be of type object"


def test_find_min_max_still_work_when_well_formed(db) -> None:
    reply = db.command({"find": "c", "min": {"a": 1}, "hint": {"a": 1}})
    assert reply["ok"] == 1.0


# --------------------------------------------------------------------------
# $lookup / $group / $sort: one code each for a non-object spec, and a
# DIFFERENT one for an object that is merely incomplete.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("bad", NOT_OBJECTS)
def test_lookup_spec_is_failed_to_parse(db, bad) -> None:
    err = _err(db, _agg({"$lookup": bad}))
    assert err.code == 9
    assert err.details["errmsg"] == (
        f"the $lookup stage specification must be an object, but found {_bson_type_name(bad)}"
    )


@pytest.mark.parametrize("bad", NOT_OBJECTS)
def test_group_spec_is_15947(db, bad) -> None:
    """A constant message -- mongod does not name the offending type here,
    unlike $lookup and $unwind."""
    err = _err(db, _agg({"$group": bad}))
    assert err.code == 15947
    assert err.details["errmsg"] == "a group's fields must be specified in an object"


def test_group_without_an_id_is_a_different_code(db) -> None:
    err = _err(db, _agg({"$group": {}}))
    assert err.code == 15955
    assert err.details["errmsg"] == "a group specification must include an _id"


@pytest.mark.parametrize("bad", NOT_OBJECTS)
def test_sort_spec_is_15973(db, bad) -> None:
    err = _err(db, _agg({"$sort": bad}))
    assert err.code == 15973
    assert err.details["errmsg"] == "the $sort key specification must be an object"


def test_empty_sort_spec_keeps_its_own_code(db) -> None:
    """The half of the old condition that WAS right. One `if` was doing two
    jobs; this pins the job it did correctly so the split cannot regress it."""
    err = _err(db, _agg({"$sort": {}}))
    assert err.code == 15976
    assert err.details["errmsg"] == "$sort stage must have at least one sort key"


# --------------------------------------------------------------------------
# $unwind: four codes over one option.
# --------------------------------------------------------------------------


@pytest.mark.parametrize("bad", [5, True, [1]])
def test_unwind_spec_must_be_a_string_or_object(db, bad) -> None:
    err = _err(db, _agg({"$unwind": bad}))
    assert err.code == 15981
    assert err.details["errmsg"] == (
        "expected either a string or an object as specification for $unwind stage, "
        f"got {_bson_type_name(bad)}"
    )


@pytest.mark.parametrize("spec", [{}, {"path": ""}, ""])
def test_unwind_with_no_path_is_28812(db, spec) -> None:
    """An absent path and an empty one are the same error. The empty string
    used to fall through to the missing-`$` check and answer 28818."""
    err = _err(db, _agg({"$unwind": spec}))
    assert err.code == 28812
    assert err.details["errmsg"] == "no path specified to $unwind stage"


@pytest.mark.parametrize("bad", [5, None, [1], {}])
def test_unwind_path_of_the_wrong_type_is_28808(db, bad) -> None:
    """Present but not a string -- including an explicit null, which is 28808
    and NOT the 28812 that a missing key gives."""
    err = _err(db, _agg({"$unwind": {"path": bad}}))
    assert err.code == 28808
    assert err.details["errmsg"] == (
        f"expected a string as the path for $unwind stage, got {_bson_type_name(bad)}"
    )


def test_unwind_path_must_be_prefixed(db) -> None:
    err = _err(db, _agg({"$unwind": {"path": "x"}}))
    assert err.code == 28818
    assert err.details["errmsg"] == "path option to $unwind stage should be prefixed with a '$': x"


def test_unwind_rejects_an_unrecognized_option(db) -> None:
    """Checked BEFORE the no-path rule: `{other: 1}` has no path either, and
    mongod still reports the unknown option."""
    err = _err(db, _agg({"$unwind": {"other": 1}}))
    assert err.code == 28811
    assert err.details["errmsg"] == "unrecognized option to $unwind stage: other"


def test_unwind_still_unwinds(db) -> None:
    out = list(db.c.aggregate([{"$unwind": "$a"}]))
    assert [d["a"] for d in out] == [1, 2]


def test_unwind_document_form_still_works(db) -> None:
    out = list(db.c.aggregate([{"$unwind": {"path": "$a", "includeArrayIndex": "i"}}]))
    assert [(d["a"], d["i"]) for d in out] == [(1, 0), (2, 1)]
