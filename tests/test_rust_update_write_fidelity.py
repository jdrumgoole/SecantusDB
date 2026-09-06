"""What the RUST server stores, and how it reports an update it refused.

Three defects, all measured against mongod 8.2.11 on 2026-09-06 and all
Rust-server-only -- the Python server was already right on every shape here:

1. **Silent data loss.** ``{$set: {a: -0.0}}`` over ``a: 0.0`` was dropped. The
   storage write guard compared ``new != doc``, and ``Bson::Double``'s ``f64 ==``
   calls the two zeros equal, so the write was skipped, ``nModified`` was 0, no
   oplog entry was emitted, and a read-back returned the OLD zero. The value the
   caller asked to store was never stored.
2. **An overflow claimed the server could not do ``$inc``.** An ``$inc`` /
   ``$mul`` past int64 could only *defer*, and a defer has no Python behind it on
   the Rust server, so it surfaced as ``2 BadValue: query uses a construct the
   Rust server does not support`` -- which is not what happened; the server does
   ``$inc``, and it was the RESULT that did not fit.
3. **Every execution-time update error came back bare.** mongod reports the
   update errors that depend on the stored document under
   ``Plan executor error during <command> :: caused by ::`` and leaves the parse
   errors readable from the update spec alone bare. The Rust server had the
   message bodies right and the wrapper on none of them.

The Rust unit tests in ``secantus-core`` / ``secantus-storage`` pin the engine
and the storage layer; this file is the end-to-end proof over the wire, which is
where a client actually sees these. Gated on the ``_secantus_server`` extension,
like ``test_rust_server_smoke.py``.
"""

from __future__ import annotations

import math

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from bson.int64 import Int64  # noqa: E402

_WRAPPER = "Plan executor error during {cmd} :: caused by :: "


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_writefidelity") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def coll(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    c = cli["writefidelity"]["c"]
    c.drop()
    try:
        yield c
    finally:
        cli.close()


def _is_negative_zero(v) -> bool:
    return isinstance(v, float) and v == 0.0 and math.copysign(1.0, v) < 0


# --- 1. the signed zero must be STORED --------------------------------------


@pytest.mark.parametrize(
    ("seed", "update", "field", "check"),
    [
        ({"a": 0.0}, {"$set": {"a": -0.0}}, "a", _is_negative_zero),
        ({"a": -0.0}, {"$set": {"a": 0.0}}, "a", lambda v: v == 0.0 and not _is_negative_zero(v)),
        ({"a": {"b": 0.0}}, {"$set": {"a.b": -0.0}}, "a", lambda v: _is_negative_zero(v["b"])),
        ({"a": [0.0]}, {"$set": {"a.0": -0.0}}, "a", lambda v: _is_negative_zero(v[0])),
        # A second, genuinely-unchanged field must not mask the changed one.
        (
            {"a": 0.0, "b": 1},
            {"$set": {"a": -0.0, "b": 1}},
            "a",
            _is_negative_zero,
        ),
    ],
)
def test_signed_zero_update_is_stored(coll, seed, update, field, check):
    coll.insert_one({"_id": 1, **seed})
    res = coll.update_one({"_id": 1}, update)
    assert res.modified_count == 1, f"{update} over {seed} must report one modified"
    got = coll.find_one({"_id": 1})[field]
    assert check(got), f"{update} over {seed} stored {got!r}"


def test_replacing_a_document_with_a_signed_zero_is_stored(coll):
    coll.insert_one({"_id": 1, "a": 0.0})
    res = coll.replace_one({"_id": 1}, {"a": -0.0})
    assert res.modified_count == 1
    assert _is_negative_zero(coll.find_one({"_id": 1})["a"])


def test_a_genuine_no_op_update_still_reports_nothing_modified(coll):
    """The other half: an unchanged document must not be written."""
    coll.insert_one({"_id": 1, "a": 0.0})
    res = coll.update_one({"_id": 1}, {"$set": {"a": 0.0}})
    assert res.matched_count == 1
    assert res.modified_count == 0


# --- 2. an int64 overflow names the operator, the value and the document -----


@pytest.mark.parametrize(
    ("seed", "update", "expected"),
    [
        (
            {"a": Int64(2**63 - 1)},
            {"$inc": {"a": 1}},
            "Failed to apply $inc operations to current value "
            "((NumberLong)9223372036854775807) for document {_id: 1}",
        ),
        (
            {"a": Int64(-(2**63))},
            {"$inc": {"a": -1}},
            "Failed to apply $inc operations to current value "
            "((NumberLong)-9223372036854775808) for document {_id: 1}",
        ),
        (
            {"a": Int64(2**62)},
            {"$mul": {"a": 4}},
            "Failed to apply $mul operations to current value "
            "((NumberLong)4611686018427387904) for document {_id: 1}",
        ),
        (
            {"a": Int64(2**62)},
            {"$inc": {"a": Int64(2**62)}},
            "Failed to apply $inc operations to current value "
            "((NumberLong)4611686018427387904) for document {_id: 1}",
        ),
    ],
)
def test_int64_overflow_reports_mongods_message(coll, seed, update, expected):
    coll.insert_one({"_id": 1, **seed})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        coll.update_one({"_id": 1}, update)
    assert exc.value.code == 2
    # Execution-time, so it carries mongod's wrapper.
    assert str(exc.value).startswith(_WRAPPER.format(cmd="update") + expected), str(exc.value)
    # The document is untouched.
    assert coll.find_one({"_id": 1})["a"] == seed["a"]


# --- 3. the execution-vs-parse wrapper --------------------------------------

# Every verdict below was measured against mongod 8.2.11 (2026-09-06).
_EXECUTION_TIME = [
    ({"a": 1}, {"$push": {"a": 2}}, "The field 'a' must be an array but is of type int"),
    ({"a": 1}, {"$pull": {"a": 2}}, "Cannot apply $pull to a non-array value"),
    ({"a": 1}, {"$pullAll": {"a": [2]}}, "Cannot apply $pull to a non-array value"),
    ({"a": 1}, {"$addToSet": {"a": 2}}, "Cannot apply $addToSet to non-array field"),
    ({"a": 1}, {"$pop": {"a": 1}}, "Path 'a' contains an element of non-array type 'int'"),
    ({"a": "s"}, {"$bit": {"a": {"and": 1}}}, "Cannot apply $bit to a value of non-integral type"),
    ({"a": "s"}, {"$inc": {"a": 1}}, "Cannot apply $inc to a value of non-numeric type"),
    ({"a": 5}, {"$set": {"a.b": 1}}, "Cannot create field 'b' in element {a: 5}"),
    ({"a": 1}, {"$set": {"_id": 9}}, "would modify the immutable field '_id'"),
]

_PARSE_TIME = [
    ({"a": 1}, {"$pop": {"a": 5}}, "$pop expects 1 or -1, found: 5"),
    ({"a": 1}, {"$rename": {"a": "a"}}, "The source and target field for $rename must differ"),
    ({"a": 1}, {"$set": {"a": 1}, "$inc": {"a": 1}}, "would create a conflict at 'a'"),
    ({"a": 1}, {"$bit": {"a": 5}}, "The $bit modifier is not compatible with a int"),
]


@pytest.mark.parametrize(("seed", "update", "body"), _EXECUTION_TIME)
def test_execution_time_update_errors_carry_mongods_wrapper(coll, seed, update, body):
    coll.insert_one({"_id": 1, **seed})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        coll.update_one({"_id": 1}, update)
    msg = str(exc.value)
    assert msg.startswith(_WRAPPER.format(cmd="update")), msg
    assert body in msg, msg


@pytest.mark.parametrize(("seed", "update", "body"), _PARSE_TIME)
def test_parse_time_update_errors_stay_bare(coll, seed, update, body):
    coll.insert_one({"_id": 1, **seed})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        coll.update_one({"_id": 1}, update)
    msg = str(exc.value)
    assert not msg.startswith("Plan executor error"), msg
    assert body in msg, msg


@pytest.mark.parametrize(("seed", "update", "body"), _EXECUTION_TIME)
def test_find_and_modify_names_itself_in_the_wrapper(coll, seed, update, body):
    """The wrapper carries the COMMAND, so it cannot live in the engine."""
    coll.insert_one({"_id": 1, **seed})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        coll.find_one_and_update({"_id": 1}, update)
    msg = str(exc.value)
    assert msg.startswith(_WRAPPER.format(cmd="findAndModify")), msg
    assert body in msg, msg
