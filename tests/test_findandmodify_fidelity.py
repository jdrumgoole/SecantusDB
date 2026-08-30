"""``findAndModify`` argument validation and reply shape match mongod's.

Found by differential-probing 18 findAndModify shapes against a real mongod:
6 diverged. One was a crash, two were silent acceptances of commands mongod
rejects, and one returned an update-shaped reply for a delete.

Probed on mongod **6.0.16** — the version the live differential gate
(``tests/test_mongod_differential.py``) spawns — and cross-checked on 8.3.4.
All the behaviour below is identical on both; only the error *wording* differs
(8.3 quotes the field names, e.g. ``both an 'update' and 'remove'=true``).
SecantusDB advertises 7.0, so 6.0's wording is what ships.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture(autouse=True)
def _fresh_databases(db):
    """Drop everything this test made, so the shared server looks new to the next.

    The isolation a per-test server gave for free, without paying for a server.
    Runs AFTER the test so a failure leaves its data in place for inspection.
    """
    yield
    _client = db.client
    for _name in _client.list_database_names():
        if _name not in ("admin", "local", "config"):
            _client.drop_database(_name)


# Module-scoped: one server for the file, with `_fresh_databases`
# below giving each test the clean slate a per-test server used to.
@pytest.fixture(scope="module")
def db(wt_home_module):
    srv = SecantusDBServer(port=0, storage_path=wt_home_module)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli["fam"]
    finally:
        cli.close()
        srv.stop()


def _fam(db, **extras):
    cmd = {"findAndModify": "c"}
    cmd.update(extras)
    return db.command(cmd)


def test_bad_update_type_does_not_crash(db) -> None:
    """The regression: a non-document `update` reached apply_update, which did
    `update.keys()` and raised AttributeError -- surfacing as a bare
    "internal server error" (code 1)."""
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update=5)
    assert exc.value.code == 9
    assert "Update argument must be either an object or an array" in str(exc.value)
    assert "internal server error" not in str(exc.value)


@pytest.mark.parametrize("bad", [5, "x", 1.5, True])
def test_every_non_document_update_is_rejected(db, bad) -> None:
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update=bad)
    assert exc.value.code == 9


def test_remove_with_new_is_rejected(db) -> None:
    """mongod refuses rather than ignoring `new` -- a remove has no "after"
    document. We used to accept it and delete anyway."""
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, remove=True, new=True)
    assert exc.value.code == 9
    assert "new=true and remove=true" in str(exc.value)
    assert db.c.count_documents({}) == 1, "the document must not be removed"


def test_remove_with_upsert_is_rejected(db) -> None:
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, remove=True, upsert=True)
    assert exc.value.code == 9
    assert "upsert=true and remove=true" in str(exc.value)
    assert db.c.count_documents({}) == 1


def test_remove_with_update_is_rejected(db) -> None:
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, remove=True, update={"$set": {"a": 1}})
    assert exc.value.code == 9
    assert "an update and remove=true" in str(exc.value)


def test_remove_reply_omits_updated_existing(db) -> None:
    """`updatedExisting` describes an UPDATE; mongod omits it for a remove.
    We emitted it, so a driver reading the field saw an update-shaped reply
    for a delete."""
    db.c.insert_one({"_id": 1, "a": 1})
    reply = _fam(db, query={"_id": 1}, remove=True)
    assert reply["lastErrorObject"] == {"n": 1}
    assert reply["value"] == {"_id": 1, "a": 1}


def test_remove_no_match_reply_omits_updated_existing(db) -> None:
    db.c.insert_one({"_id": 1})
    reply = _fam(db, query={"_id": 9}, remove=True)
    assert reply["lastErrorObject"] == {"n": 0}
    assert reply["value"] is None


def test_update_replies_still_carry_updated_existing(db) -> None:
    """The omission is remove-only -- an update must keep the field."""
    db.c.insert_one({"_id": 1, "a": 1})
    reply = _fam(db, query={"_id": 1}, update={"$set": {"a": 2}})
    assert reply["lastErrorObject"] == {"n": 1, "updatedExisting": True}

    miss = _fam(db, query={"_id": 9}, update={"$set": {"a": 2}})
    assert miss["lastErrorObject"] == {"n": 0, "updatedExisting": False}


def test_upsert_reply_shape_is_unchanged(db) -> None:
    reply = _fam(db, query={"_id": 5}, update={"$set": {"a": 1}}, upsert=True)
    leo = reply["lastErrorObject"]
    assert leo["n"] == 1
    assert leo["updatedExisting"] is False
    assert leo["upserted"] == 5


def test_valid_remove_and_update_still_work(db) -> None:
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 2}])
    assert _fam(db, query={"_id": 1}, update={"$set": {"a": 9}}, new=True)["value"]["a"] == 9
    assert _fam(db, query={"_id": 2}, remove=True)["value"] == {"_id": 2, "a": 2}
    assert db.c.count_documents({}) == 1


# ---------------------------------------------------------------------------
# A second differential pass over findAndModify (2026-08-29) -- 49 option
# combinations against mongod 6.0.16, of which 14 diverged. The interesting
# ones were not the error wordings: `update: {}` silently kept every field the
# client asked to drop, an upsert whose query used a dotted path stored a
# literal key with a dot in it, `new` was never type-checked, and `hint` was
# accepted and ignored. Everything below is probed, not inferred.
# ---------------------------------------------------------------------------

BOOL_OR_NUMBER = "expected types '[int, decimal, long, bool, double]'"


def test_unknown_top_level_field_is_rejected(db) -> None:
    """We accepted anything and ran the write, so a misspelled option was
    silently dropped and the caller got a correct-looking reply computed under
    options they did not ask for."""
    db.c.insert_one({"_id": 1, "n": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, zz=1)
    assert exc.value.code == 40415
    # 8.x gave 40415 a symbolic name; 6.0 rendered it as Location40415.
    assert exc.value.details["codeName"] == "IDLUnknownField"
    assert str(exc.value).startswith("BSON field 'findAndModify.zz' is an unknown field.")
    assert db.c.find_one({"_id": 1})["n"] == 5, "the write must not have run"


@pytest.mark.parametrize("field", ["new", "remove", "upsert"])
@pytest.mark.parametrize("bad", ["yes", [1], {}])
def test_bool_flags_reject_non_numeric_types(db, field, bad) -> None:
    db.c.insert_one({"_id": 1, "n": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, **{field: bad})
    assert exc.value.code == 14
    assert f"BSON field 'findAndModify.{field}' is the wrong type" in str(exc.value)
    # mongod's own quoting: the closing quote sits INSIDE the bracket.
    assert BOOL_OR_NUMBER in str(exc.value)


def test_new_accepts_numbers_and_null_like_mongod(db) -> None:
    """`new` takes the same bool-or-number rule as `upsert`. Untyped, a string
    went through Python truthiness -- `new: "no"` returned the POST image."""
    db.c.insert_one({"_id": 1, "n": 5})
    assert _fam(db, query={"_id": 1}, update={"$set": {"n": 6}}, new=1)["value"]["n"] == 6
    assert _fam(db, query={"_id": 1}, update={"$set": {"n": 7}}, new=0)["value"]["n"] == 6
    assert _fam(db, query={"_id": 1}, update={"$set": {"n": 8}}, new=None)["value"]["n"] == 7


def test_zero_valued_decimal_flag_is_false(db) -> None:
    """`Decimal128` has no `__bool__`, so every instance is truthy in Python and
    `new: Decimal128("0")` would mean the opposite of what it says."""
    from bson import Decimal128

    db.c.insert_one({"_id": 1, "n": 5})
    reply = _fam(db, query={"_id": 1}, update={"$set": {"n": 6}}, new=Decimal128("0"))
    assert reply["value"]["n"] == 5, "new=0 must return the pre-image"


def test_array_filters_type_errors_name_the_findandmodify_field(db) -> None:
    """We reported a field path that does not exist on this command
    (`update.updates.arrayFilters.0`), naming the wrong type as well."""
    db.c.insert_one({"_id": 1, "n": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, arrayFilters={"e": 1})
    assert exc.value.code == 14
    assert exc.value.details["errmsg"] == (
        "BSON field 'findAndModify.arrayFilters' is the wrong type 'object', expected type 'array'"
    )


def test_array_filters_element_type_error(db) -> None:
    db.c.insert_one({"_id": 1, "n": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, arrayFilters=[5])
    assert exc.value.code == 14
    assert exc.value.details["errmsg"] == (
        "BSON field 'findAndModify.arrayFilters.0' is the wrong type 'int', expected type 'object'"
    )


def test_null_array_filters_reads_as_absent(db) -> None:
    """On 6.0 an explicit null took an older path and answered Location10065.
    8.x treats it as if the field had not been sent, so the update just runs."""
    db.c.insert_one({"_id": 1, "n": 5})
    reply = _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, arrayFilters=None)
    assert reply["lastErrorObject"]["updatedExisting"] is True
    assert db.c.find_one({"_id": 1})["n"] == 1


def test_missing_array_filter_identifier(db) -> None:
    db.c.insert_one({"_id": 1, "arr": [1, 2, 3]})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$set": {"arr.$[e]": 1}})
    assert exc.value.code == 2
    assert "No array filter found for identifier 'e' in path 'arr.$[e]'" in str(exc.value)


# --- update-error codes and the execution wrapper --------------------------
#
# These used to escape to dispatch's generic handler, which reported EVERY one
# of them as 14 TypeMismatch -- so a driver's canonical handling, which keys on
# 66 / 9 / 40, never fired.

WRAPPER = "Plan executor error during findAndModify :: caused by :: "


@pytest.mark.parametrize(
    "seed,update,code,name",
    [
        ({"_id": 1, "n": 5}, {"$nope": {"n": 1}}, 9, "FailedToParse"),
        ({"_id": 1, "n": 5}, {"$set": {"n": 1}, "z": 2}, 9, "FailedToParse"),
        ({"_id": 1, "n": 5}, {"$set": {"_id": 9}}, 66, "ImmutableField"),
        (
            {"_id": 1, "n": 5},
            {"$set": {"a": 2}, "$inc": {"a.b": 1}},
            40,
            "ConflictingUpdateOperators",
        ),
        ({"_id": 1, "n": 5}, {"$set": {"n.x": 1}}, 28, "PathNotViable"),
        ({"_id": 1, "n": "x"}, {"$inc": {"n": 1}}, 14, "TypeMismatch"),
    ],
)
def test_update_error_keeps_its_code_and_name(db, seed, update, code, name) -> None:
    db.c.insert_one(dict(seed))
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update=update)
    assert exc.value.code == code
    assert exc.value.details["codeName"] == name


def test_execution_errors_carry_the_wrapper(db) -> None:
    """6.0.16 wraps findAndModify's EXECUTION errors -- the ones that depend on
    the stored document -- with a command-named prefix. (The `update` command
    on the same server does not; that asymmetry is real and probed.)"""
    db.c.insert_one({"_id": 1, "n": "x"})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$inc": {"n": 1}})
    assert str(exc.value).startswith(WRAPPER)
    assert "Cannot apply $inc to a value of non-numeric type" in str(exc.value)


def test_parse_errors_are_not_wrapped(db) -> None:
    """The other half of the rule: a complaint readable from the update
    document alone comes back bare."""
    db.c.insert_one({"_id": 1, "n": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$nope": {"n": 1}})
    assert not str(exc.value).startswith(WRAPPER)
    assert str(exc.value).startswith("Unknown modifier: $nope.")


# --- hint ------------------------------------------------------------------


def test_unknown_hint_is_rejected(db) -> None:
    """`hint` was accepted and dropped on the floor, so hinting an index that
    does not exist got a silent collection scan and an ok: 1 reply."""
    db.c.insert_one({"_id": 1, "n": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, hint="nope")
    assert exc.value.code == 2
    assert exc.value.details["codeName"] == "BadValue"
    assert db.c.find_one({"_id": 1})["n"] == 5, "the write must not have run"


def test_natural_hint_is_not_valid_here(db) -> None:
    """Unlike `find`, findAndModify rejects `$natural` (probed 6.0.16)."""
    db.c.insert_one({"_id": 1, "n": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, hint="$natural")
    assert exc.value.code == 2


def test_hint_wrong_type_is_failed_to_parse(db) -> None:
    db.c.insert_one({"_id": 1, "n": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update={"$set": {"n": 1}}, hint=5)
    assert exc.value.code == 9
    assert str(exc.value).startswith("Hint must be a string or an object")


@pytest.mark.parametrize("hint", ["_id_", {"_id": 1}, {}])
def test_resolvable_hints_are_honoured(db, hint) -> None:
    db.c.insert_one({"_id": 1, "n": 5})
    assert (
        _fam(db, query={"_id": 1}, update={"$set": {"n": 6}}, hint=hint, new=True)["value"]["n"]
        == 6
    )


def test_a_real_index_can_be_hinted(db) -> None:
    db.c.insert_many([{"_id": 1, "n": 5}, {"_id": 2, "n": 7}])
    db.c.create_index([("n", 1)], name="n_1")
    reply = _fam(db, query={"n": 7}, update={"$set": {"hit": 1}}, hint="n_1", new=True)
    assert reply["value"]["_id"] == 2


# --- the upserted document -------------------------------------------------


def test_upserted_value_leads_with_id(db) -> None:
    """mongod orders an upserted document `_id` first, then the query-seeded
    fields, then the update's -- each group in field-name order. Ours appended
    `_id` LAST, and BSON keeps field order on the wire."""
    reply = _fam(db, query={"b": 1, "a": 2}, update={"$set": {"y": 3}}, upsert=True, new=True)
    assert list(reply["value"]) == ["_id", "a", "b", "y"]


def test_upsert_from_a_dotted_query_builds_the_nesting(db) -> None:
    """The regression: `{"sub.k": 77}` upserted a document with a LITERAL key
    containing a dot -- one mongod cannot produce, most drivers refuse to send,
    and which then never matched the query that created it."""
    reply = _fam(db, query={"sub.k": 77}, update={"$set": {"y": 1}}, upsert=True, new=True)
    assert reply["value"]["sub"] == {"k": 77}
    assert "sub.k" not in reply["value"]
    assert db.c.count_documents({"sub.k": 77}) == 1, "the upserted doc must match its own query"


def test_upsert_from_a_deep_dotted_query(db) -> None:
    reply = _fam(db, query={"a.b.c": 5}, update={"$set": {"y": 1}}, upsert=True, new=True)
    assert reply["value"]["a"] == {"b": {"c": 5}}


def test_dotted_query_and_update_merge_into_one_subdocument(db) -> None:
    reply = _fam(db, query={"a.b": 5}, update={"$set": {"a.c": 1}}, upsert=True, new=True)
    assert reply["value"]["a"] == {"b": 5, "c": 1}


def test_operator_valued_dotted_predicate_is_not_seeded(db) -> None:
    """Only bare equalities seed the upsert; `{a.b: {$gt: 5}}` must not."""
    reply = _fam(db, query={"a.b": {"$gt": 5}}, update={"$set": {"y": 1}}, upsert=True, new=True)
    assert "a" not in reply["value"]
