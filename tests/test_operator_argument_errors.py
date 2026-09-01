"""What the server says about a bad ARGUMENT to a query or update operator.

Every code and message here was measured against mongod 8.2.11 on 2026-09-01 by
`tools/probes/operator_error_surface.py`, which crosses every operator with a
fixed set of pathological arguments -- 2,226 shapes, of which 583 disagreed when
it was first run. A hand-picked sample of 32 had found only 12 of those, and had
missed `$bits*` entirely; `$bits*` turned out to be returning the wrong
DOCUMENTS.

The tests below are the ones where we answered wrongly rather than merely
phrasing an error differently. Wording is covered by the probe.
"""

from __future__ import annotations

import pytest
from bson import Binary, Code, Decimal128
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def coll(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            yield client["testdb"]["things"]
        finally:
            client.close()


# --- $bits*: the value types it accepts -------------------------------------
# It took a plain int and nothing else, so a double, a Decimal128, a BinData and
# every element of an array field were silently skipped.
BITS_DOCS = [
    {"_id": 1, "v": 0},
    {"_id": 2, "v": 1},
    {"_id": 3, "v": 5},
    {"_id": 4, "v": [1, 4]},
    {"_id": 5, "v": 1.0},
    {"_id": 6, "v": Binary(b"\x05")},
    {"_id": 7, "v": Decimal128("5")},
    {"_id": 8, "v": "x"},
    {"_id": 9, "v": 5.5},
]


@pytest.mark.parametrize(
    "mask,expected",
    [
        # bit 0: 1, 5, [1, 4] via its first element, 1.0, BinData 0x05, Dec 5.
        (1, [2, 3, 4, 5, 6, 7]),
        # bits 0 and 2: 5, BinData 0x05, Dec 5.
        (5, [3, 6, 7]),
        # A BinData mask is little-endian, LSB-first per byte.
        (Binary(b"\x05"), [3, 6, 7]),
        # An empty mask requires nothing, so every bit-eligible value matches.
        (Binary(b""), [1, 2, 3, 4, 5, 6, 7]),
        ([], [1, 2, 3, 4, 5, 6, 7]),
        # A bit-position array is the same set as the equivalent integer mask.
        ([0, 2], [3, 6, 7]),
    ],
)
def test_bits_all_set_value_types(coll, mask, expected):
    coll.insert_many(BITS_DOCS)
    assert sorted(d["_id"] for d in coll.find({"v": {"$bitsAllSet": mask}}, {"_id": 1})) == expected


def test_bits_skips_non_eligible_values(coll):
    # A string and a fractional double are not bit sources at all.
    coll.insert_many(BITS_DOCS)
    matched = {d["_id"] for d in coll.find({"v": {"$bitsAnyClear": 255}}, {"_id": 1})}
    assert 8 not in matched and 9 not in matched


def test_bits_negative_values_are_twos_complement(coll):
    # -1 has every bit set; -2 has bit 0 clear and the rest set, with infinite
    # sign extension (so bit 63 too).
    coll.insert_many([{"_id": 1, "v": -1}, {"_id": 2, "v": -2}])
    assert [d["_id"] for d in coll.find({"v": {"$bitsAllSet": [0, 63]}})] == [1]
    assert [d["_id"] for d in coll.find({"v": {"$bitsAllClear": [0]}})] == [2]


# --- arguments that used to CRASH ------------------------------------------
@pytest.mark.parametrize(
    "query,code,message",
    [
        # Compiled as a BYTES regex, then raised TypeError from search().
        ({"v": {"$regex": Binary(b"z")}}, 2, "$regex has to be a string"),
        # bson.Code subclasses str AND is unhashable, so it reached a set test.
        (
            {"v": {"$type": Code("x=1")}},
            14,
            "type must be represented as a number or a string",
        ),
    ],
)
def test_arguments_that_used_to_be_internal_errors(coll, query, code, message):
    coll.insert_one({"_id": 1, "v": "abc"})
    with pytest.raises(OperationFailure) as exc:
        list(coll.find(query))
    assert exc.value.code == code
    assert message in str(exc.value)


# --- arguments that used to be silently ACCEPTED ---------------------------
def test_not_requires_operator_keys(coll):
    # `{v: {$not: {a: 1}}}` degraded to "not equal to {a: 1}" and MATCHED.
    coll.insert_one({"_id": 1, "v": 5})
    with pytest.raises(OperationFailure) as exc:
        list(coll.find({"v": {"$not": {"a": 1}}}))
    assert exc.value.code == 2 and "unknown operator: a" in str(exc.value)


def test_rename_to_a_code_target_is_refused(coll):
    # Code subclasses str, so the isinstance check passed and the rename ran.
    coll.insert_one({"_id": 1, "n": 5})
    with pytest.raises(OperationFailure) as exc:
        coll.update_one({"_id": 1}, {"$rename": {"n": Code("x=1")}})
    assert exc.value.code == 2
    assert "The 'to' field for $rename must be a string" in str(exc.value)
    assert coll.find_one({"_id": 1}) == {"_id": 1, "n": 5}


def test_type_rejects_an_empty_alias_list(coll):
    coll.insert_one({"_id": 1, "v": 5})
    with pytest.raises(OperationFailure) as exc:
        list(coll.find({"v": {"$type": []}}))
    assert exc.value.code == 9 and "must match at least one type" in str(exc.value)


def test_currentdate_names_the_unrecognized_key(coll):
    # Reported before the $type value is looked at, so a valid $type alongside
    # a stray key still names the stray key.
    coll.insert_one({"_id": 1, "n": 5})
    with pytest.raises(OperationFailure) as exc:
        coll.update_one({"_id": 1}, {"$currentDate": {"n": {"$type": "date", "a": 1}}})
    assert exc.value.code == 2 and "Unrecognized $currentDate option: a" in str(exc.value)


# --- a whole Decimal128 is a valid numeric argument -------------------------
@pytest.mark.parametrize(
    "update,ok",
    [({"$pop": {"arr": Decimal128("1")}}, True), ({"$pop": {"arr": Decimal128("1.5")}}, False)],
)
def test_pop_accepts_a_whole_decimal(coll, update, ok):
    coll.insert_one({"_id": 1, "arr": [1, 2, 3]})
    if ok:
        coll.update_one({"_id": 1}, update)
        assert coll.find_one({"_id": 1})["arr"] == [1, 2]
    else:
        with pytest.raises(OperationFailure) as exc:
            coll.update_one({"_id": 1}, update)
        assert "Cannot represent as a 64-bit integer" in str(exc.value)


def test_size_accepts_a_whole_decimal(coll):
    coll.insert_one({"_id": 1, "arr": [1, 2]})
    assert [d["_id"] for d in coll.find({"arr": {"$size": Decimal128("2")}})] == [1]


# --- the shared BSON value renderer ----------------------------------------
@pytest.mark.parametrize(
    "argument,rendered",
    [
        ([1], "[ 1 ]"),
        ({"a": 1}, "{ a: 1 }"),
        ("x", '"x"'),
        (True, "true"),
        (None, "null"),
        (Binary(b"z"), "BinData(0, 7A)"),
        (Code("x=1"), "x=1"),
    ],
)
def test_error_messages_render_values_mongods_way(coll, argument, rendered):
    # Not Python's repr: `[ 1 ]` not `[1]`, `{ a: 1 }` not `{'a': 1}`.
    coll.insert_one({"_id": 1, "n": 5})
    with pytest.raises(OperationFailure) as exc:
        coll.update_one({"_id": 1}, {"$inc": {"n": argument}})
    assert f"Cannot increment with non-numeric argument: {{n: {rendered}}}" in str(exc.value)
