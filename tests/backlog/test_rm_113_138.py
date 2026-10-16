"""Gap tests for `tasks/backlog-rust-mongodb.md` items RM-113 to RM-138.

Most of that range is performance, CI and tooling (no gap test); these are
the items that name a behaviour. Each compares the Rust MongoDB server with
mongod 8.2.11 (the `mongod` fixture) or with an answer measured on it.
"""

from __future__ import annotations

import struct
from typing import Any

import pytest
from bson import ObjectId
from bson.raw_bson import RawBSONDocument

from tests.backlog.conftest import mongo_answer


def _both(rust_mongo: Any, mongod: Any, name: str) -> list[Any]:
    """The same fresh collection on both servers."""
    colls = [c["rm_113_138"][name] for c in (rust_mongo, mongod)]
    for c in colls:
        c.drop()
    return colls


def _dbpointer_doc(key: str) -> RawBSONDocument:
    """A raw BSON document `{key: DBPointer("db.c", <oid>)}` (type 0x0C, which
    no driver can build any other way)."""
    ns = b"db.c\x00"
    oid = ObjectId("5f0000000000000000000001").binary
    elem = b"\x0c" + key.encode() + b"\x00" + struct.pack("<i", len(ns)) + ns + oid
    return RawBSONDocument(struct.pack("<i", 4 + len(elem) + 1) + elem + b"\x00")


@pytest.mark.backlog("RM-120")
def test_regex_over_1000_characters(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 compiles a 1001-character pattern and matches it."""
    s = "a" * 1001
    answers = []
    for c in _both(rust_mongo, mongod, "rm120"):
        c.insert_one({"_id": 1, "x": s})
        answers.append(mongo_answer(lambda c=c: list(c.find({"x": {"$regex": s}}))))
    assert answers[0] == answers[1]


@pytest.mark.backlog_fixed("RM-123")
def test_push_sort_over_non_sortable_elements(rust_mongo: Any, mongod: Any) -> None:
    """mongod sorts `[true, 1]` under `$push ... $sort: 1` (numbers before bools)."""
    answers = []
    for c in _both(rust_mongo, mongod, "rm123"):
        c.insert_one({"_id": 1, "a": []})
        upd = mongo_answer(
            lambda c=c: (
                c.update_one(
                    {"_id": 1}, {"$push": {"a": {"$each": [True, 1], "$sort": 1}}}
                ).modified_count
            )
        )
        answers.append((upd, c.find_one({"_id": 1})))
    assert answers[0] == answers[1]


@pytest.mark.backlog("RM-124")
def test_to_hashed_index_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 evaluates `$toHashedIndexKey`. (Its `$group` `$bitAnd` /
    `$bitOr` / `$bitXor` accumulators are still `15952 unknown group operator`
    on 8.2.11, so only this half of the item can be checked.)"""
    pipeline = [
        {"$documents": [{"v": 1}, {"v": "abc"}]},
        {"$project": {"_id": 0, "h": {"$toHashedIndexKey": "$v"}}},
    ]
    answers = [
        mongo_answer(lambda c=c: list(c["rm_113_138"].aggregate(pipeline)))
        for c in (rust_mongo, mongod)
    ]
    assert answers[0] == answers[1]


@pytest.mark.backlog_fixed("RM-125")
def test_missing_sort_key_versus_explicit_null(rust_mongo: Any, mongod: Any) -> None:
    """Whether a missing key ties with `null`: a `_id` descending tie-break
    shows it (a tie interleaves the two groups)."""
    answers = []
    for c in _both(rust_mongo, mongod, "rm125"):
        c.insert_many([{"_id": 1, "a": None}, {"_id": 2}, {"_id": 3, "a": None}, {"_id": 4}])
        answers.append(
            mongo_answer(lambda c=c: [d["_id"] for d in c.find().sort([("a", 1), ("_id", -1)])])
        )
    assert answers[0] == answers[1]


@pytest.mark.backlog_fixed("RM-126")
@pytest.mark.parametrize(
    "spec",
    [
        {"year": 1, "month": "x"},
        {"year": 0},
        {"year": 1, "timezone": 5},
        {"year": 1, "month": 40000},
    ],
    ids=["non-numeric-month", "year-out-of-range", "timezone-not-string", "month-overflow"],
)
def test_date_from_parts_error_codes(rust_mongo: Any, mongod: Any, spec: dict) -> None:
    """mongod 8.2.11: 40515, 40523, 40517 and 31034 respectively."""
    pipeline = [{"$documents": [{}]}, {"$project": {"_id": 0, "d": {"$dateFromParts": spec}}}]
    answers = [
        mongo_answer(lambda c=c: list(c["rm_113_138"].aggregate(pipeline)))[:2]
        for c in (rust_mongo, mongod)
    ]
    assert answers[0] == answers[1]


@pytest.mark.backlog_fixed("RM-127")
def test_dbpointer_in_range_operand(rust_mongo: Any, mongod: Any) -> None:
    """A DBPointer nested in an array `$lt` operand: mongod 8.2.11 matches `_id` 1 and 3."""
    answers = []
    for c in _both(rust_mongo, mongod, "rm127a"):
        c.insert_many([{"_id": 1, "x": [1]}, {"_id": 2, "x": "s"}, {"_id": 3, "x": [{"a": 1}]}])
        flt = {"x": {"$lt": [_dbpointer_doc("a")]}}
        answers.append(mongo_answer(lambda c=c, f=flt: [d["_id"] for d in c.find(f)]))
    assert answers[0] == answers[1]


@pytest.mark.backlog("RM-127")
def test_dbpointer_min_update_operand(rust_mongo: Any, mongod: Any) -> None:
    """`$min` with a DBPointer operand: mongod compares it and updates."""
    answers = []
    for c in _both(rust_mongo, mongod, "rm127b"):
        c.insert_one({"_id": 1, "a": 5})
        upd = {"$min": _dbpointer_doc("a")}
        res = mongo_answer(lambda c=c, u=upd: c.update_one({"_id": 1}, u).modified_count)
        answers.append((res, c.find_one({"_id": 1})))
    assert answers[0] == answers[1]


@pytest.mark.backlog("RM-128")
def test_std_dev_pop_last_ulp(rust_mongo: Any) -> None:
    """mongod 8.2.11 over [99, -4, 7.338]: `$stdDevPop` 46.11516078495468 and
    `$stdDevSamp` 56.47930666477178 (measured 2026-10-14)."""
    pipeline = [
        {"$documents": [{"v": 99}, {"v": -4}, {"v": 7.338}]},
        {"$group": {"_id": None, "s": {"$stdDevPop": "$v"}, "t": {"$stdDevSamp": "$v"}}},
    ]
    got = mongo_answer(lambda: list(rust_mongo["rm_113_138"].aggregate(pipeline)))
    assert got == ("ok", [{"_id": None, "s": 46.11516078495468, "t": 56.47930666477178}])
