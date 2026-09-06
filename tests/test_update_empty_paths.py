"""An update path may not be empty, or have an empty component.

Both servers used to ACCEPT ``{$set: {"": 1}}`` and store ``{"": 1}`` -- a
document mongod cannot produce, and one the query that created it then fails to
match. That is the "user-supplied path used as a dict key" shape ``CLAUDE.md``
calls out, and it applied uniformly: ten operators, twenty shapes, every one of
them either silently writing an empty field name or answering the wrong code.

mongod (probed 8.2.11, 2026-09-06) answers ``56`` with two distinct messages:

* the whole path empty  -> ``An empty update path is not valid.``
* a component empty     -> ``The update path 'a.' contains an empty field name,
  which is not allowed.``

Both are PARSE errors, so they come back bare -- no
``Plan executor error during update`` wrapper.

The ordering against the path-conflict check (code 40) is load-bearing and is
pinned below: mongod interleaves the two checks in one walk over the update in
document order, and the FIRST offender wins. A separate earlier emptiness pass
answers 56 for both of the discriminating cases.

A REPLACEMENT-style update is deliberately exempt: ``replace_one({"_id": 1},
{"": 1})`` really does store an empty field name on mongod. Only operator paths
are validated.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer
from secantus.update import UpdateError, apply_update

# Every operator mongod applies this rule to, with a payload it accepts.
OPERATOR_VALUES = {
    "$set": 1,
    "$unset": "",
    "$inc": 1,
    "$mul": 1,
    "$min": 1,
    "$max": 1,
    "$push": 1,
    "$addToSet": 1,
    "$pop": 1,
    "$bit": {"and": 1},
}

EMPTY_WHOLE = "An empty update path is not valid."


def _empty_component(path: str) -> str:
    return f"The update path '{path}' contains an empty field name, which is not allowed."


@pytest.fixture
def client(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli
    finally:
        cli.close()
        srv.stop()


# --- the engine ------------------------------------------------------------


@pytest.mark.parametrize("op", sorted(OPERATOR_VALUES))
def test_empty_path_is_rejected_for_every_operator(op):
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 1}, {op: {"": OPERATOR_VALUES[op]}})
    assert exc.value.code == 56
    assert str(exc.value) == EMPTY_WHOLE


@pytest.mark.parametrize("op", sorted(OPERATOR_VALUES))
def test_empty_component_is_rejected_for_every_operator(op):
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 1}, {op: {"a.": OPERATOR_VALUES[op]}})
    assert exc.value.code == 56
    assert str(exc.value) == _empty_component("a.")


@pytest.mark.parametrize("path", ["a.", ".a", "a..b", "a.b.", "..", "."])
def test_every_position_of_an_empty_component_is_named(path):
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 1}, {"$set": {path: 1}})
    assert exc.value.code == 56
    assert str(exc.value) == _empty_component(path)


def test_rename_validates_both_of_its_ends():
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 1}, {"$rename": {"a": "b."}})
    assert exc.value.code == 56
    assert str(exc.value) == _empty_component("b.")

    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 1}, {"$rename": {"a.": "b"}})
    assert exc.value.code == 56
    assert str(exc.value) == _empty_component("a.")

    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 1}, {"$rename": {"": "b"}})
    assert exc.value.code == 56
    assert str(exc.value) == EMPTY_WHOLE


def test_setoninsert_is_validated_even_when_it_will_not_apply():
    """Parse-time, so it fires on a plain (non-upsert) update too."""
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 1}, {"$setOnInsert": {"": 1}}, is_upsert=False)
    assert exc.value.code == 56


def test_a_replacement_may_store_an_empty_field_name():
    """mongod allows it: `replace_one({"_id": 1}, {"": 1})` stores `{"": 1}`."""
    assert apply_update({"_id": 1, "a": 1}, {"": 1}) == {"_id": 1, "": 1}


def test_valid_paths_are_untouched():
    assert apply_update({"_id": 1}, {"$set": {"a.b": 1}}) == {"_id": 1, "a": {"b": 1}}
    assert apply_update({"_id": 1, "a": [{"x": 1}]}, {"$set": {"a.0.x": 2}})["a"] == [{"x": 2}]


# --- ordering against the conflict check -----------------------------------


# The discriminating pair: the same two faults, in the two orders. A separate
# emptiness pass ahead of the conflict walk answers 56 for both.
def test_an_empty_path_before_a_conflict_reports_the_empty_path():
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1}, {"$inc": {"": 1}, "$set": {"a": 1, "a.b": 1}})
    assert exc.value.code == 56


def test_a_conflict_before_an_empty_path_reports_the_conflict():
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1}, {"$set": {"a": 1, "a.b": 1}, "$inc": {"": 1}})
    assert exc.value.code == 40
    assert "would create a conflict" in str(exc.value)


def test_an_unknown_modifier_still_wins_over_an_empty_path():
    """mongod validates the operator name first (code 9)."""
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1}, {"$nope": {"": 1}})
    assert exc.value.code == 9


# --- over the wire ---------------------------------------------------------


@pytest.mark.parametrize(
    ("update", "message"),
    [
        ({"$set": {"": 1}}, EMPTY_WHOLE),
        ({"$unset": {"": ""}}, EMPTY_WHOLE),
        ({"$inc": {"": 1}}, EMPTY_WHOLE),
        ({"$push": {"": 1}}, EMPTY_WHOLE),
        ({"$set": {"a.": 1}}, _empty_component("a.")),
        ({"$set": {"a..b": 1}}, _empty_component("a..b")),
    ],
)
def test_over_the_wire_the_document_is_not_written(client, update, message):
    coll = client["emptypaths"]["c"]
    coll.drop()
    coll.insert_one({"_id": 1, "a": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        coll.update_one({"_id": 1}, update)
    assert exc.value.code == 56
    # A parse error: bare, with no executor wrapper.
    assert str(exc.value).startswith(message), str(exc.value)
    # The document is untouched -- in particular it has no empty field name.
    assert coll.find_one({"_id": 1}) == {"_id": 1, "a": 1}
