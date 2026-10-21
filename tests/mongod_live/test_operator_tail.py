"""Operators and commands that no CI test compared with mongod 8.2 (the
coverage review of 2026-10-21): each is evaluated on both servers over the
same documents and compared exactly, errors included."""

from __future__ import annotations

import datetime
from typing import Any

import pytest
from bson import Binary, Decimal128, Int64

from tests.mongod_live.conftest import mongo_answer, same

DOCS = [
    {
        "_id": 1,
        "s": "2026-03-04T05:06:07.089Z",
        "d": datetime.datetime(2026, 3, 4, 5, 6, 7, 89000),
        "i": 12,
        "l": Int64(10),
        "v": [1.0, 2.0, 3.0],
        "w": [3.0, 2.0, 1.0],
        "u": "0e3a9a5f-8c2b-4b3e-9b2a-4f6b7a1c2d3e",
        "o": {"a": 1, "b": 2},
        "n": Decimal128("2.5"),
    },
    {
        "_id": 2,
        "s": "not a date",
        "d": datetime.datetime(1969, 12, 31, 23, 59, 59),
        "i": -7,
        "l": Int64(3),
        "v": [0.0, 1.0],
        "w": [1.0, 0.0],
        "u": "nope",
        "o": {"a": None},
        "n": -1.25,
    },
]

#: (name, expression) -- each projected as `r` over DOCS.
EXPRESSIONS = [
    ("dateFromString", {"$dateFromString": {"dateString": "$s", "onError": "bad"}}),
    (
        "dateFromString with a format",
        {"$dateFromString": {"dateString": "04/03/2026", "format": "%d/%m/%Y"}},
    ),
    (
        "dateFromString with a timezone",
        {"$dateFromString": {"dateString": "2026-03-04 05:06", "timezone": "Asia/Kolkata"}},
    ),
    ("dateFromString refusing", {"$dateFromString": {"dateString": "$s"}}),
    ("dateFromParts", {"$dateFromParts": {"year": 2026, "month": 14, "day": -3, "hour": 25}}),
    (
        "dateFromParts iso",
        {"$dateFromParts": {"isoWeekYear": 2026, "isoWeek": 53, "isoDayOfWeek": 7}},
    ),
    ("dateToParts", {"$dateToParts": {"date": "$d"}}),
    (
        "dateToParts iso in a zone",
        {"$dateToParts": {"date": "$d", "iso8601": True, "timezone": "-0330"}},
    ),
    ("bitAnd", {"$bitAnd": ["$i", "$l"]}),
    ("bitOr", {"$bitOr": ["$i", "$l", 1]}),
    ("bitXor", {"$bitXor": ["$i", "$l"]}),
    ("bitNot", {"$bitNot": "$l"}),
    ("bitOr of a double", {"$bitOr": ["$i", 1.5]}),
    ("toUUID", {"$toUUID": "$u"}),
    # `$convert` to a target it cannot produce ("uuid" among them) diverges:
    # RM-228, tests/backlog/test_rm_found_2026_10_21.py.
    ("sigmoid", {"$sigmoid": "$n"}),
    ("sigmoid of an int", {"$sigmoid": "$i"}),
    ("similarityCosine", {"$similarityCosine": ["$v", "$v"]}),
    ("similarityDotProduct", {"$similarityDotProduct": ["$v", "$w"]}),
    ("similarityEuclidean", {"$similarityEuclidean": ["$v", "$w"]}),
    ("similarityCosine of unequal lengths", {"$similarityCosine": [[1.0, 2.0], [1.0]]}),
    ("unsetField", {"$unsetField": {"field": "a", "input": "$o"}}),
    (
        "unsetField of a dollar name",
        {"$unsetField": {"field": {"$literal": "$x"}, "input": {"$literal": {"$x": 1, "y": 2}}}},
    ),
    ("stdDevPop over an array", {"$stdDevPop": "$v"}),
    ("stdDevSamp over values", {"$stdDevSamp": ["$i", "$l", "$n"]}),
]


@pytest.fixture(scope="module")
def seeded(rust_mongo: Any, mongod: Any) -> tuple[Any, Any]:
    out = []
    for client in (rust_mongo, mongod):
        coll = client["live_tail"]["c"]
        coll.drop()
        coll.insert_many(DOCS)
        out.append(coll)
    return out[0], out[1]


@pytest.mark.parametrize(("name", "expr"), EXPRESSIONS, ids=[n for n, _ in EXPRESSIONS])
def test_expression_matches_mongod(name: str, expr: dict, seeded: tuple[Any, Any]) -> None:
    pipeline = [{"$project": {"r": expr}}, {"$sort": {"_id": 1}}]
    ours, theirs = seeded
    same(
        mongo_answer(lambda: list(ours.aggregate(pipeline))),
        mongo_answer(lambda: list(theirs.aggregate(pipeline))),
    )


BITS_DOCS = [
    {"_id": 1, "b": 0},
    {"_id": 2, "b": 5},
    {"_id": 3, "b": Int64(-1)},
    {"_id": 4, "b": 2.0},
    {"_id": 5, "b": 2.5},
    {"_id": 6, "b": Binary(b"\x05\x00")},
    {"_id": 7, "b": "x"},
]

#: Query-operator filters over BITS_DOCS.
FILTERS = [
    ("bitsAllClear mask", {"b": {"$bitsAllClear": 2}}),
    ("bitsAllClear positions", {"b": {"$bitsAllClear": [0, 2]}}),
    ("bitsAllClear binary mask", {"b": {"$bitsAllClear": Binary(b"\x01")}}),
    ("bitsAnySet", {"b": {"$bitsAnySet": [1]}}),
    ("bitsAllClear negative mask", {"b": {"$bitsAllClear": -1}}),
]


@pytest.mark.parametrize(("name", "flt"), FILTERS, ids=[n for n, _ in FILTERS])
def test_bits_filter_matches_mongod(name: str, flt: dict, rust_mongo: Any, mongod: Any) -> None:
    answers = []
    for client in (rust_mongo, mongod):
        coll = client["live_tail"]["bits"]
        coll.drop()
        coll.insert_many(BITS_DOCS)
        answers.append(mongo_answer(lambda c=coll: [d["_id"] for d in c.find(flt).sort("_id", 1)]))
    same(*answers)


#: Commands with no test in CI: lowercase aliases mongod still accepts, and
#: two whose replies are fixed.
COMMANDS = [
    ("collstats", {"collstats": "c"}, ("ok", "ns", "count")),
    ("datasize", {"datasize": "live_tail.c"}, ("ok", "size", "numObjects")),
    ("collmod", {"collmod": "c"}, ("ok",)),
    ("planCacheClearFilters", {"planCacheClearFilters": "c"}, ("ok",)),
    ("logout", {"logout": 1}, ("ok",)),
]


@pytest.mark.parametrize(("name", "cmd", "keys"), COMMANDS, ids=[n for n, _, _ in COMMANDS])
def test_command_matches_mongod(name: str, cmd: dict, keys: tuple, seeded: tuple[Any, Any]) -> None:
    """The fields a client reads from each reply (sizes vary by engine and
    are not compared, only their presence and type)."""
    answers = []
    for coll in seeded:
        answer = mongo_answer(lambda c=coll: c.database.command(cmd))
        if answer[0] == "ok":
            reply = answer[1]
            answer = ("ok", {k: type(reply.get(k)).__name__ for k in keys})
        answers.append(answer)
    same(*answers)
