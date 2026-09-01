"""What an aggregation stage says about a bad SPEC.

Measured by `tools/probes/aggregation_stage_specs.py`, which crosses every stage
with every pathological argument -- 725 shapes, of which 167 disagreed with
mongod 8.2.11 when it was first run. The tests below are the ones where we
answered wrongly rather than merely phrasing an error differently; wording is
the probe's job.
"""

from __future__ import annotations

import pytest
from bson import Code
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def db(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            database = client["testdb"]
            database["things"].insert_one({"_id": 1, "n": 5})
            yield database
        finally:
            client.close()


# --- $unset validated nothing --------------------------------------------
@pytest.mark.parametrize(
    "spec,code",
    [
        ("", 40352),  # accepted, did nothing
        ([], 31119),  # accepted, did nothing
        ([1], 31120),  # crashed
        (5, 31002),  # crashed
        ({}, 31002),  # silently iterated the document's KEYS
        (Code("x=1"), 31002),  # accepted: Code subclasses str
    ],
)
def test_unset_spec_is_validated(db, spec, code):
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$unset": spec}]))
    assert exc.value.code == code


def test_unset_still_works(db):
    out = list(db["things"].aggregate([{"$unset": "n"}]))
    assert out == [{"_id": 1}]


# --- namespaces ----------------------------------------------------------
@pytest.mark.parametrize("stage", ["$out", "$merge"])
def test_empty_target_namespace_is_refused(db, stage):
    # Both used to be accepted, writing to a nameless collection.
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{stage: ""}]))
    assert exc.value.code == 73


def test_out_reports_a_missing_required_field(db):
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$out": {"db": "x"}}]))
    assert exc.value.code == 40414 and "'$out.coll'" in str(exc.value)


def test_out_reports_an_unknown_field(db):
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$out": {"coll": "c", "bogus": 1}}]))
    assert exc.value.code == 40415 and "'$out.bogus'" in str(exc.value)


def test_merge_takes_into_not_coll(db):
    # $out takes {db, coll}; $merge takes {into: ...} and rejects the others.
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$merge": {"coll": "c"}}]))
    assert exc.value.code == 40415


def test_merge_rejects_an_empty_into(db):
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$merge": {"into": ""}}]))
    assert exc.value.code == 5786800


# --- $documents is collection-less ---------------------------------------
def test_documents_requires_a_collectionless_aggregate(db):
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$documents": [{"_id": 1}]}]))
    assert exc.value.code == 73
    assert "can only be run with {aggregate: 1}" in str(exc.value)


def test_documents_works_collectionless(db):
    assert [d["_id"] for d in db.aggregate([{"$documents": [{"_id": 7}]}])] == [7]


# --- a Code spec is not a string spec ------------------------------------
@pytest.mark.parametrize(
    "stage,code",
    [("$count", 40156), ("$out", 16990), ("$merge", 14), ("$unwind", 15981)],
)
def test_a_code_spec_is_not_a_string_spec(db, stage, code):
    # `Code` subclasses `str`, so these took the string branch: three crashed
    # with `internal server error` and $unwind reported the wrong complaint.
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{stage: Code("x=1")}]))
    assert exc.value.code == code


# --- $set names itself, not its alias ------------------------------------
def test_set_reports_its_own_name(db):
    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$set": "x"}]))
    assert "$set specification stage must be an object" in str(exc.value)


# --- the two value renderings --------------------------------------------
def test_stage_errors_use_the_stage_rendering(db):
    """mongod has two renderings; the stage family has no inner spaces in
    containers and quotes a Binary's hex. See `bsontypes`."""
    from bson import Binary

    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$redact": Binary(b"z")}]))
    assert 'BinData(0, "7A")' in str(exc.value)

    with pytest.raises(OperationFailure) as exc:
        list(db["things"].aggregate([{"$limit": Binary(b"z")}]))
    # ...whereas $limit's message is the QUERY family: unquoted hex.
    assert "BinData(0, 7A)" in str(exc.value)
