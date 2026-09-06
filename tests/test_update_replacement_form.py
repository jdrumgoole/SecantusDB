"""Which key decides whether an update is operators or a replacement.

mongod decides on the **first key alone** (probed 8.2.11, 2026-09-06) and then
complains in that form's vocabulary::

    {$set: {a: 1}, z: 2}   ->  9  Unknown modifier: z            (parse-time)
    {z: 2, $set: {a: 1}}   -> 52  The dollar ($) prefixed field  (execution-time)
                                  '$set' in '$set' is not allowed in the context
                                  of an update's replacement document. Consider
                                  using an aggregation pipeline with $replaceWith.

Both servers used to ask "does ANY key start with `$`", which made the second
one an operator update too and answered 9 for it.

The two errors differ in more than wording, and the difference is observable:

* the operator-form 9 is **parse-time** -- reported with no matching document
  and on an upsert;
* the replacement-form 52 is **execution-time** -- it fires only when a document
  is actually replaced. With no match the statement is a silent no-op (``n: 0``),
  and an **upsert inserts the document verbatim, ``$``-key and all**. It is
  therefore wrapped in ``Plan executor error during <command> :: caused by ::``.

Only the TOP level is restricted: mongod 8.x stores ``{a: {$bad: 1}}``,
``{a: [{$bad: 1}]}`` and even a literal dotted key ``{"a.b": 1}`` without
complaint, and ``insert`` accepts all of those too.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer
from secantus.update import UpdateError, apply_update, is_operator_form

WRAPPER = "Plan executor error during {cmd} :: caused by :: "


def _dollar_msg(field: str) -> str:
    return (
        f"The dollar ($) prefixed field '{field}' in '{field}' is not allowed in the "
        "context of an update's replacement document. Consider using an aggregation "
        "pipeline with $replaceWith."
    )


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


# --- the form decision ------------------------------------------------------


@pytest.mark.parametrize(
    ("update", "operator_form"),
    [
        ({"$set": {"a": 1}}, True),
        ({"$set": {"a": 1}, "z": 2}, True),
        ({"z": 2, "$set": {"a": 1}}, False),
        ({"y": 1, "z": 2, "$set": {"a": 1}}, False),
        ({"_id": 1, "$set": {"a": 1}}, False),
        ({"a.b": 1, "$set": {"a": 1}}, False),
        ({"a": 9}, False),
        # An empty update is a replacement of nothing -- that is how `{}`
        # reduces a stored document to its `_id`.
        ({}, False),
    ],
)
def test_the_first_key_decides_the_form(update, operator_form):
    assert is_operator_form(update) is operator_form


def test_operator_form_names_the_first_bare_key():
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 0}, {"$set": {"a": 1}, "y": 1, "z": 2})
    assert exc.value.code == 9
    assert "Unknown modifier: y" in str(exc.value)


# --- the replacement-form 52 ------------------------------------------------


@pytest.mark.parametrize(
    ("update", "named"),
    [
        ({"z": 2, "$set": {"a": 1}}, "$set"),
        ({"z": 2, "$weird": 3}, "$weird"),
        # The FIRST $-prefixed key is named, wherever it sits.
        ({"z": 1, "$aaa": 1, "$bbb": 2}, "$aaa"),
        ({"z": 1, "$aaa": 1, "y": 2}, "$aaa"),
        ({"_id": 1, "$set": {"a": 1}}, "$set"),
    ],
)
def test_a_dollar_key_in_a_replacement_is_refused(update, named):
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": 0}, update)
    assert exc.value.code == 52
    assert str(exc.value) == _dollar_msg(named)
    assert exc.value.exec_error, "mongod wraps this one"


@pytest.mark.parametrize(
    "update",
    [
        {"a": {"$bad": 1}},
        {"a": {"b": {"$bad": 1}}},
        {"a": [{"$bad": 1}]},
        {"a.b": 1},
        {"a": 9},
    ],
)
def test_only_the_top_level_is_restricted(update):
    """mongod 8.x stores a nested `$`-key, and a literal dotted key, verbatim."""
    out = apply_update({"_id": 1, "a": 0}, update)
    assert out == {"_id": 1, **update}


def test_an_upsert_inserts_the_document_verbatim():
    """The 52 is execution-time, so the upsert-insert path must not raise it."""
    out = apply_update({"_id": 99}, {"z": 2, "$set": {"a": 1}}, is_upsert=True)
    assert out == {"_id": 99, "z": 2, "$set": {"a": 1}}


# --- over the wire ----------------------------------------------------------


def _update_cmd(db, u, q=None, upsert=False):
    """The raw `update` command.

    pymongo's `update_one` helper refuses a bare-first document client-side
    ("update only works with $ operators"), so this shape reaches a server only
    through the raw command -- which is how the other drivers send it, and how
    it was probed against mongod.
    """
    return db.command("update", "c", updates=[{"q": q or {"_id": 1}, "u": u, "upsert": upsert}])


def test_a_replacement_dollar_key_is_wrapped_by_update(client):
    db = client["replform"]
    coll = db["c"]
    coll.drop()
    coll.insert_one({"_id": 1, "a": 0})
    reply = _update_cmd(db, {"z": 2, "$set": {"a": 1}})
    err = reply["writeErrors"][0]
    assert err["code"] == 52
    assert err["errmsg"] == WRAPPER.format(cmd="update") + _dollar_msg("$set")
    assert coll.find_one({"_id": 1}) == {"_id": 1, "a": 0}


def test_find_and_modify_names_itself(client):
    coll = client["replform"]["c"]
    coll.drop()
    coll.insert_one({"_id": 1, "a": 0})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        coll.find_one_and_replace({"_id": 1}, {"z": 2, "$set": {"a": 1}})
    assert exc.value.code == 52
    assert str(exc.value).startswith(WRAPPER.format(cmd="findAndModify"))


def test_no_match_is_a_silent_no_op(client):
    """Execution-time: nothing is replaced, so nothing complains."""
    coll = client["replform"]["c"]
    coll.drop()
    coll.insert_one({"_id": 1, "a": 0})
    # (the raw command below; pymongo's helper refuses this shape client-side)
    reply = _update_cmd(client["replform"], {"z": 2, "$set": {"a": 1}}, q={"_id": 99})
    assert "writeErrors" not in reply
    assert (reply["n"], reply["nModified"]) == (0, 0)
    assert list(coll.find({})) == [{"_id": 1, "a": 0}]


def test_an_upsert_over_the_wire_inserts_the_dollar_key(client):
    coll = client["replform"]["c"]
    coll.drop()
    coll.insert_one({"_id": 1, "a": 0})
    reply = _update_cmd(client["replform"], {"z": 2, "$set": {"a": 1}}, q={"_id": 99}, upsert=True)
    assert "writeErrors" not in reply
    assert reply["upserted"][0]["_id"] == 99
    # Field order is mongod's too: `_id` first, then the document as sent.
    got = coll.find_one({"_id": 99})
    assert got == {"_id": 99, "z": 2, "$set": {"a": 1}}
    assert list(got.keys()) == ["_id", "z", "$set"]


def test_operator_form_stays_parse_time(client):
    """The 9 fires with no match and on an upsert -- unlike the 52."""
    coll = client["replform"]["c"]
    coll.drop()
    coll.insert_one({"_id": 1, "a": 0})
    for upsert in (False, True):
        reply = _update_cmd(
            client["replform"], {"$set": {"a": 1}, "z": 2}, q={"_id": 99}, upsert=upsert
        )
        err = reply["writeErrors"][0]
        assert err["code"] == 9
        assert not err["errmsg"].startswith("Plan executor error")
