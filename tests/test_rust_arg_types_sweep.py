"""The 2026-08-31 wide argument sweep, pinned against the RUST server.

`tests/test_command_arg_types.py`, `test_arg_types_numeric.py`,
`test_arg_types_accepted_slots.py` and `test_arg_types_wrong_codes.py` pin the
Python server's argument validation. This file is the same class of check for
the Rust server, over the slots the earlier sweeps never reached.

**Why it exists.** The earlier sweeps compared CODES only, over 244 shapes, and
both servers read clean. Widening the comparison to MESSAGES and the corpus to
685 shapes -- `tools/probes/arg_types_messages.py`, against mongod 8.2.11 --
found the Rust server diverging on **76 argument slots**, nearly all of them
*silently accepted*: `find({...}, hint="nosuch")` on an update ran the write
unhinted, `killCursors` with a non-array `cursors` reported the cursors killed,
`update` with an empty `updates` array answered ok:1. Each of those tells a
driver that something happened which did not.

The per-slot asymmetries are the point of the file. mongod's strictness is not
uniform, and a blanket rule reproduces none of it:

* `count.limit` rejects an explicit null with its own BadValue wording while
  `count.skip`, the slot beside it, accepts one and uses the BSON-field family;
* `getMore.collection` reads null as ABSENT (40414), not wrong-typed (14);
* `createIndexes`' `unique` accepts `1.5` (a number is convertible to bool) and
  rejects `"x"`, quoting the whole spec back with mongod's own unclosed quote;
* `$densify` capitalises "The" where `$setWindowFields` does not.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_argtypes") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["argsweep"]
    d.c.drop()
    d.c.insert_one({"_id": 1, "a": 1})
    try:
        yield d
    finally:
        cli.close()


def _err(db, cmd):
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command(dict(cmd))
    assert exc.value.code != 1, f"internal error, not a parse error: {exc.value}"
    return exc.value


# --- the BSON-field family, over slots that used to be accepted -------------


@pytest.mark.parametrize(
    ("cmd", "path"),
    [
        ({"count": "c", "query": 5}, "count.query"),
        ({"distinct": "c", "key": "a", "query": 5}, "distinctCommandRequest.query"),
        ({"distinct": "c", "key": "a", "collation": 5}, "distinctCommandRequest.collation"),
        ({"find": "c", "readConcern": 5}, "FindCommandRequest.readConcern"),
        ({"aggregate": "c", "pipeline": [], "cursor": {}, "collation": 5}, "aggregate.collation"),
        ({"listCollections": 1, "filter": 5}, "listCollections.filter"),
        ({"collMod": "c", "validator": 5}, "collMod.validator"),
        ({"insert": "c", "documents": [{}], "writeConcern": 5}, "insert.writeConcern"),
        (
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "fields": 5},
            "findAndModify.fields",
        ),
    ],
)
def test_wrong_typed_object_slot_names_its_idl_path(db, cmd, path) -> None:
    err = _err(db, cmd)
    assert err.code == 14
    assert err.details["errmsg"] == (
        f"BSON field '{path}' is the wrong type 'int', expected type 'object'"
    )


def test_an_object_slot_accepts_an_explicit_null(db) -> None:
    """The BSON-field family's null-means-absent rule, which `hint` does NOT share."""
    db.command({"count": "c", "query": None})
    db.command({"insert": "c", "documents": [{"z": 1}], "writeConcern": None})


# --- hint: one message, six commands, and null is NOT accepted -------------


@pytest.mark.parametrize(
    "cmd",
    [
        {"find": "c", "hint": 5},
        {"count": "c", "hint": 5},
        {"aggregate": "c", "pipeline": [], "cursor": {}, "hint": 5},
        {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "hint": 5}]},
        {"delete": "c", "deletes": [{"q": {}, "limit": 0, "hint": 5}]},
        {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "hint": 5},
    ],
)
def test_a_wrong_typed_hint_is_failed_to_parse(db, cmd) -> None:
    err = _err(db, cmd)
    assert err.code == 9
    assert err.details["errmsg"] == "Hint must be a string or an object"


@pytest.mark.parametrize(
    "cmd",
    [
        {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "hint": "nosuch"}]},
        {"delete": "c", "deletes": [{"q": {}, "limit": 0, "hint": "nosuch"}]},
        {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "hint": "nosuch"},
    ],
)
def test_a_hint_naming_no_index_fails_the_write(db, cmd) -> None:
    """These three ignored `hint` entirely: the write ran unhinted, reporting ok."""
    assert _err(db, cmd).code == 2


# --- the per-slot asymmetries ---------------------------------------------


def test_count_limit_and_skip_disagree_about_null(db) -> None:
    err = _err(db, {"count": "c", "limit": None})
    assert err.code == 2
    assert err.details["errmsg"] == "limit value is not a valid number"
    # The slot beside it takes the BSON-field family, which accepts a null.
    db.command({"count": "c", "skip": None})


def test_a_required_slot_reads_null_as_missing(db) -> None:
    err = _err(db, {"getMore": bson_int64(1), "collection": None})
    assert err.code == 40414
    assert err.details["errmsg"] == (
        "BSON field 'getMore.collection' is missing but a required field"
    )
    err = _err(db, {"getMore": bson_int64(1), "collection": 5})
    assert err.code == 14


def test_kill_cursors_rejects_a_non_array(db) -> None:
    """Used to report ok:1 -- the caller was told cursors it named were killed."""
    err = _err(db, {"killCursors": "c", "cursors": 5})
    assert err.code == 14
    assert err.details["errmsg"] == (
        "BSON field 'killCursors.cursors' is the wrong type 'int', expected type 'array'"
    )
    assert _err(db, {"killCursors": "c", "cursors": None}).code == 40414


def test_index_spec_bool_accepts_a_number_and_quotes_the_spec_back(db) -> None:
    err = _err(
        db,
        {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "i", "unique": "x"}]},
    )
    assert err.code == 14
    # mongod opens a quote before the field name and never closes it.
    assert 'The field \'unique has value unique: "x"' in err.details["errmsg"]
    # A number IS convertible to bool, so this one is accepted.
    db.command({"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "i2", "unique": 1.5}]})


def test_ttl_option_answers_cannot_create_index(db) -> None:
    err = _err(
        db,
        {
            "createIndexes": "c",
            "indexes": [{"key": {"a": 1}, "name": "i", "expireAfterSeconds": "x"}],
        },
    )
    assert err.code == 67
    assert err.details["errmsg"].startswith(". Index spec: ")


# --- write batches and stage specs ----------------------------------------


@pytest.mark.parametrize(
    "cmd",
    [
        {"insert": "c", "documents": []},
        {"update": "c", "updates": []},
        {"delete": "c", "deletes": []},
    ],
)
def test_an_empty_write_batch_is_invalid_length_16(db, cmd) -> None:
    """`update` / `delete` used to answer ok:1 with n:0 for a batch never sent."""
    err = _err(db, cmd)
    assert err.code == 16
    assert err.details["codeName"] == "InvalidLength"


def test_a_non_array_pipeline_is_rejected_before_it_is_read(db) -> None:
    err = _err(db, {"aggregate": "c", "pipeline": 5, "cursor": {}})
    assert err.code == 14
    assert err.details["errmsg"] == "A pipeline must be an array of objects"


@pytest.mark.parametrize(
    ("stage", "code"),
    [
        ("$addFields", 40272),
        ("$project", 15969),
        ("$replaceRoot", 40229),
        ("$facet", 40169),
        ("$bucket", 40201),
        ("$geoNear", 10065),
        ("$graphLookup", 9),
        ("$setWindowFields", 9),
        ("$densify", 9),
        ("$fill", 9),
        ("$sample", 28745),
        ("$sortByCount", 40149),
    ],
)
def test_each_stage_spec_carries_its_own_code(db, stage, code) -> None:
    err = _err(db, {"aggregate": "c", "pipeline": [{stage: 5}], "cursor": {}})
    assert err.code == code


def test_an_unmatched_array_filter_identifier_is_named(db) -> None:
    """Fires even though `a` is not an array -- mongod decides this from the
    update document alone, which is exactly what the engine's walk could not do."""
    for cmd in (
        {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a.$[e]": 1}}}]},
        {"findAndModify": "c", "query": {}, "update": {"$set": {"a.$[e]": 1}}},
    ):
        err = _err(db, cmd)
        assert err.code == 2
        assert err.details["errmsg"] == (
            "No array filter found for identifier 'e' in path 'a.$[e]'"
        )


def test_drop_indexes_by_key_spec_is_not_an_internal_error(db) -> None:
    """Answered code 1 (InternalError) -- the crash code -- for a shape mongod
    handles routinely."""
    db.command({"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]})
    db.command({"dropIndexes": "c", "index": {"a": 1}})
    err = _err(db, {"dropIndexes": "c", "index": {"nosuch": 1}})
    assert err.code == 27
    assert err.details["errmsg"] == "can't find index with key: { nosuch: 1 }"


def bson_int64(n):
    from bson import Int64

    return Int64(n)
