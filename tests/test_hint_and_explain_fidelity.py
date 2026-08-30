"""`hint` honouring and `explain`'s error handling match mongod's.

Phase 2 of ``tasks/remaining-work-plan.md``, third surface. The headline is a
**write that should not have happened**:

    ``delete`` and ``update`` take a per-statement ``hint``, and mongod refuses
    the STATEMENT when it names no index -- ``n: 0`` plus a writeError. Both
    commands ignored the field outright and PERFORMED the write. A caller who
    hinted a typo'd index name had their delete applied where MongoDB declines
    to run it.

Two more of the same family: ``explain`` -- the command you run to *check* a
hint -- was the only read path that did not validate one, and it fabricated a
plausible ``COLLSCAN`` plan for commands that do not exist.

Probed against mongod 6.0.16.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture
def db(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        d = cli["hint"]
        d.c.insert_many([{"_id": i, "a": i} for i in range(1, 6)])
        d.c.create_index([("a", 1)], name="a_1")
        yield d
    finally:
        cli.close()
        srv.stop()


def _err(db, cmd):
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command(cmd)
    return exc.value


# --- the write paths --------------------------------------------------------


def test_delete_with_an_unresolvable_hint_does_not_delete(db) -> None:
    """The regression, and the reason this batch exists: the documents were
    deleted. mongod refuses the statement."""
    reply = db.command({"delete": "c", "deletes": [{"q": {}, "limit": 1, "hint": "nope"}]})
    assert reply["n"] == 0
    assert reply["writeErrors"][0]["index"] == 0
    assert reply["writeErrors"][0]["code"] == 2
    assert db.c.count_documents({}) == 5, "nothing may have been deleted"


def test_update_with_an_unresolvable_hint_does_not_update(db) -> None:
    reply = db.command(
        {"update": "c", "updates": [{"q": {}, "u": {"$set": {"z": 1}}, "hint": "nope"}]}
    )
    assert reply["n"] == 0
    assert reply["nModified"] == 0
    assert reply["writeErrors"][0]["code"] == 2
    assert db.c.count_documents({"z": 1}) == 0, "nothing may have been updated"


def test_a_resolvable_hint_still_writes(db) -> None:
    reply = db.command({"delete": "c", "deletes": [{"q": {}, "limit": 1, "hint": "a_1"}]})
    assert reply["n"] == 1
    assert "writeErrors" not in reply


def test_an_empty_hint_means_no_hint(db) -> None:
    reply = db.command({"delete": "c", "deletes": [{"q": {}, "limit": 1, "hint": {}}]})
    assert reply["n"] == 1


def test_hint_by_key_spec_resolves(db) -> None:
    reply = db.command(
        {"update": "c", "updates": [{"q": {}, "u": {"$set": {"z": 1}}, "hint": {"a": 1}}]}
    )
    assert reply["n"] == 1
    assert "writeErrors" not in reply


def test_an_unordered_batch_continues_past_a_bad_hint(db) -> None:
    """The hint failure is a per-STATEMENT write error, so an unordered batch
    runs the rest -- the same shape the other per-statement errors use."""
    reply = db.command(
        {
            "delete": "c",
            "ordered": False,
            "deletes": [
                {"q": {"_id": 1}, "limit": 1, "hint": "nope"},
                {"q": {"_id": 2}, "limit": 1},
            ],
        }
    )
    assert reply["n"] == 1
    assert [w["index"] for w in reply["writeErrors"]] == [0]
    assert db.c.find_one({"_id": 1}) is not None
    assert db.c.find_one({"_id": 2}) is None


def test_an_ordered_batch_stops_at_a_bad_hint(db) -> None:
    reply = db.command(
        {
            "delete": "c",
            "deletes": [
                {"q": {"_id": 1}, "limit": 1, "hint": "nope"},
                {"q": {"_id": 2}, "limit": 1},
            ],
        }
    )
    assert reply["n"] == 0
    assert db.c.count_documents({}) == 5


# --- $natural direction -----------------------------------------------------


def test_reverse_natural_hint_walks_backwards(db) -> None:
    """``{$natural: -1}`` is a REVERSE collection scan. We resolved both
    directions to the same "$natural" token, dropping the sign, so a caller
    asking for reverse insertion order silently got forward order."""
    ids = [d["_id"] for d in db.c.find({}, hint={"$natural": -1})]
    assert ids == [5, 4, 3, 2, 1]


def test_forward_natural_hint_is_unchanged(db) -> None:
    ids = [d["_id"] for d in db.c.find({}, hint={"$natural": 1})]
    assert ids == [1, 2, 3, 4, 5]


def test_an_explicit_sort_still_wins_over_reverse_natural(db) -> None:
    ids = [d["_id"] for d in db.c.find({}, hint={"$natural": -1}).sort("a", 1)]
    assert ids == [1, 2, 3, 4, 5]


# --- explain ----------------------------------------------------------------


def test_explain_validates_the_hint(db) -> None:
    """explain is what you run to CHECK a hint, and it was the one read path
    that did not validate one -- reporting a COLLSCAN, which told the caller
    their hint was fine and that it was being ignored in the same breath."""
    err = _err(db, {"explain": {"find": "c", "hint": "nope"}, "verbosity": "queryPlanner"})
    assert err.code == 2
    assert "does not correspond to an existing index" in err.details["errmsg"]


def test_explain_of_an_unknown_command_is_an_error(db) -> None:
    """It used to fabricate a plausible COLLSCAN plan, ``ok: 1``, for a command
    that does not exist."""
    err = _err(db, {"explain": {"nosuchcmd": "c"}, "verbosity": "queryPlanner"})
    assert err.code == 59
    assert err.details["errmsg"] == "Explain failed due to unknown command: nosuchcmd"


def test_explain_of_an_empty_document_is_an_error(db) -> None:
    err = _err(db, {"explain": {}, "verbosity": "queryPlanner"})
    assert err.code == 59


def test_explain_of_a_non_document_is_a_type_error(db) -> None:
    err = _err(db, {"explain": 5, "verbosity": "queryPlanner"})
    assert err.code == 14
    assert err.details["errmsg"] == (
        "BSON field 'explain.explain' is the wrong type 'int', expected type 'object'"
    )


def test_explain_verbosity_wrong_type_is_a_type_error(db) -> None:
    """mongod separates "wrong type" from "not a valid enum value"; we reported
    our own wording for both."""
    err = _err(db, {"explain": {"find": "c"}, "verbosity": 5})
    assert err.code == 14
    assert err.details["errmsg"] == (
        "BSON field 'explain.verbosity' is the wrong type 'int', expected type 'string'"
    )


def test_explain_verbosity_bad_enum_value(db) -> None:
    err = _err(db, {"explain": {"find": "c"}, "verbosity": "nope"})
    assert err.code == 2
    assert err.details["errmsg"] == (
        "Enumeration value 'nope' for field 'explain.verbosity' is not a valid value."
    )


def test_a_normal_explain_still_works(db) -> None:
    reply = db.command({"explain": {"find": "c", "filter": {"a": 3}}, "verbosity": "queryPlanner"})
    assert reply["ok"] == 1.0
    assert reply["queryPlanner"]["namespace"] == "hint.c"


def test_explain_with_a_resolvable_hint_still_works(db) -> None:
    reply = db.command({"explain": {"find": "c", "hint": "a_1"}, "verbosity": "queryPlanner"})
    assert reply["queryPlanner"]["winningPlan"]["inputStage"]["indexName"] == "a_1"


# --- distinct ---------------------------------------------------------------


def test_distinct_rejects_an_unknown_field(db) -> None:
    """It accepted any field and ignored it, so a misspelled option was
    silently dropped."""
    err = _err(db, {"distinct": "c", "key": "a", "zz": 1})
    assert err.code == 40415
    assert err.details["errmsg"] == "BSON field 'distinctCommandRequest.zz' is an unknown field."


def test_distinct_still_works(db) -> None:
    assert sorted(db.command({"distinct": "c", "key": "a"})["values"]) == [1, 2, 3, 4, 5]


def test_distinct_accepts_hint_despite_6_0_rejecting_it(db) -> None:
    """Deliberate: mongod 6.0.16 answers `40415` for `distinct.hint`, but a
    later release added the option, so a current driver may legitimately send
    it. Accepting is the safe direction for a field whose status changed."""
    assert db.command({"distinct": "c", "key": "a", "hint": "a_1"})["ok"] == 1.0
