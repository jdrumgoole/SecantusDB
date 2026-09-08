"""Update errors the Rust server named wrongly, or not at all.

Two defects, measured against mongod 8.2.11 on 2026-09-08:

1. **`$inc` / `$mul` with a non-numeric OPERAND carried a wrapper mongod does
   not send.** mongod has two shapes for the same code 14 and wraps only one:
   a bad *operand* is readable from the update spec alone and comes back BARE,
   while a bad stored *field* is discoverable only against a document and is
   wrapped `Plan executor error during update :: caused by ::`. Both were being
   reported as execution-time.
2. **`$position` / `$slice` / `$bit` with a bool argument answered the generic
   refusal.** The code (2) was right and the guards already existed -- they just
   deferred, which on this server is `query uses a construct the Rust server
   does not support`, blaming the operator for a bad argument.

`$position` and `$slice` are worded DIFFERENTLY by mongod ("not of type:" vs
"but was given type:"), which is why each is measured rather than shared.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_upderr") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["upderr"]
    d.c.drop()
    d.c.insert_one({"_id": 1, "n": 5, "s": "str", "arr": [1, 2], "big": 2**63 - 1})
    try:
        yield d
    finally:
        cli.close()


# (update, code, exact message) -- mongod 8.2.11.
BARE = [
    ({"$inc": {"n": "x"}}, 14, 'Cannot increment with non-numeric argument: {n: "x"}'),
    ({"$inc": {"n": True}}, 14, "Cannot increment with non-numeric argument: {n: true}"),
    ({"$mul": {"n": "x"}}, 14, 'Cannot multiply with non-numeric argument: {n: "x"}'),
    (
        {"$push": {"arr": {"$each": [1], "$position": True}}},
        2,
        "The value for $position must be an integer value, not of type: bool",
    ),
    (
        {"$push": {"arr": {"$each": [1], "$slice": True}}},
        2,
        "The value for $slice must be an integer value but was given type: bool",
    ),
    (
        {"$bit": {"n": {"and": True}}},
        2,
        "The $bit modifier field must be an Integer(32/64 bit); "
        "a 'bool' is not supported here: {and: true}",
    ),
    ({"$pop": {"arr": True}}, 9, "Expected a number in: arr: true"),
    ({"$pop": {"arr": 5}}, 9, "$pop expects 1 or -1, found: 5"),
]


@pytest.mark.parametrize("update,code,message", BARE, ids=[str(u)[:28] for u, _, _ in BARE])
def test_parse_errors_are_bare_and_exact(db, update, code, message):
    """A parse error is readable from the spec, so mongod sends no wrapper."""
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        db.c.update_one({"_id": 1}, update)
    assert e.value.code == code
    assert message in str(e.value)
    assert "Plan executor error" not in str(e.value)


# The same code 14, but discoverable only against a stored document.
WRAPPED = [
    ({"$inc": {"s": 1}}, 14, "Cannot apply $inc to a value of non-numeric type"),
    ({"$push": {"s": 1}}, 2, "must be an array but is of type string"),
]


@pytest.mark.parametrize("update,code,fragment", WRAPPED, ids=["inc-field", "push-non-array"])
def test_execution_errors_keep_the_wrapper(db, update, code, fragment):
    """The other half of the rule: these DO carry mongod's executor wrapper."""
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        db.c.update_one({"_id": 1}, update)
    assert e.value.code == code
    assert str(e.value).startswith("Plan executor error during update :: caused by ::")
    assert fragment in str(e.value)


def test_valid_updates_still_apply(db):
    db.c.update_one({"_id": 1}, {"$inc": {"n": 1}})
    assert db.c.find_one({"_id": 1})["n"] == 6
    db.c.update_one({"_id": 1}, {"$push": {"arr": {"$each": [9], "$position": 0}}})
    assert db.c.find_one({"_id": 1})["arr"][0] == 9
    db.c.update_one({"_id": 1}, {"$bit": {"n": {"and": 3}}})
    assert db.c.find_one({"_id": 1})["n"] == 6 & 3
