"""``collation`` spec validation, pinned to mongod 8.2.11 (probed 2026-08-31).

Before this, every invalid collation spec was ACCEPTED: a missing ``locale``,
``strength: 9``, a misspelled field. The query then ran under a different
collation than the caller asked for and reported success -- the same shape as
the ignored-argument class in ``tasks/backlog.md`` §3.

The rules are not symmetric, and each asymmetry here was measured rather than
inferred:

* an empty ``{}`` is fine, but any non-empty spec needs a ``locale``;
* ``strength: 0`` is an ENUM error and ``6`` a RANGE error, worded differently;
* ``strength: 2.5`` is accepted (truncates), ``strength: true`` is not;
* ``caseLevel`` rejects ``1`` -- strict bool, not the bool-or-number family;
* ``backwards`` uses the ``find``-family bool wording while its neighbours use
  the ``BSON field`` form.

The last class of test is the important one: ``update`` and ``delete`` do NOT
validate the spec's contents at all. Applying the validator uniformly would
reject specs mongod accepts, so those cases pin the non-validation as
deliberate.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer

VALIDATING = ["find", "aggregate", "count", "distinct", "findAndModify"]


def _command(name: str, spec: object) -> dict:
    return {
        "find": {"find": "c", "collation": spec},
        "aggregate": {"aggregate": "c", "pipeline": [], "cursor": {}, "collation": spec},
        "count": {"count": "c", "collation": spec},
        "distinct": {"distinct": "c", "key": "s", "collation": spec},
        "findAndModify": {
            "findAndModify": "c",
            "query": {"s": "zzz"},
            "remove": True,
            "collation": spec,
        },
    }[name]


@pytest.fixture
def server(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield mc
    finally:
        mc.close()


@pytest.fixture
def db(client: MongoClient):
    d = client["collation_spec_db"]
    d.c.drop()
    d.c.insert_one({"_id": 1, "s": "a"})
    return d


@pytest.mark.parametrize("command", VALIDATING)
@pytest.mark.parametrize(
    ("spec", "code", "errmsg"),
    [
        ({"strength": 2}, 40414, "BSON field 'collation.locale' is missing but a required field"),
        # An explicit null locale reads as MISSING, not as a wrong type.
        ({"locale": None}, 40414, "BSON field 'collation.locale' is missing but a required field"),
        (
            {"locale": 5},
            14,
            "BSON field 'collation.locale' is the wrong type 'int', expected type 'string'",
        ),
        (
            {"locale": "en", "strength": "x"},
            14,
            "BSON field 'collation.strength' is the wrong type 'string', "
            "expected types '[double, decimal, long, int]'",
        ),
        # bool is NOT a number for this field.
        (
            {"locale": "en", "strength": True},
            14,
            "BSON field 'collation.strength' is the wrong type 'bool', "
            "expected types '[double, decimal, long, int]'",
        ),
        (
            {"locale": "en", "strength": 6},
            2,
            "BSON field 'strength' value must be <= 5, actual value '6'",
        ),
        (
            {"locale": "en", "strength": -1},
            2,
            "BSON field 'strength' value must be >= 0, actual value '-1'",
        ),
        # ... but ZERO is in range and still invalid, with the enum wording.
        (
            {"locale": "en", "strength": 0},
            2,
            "Enumeration value '0' for field 'collation.strength' is not a valid value.",
        ),
        (
            {"locale": "en", "caseLevel": 1},
            14,
            "BSON field 'collation.caseLevel' is the wrong type 'int', expected type 'bool'",
        ),
        (
            {"locale": "en", "caseFirst": "sideways"},
            2,
            "Enumeration value 'sideways' for field 'collation.caseFirst' is not a valid value.",
        ),
        (
            {"locale": "en", "alternate": "nope"},
            2,
            "Enumeration value 'nope' for field 'collation.alternate' is not a valid value.",
        ),
        (
            {"locale": "en", "maxVariable": "nope"},
            2,
            "Enumeration value 'nope' for field 'collation.maxVariable' is not a valid value.",
        ),
        (
            {"locale": "en", "numericOrdering": "x"},
            14,
            "BSON field 'collation.numericOrdering' is the wrong type 'string', "
            "expected type 'bool'",
        ),
        # `backwards` uses mongod's OTHER boolean wording -- on this field alone.
        (
            {"locale": "en", "backwards": "x"},
            14,
            "Field 'backwards' should be a boolean value, but found: string",
        ),
        ({"locale": "en", "bogus": 1}, 40415, "BSON field 'collation.bogus' is an unknown field."),
    ],
)
def test_invalid_collation_spec_is_rejected(db, command, spec, code, errmsg) -> None:
    with pytest.raises(OperationFailure) as exc:
        db.command(_command(command, spec))
    assert exc.value.code == code
    assert exc.value.details["errmsg"] == errmsg


@pytest.mark.parametrize("command", VALIDATING)
@pytest.mark.parametrize(
    "spec",
    [
        {},  # empty is a no-op, and needs no locale
        {"locale": "en"},
        {"locale": "simple", "strength": 2},
        {"locale": "en", "strength": 1},
        {"locale": "en", "strength": 5},
        {"locale": "en", "strength": 2.5},  # truncates rather than erroring
        {"locale": "en", "caseFirst": "off"},
        {"locale": "en", "alternate": "non-ignorable"},
        {"locale": "en", "maxVariable": "punct"},
        {"locale": "en", "caseLevel": True, "numericOrdering": False, "backwards": False},
    ],
)
def test_valid_collation_spec_is_accepted(db, command, spec) -> None:
    db.command(_command(command, spec))


@pytest.mark.parametrize(
    "spec",
    [
        {"strength": 2},
        {"locale": 5},
        {"locale": "en", "strength": 9},
        {"locale": "en", "caseFirst": "sideways"},
        {"locale": "en", "bogus": 1},
    ],
)
def test_update_and_delete_do_not_validate_the_spec(db, spec) -> None:
    """mongod validates collation CONTENTS on five commands and not on these two.

    Probed 8.2.11: a missing locale, an out-of-range strength and a bad enum
    all run on ``update`` and ``delete``. These cases exist so the validator
    cannot later be applied "for consistency" and start rejecting specs mongod
    accepts -- the worse direction to be wrong in.
    """
    db.command({"update": "c", "updates": [{"q": {}, "u": {"$set": {"x": 1}}, "collation": spec}]})
    db.command({"delete": "c", "deletes": [{"q": {"s": "zzz"}, "limit": 0, "collation": spec}]})
