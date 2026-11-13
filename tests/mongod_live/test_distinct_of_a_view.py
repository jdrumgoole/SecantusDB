"""A `distinct` of a view runs as the aggregation mongod builds for it, and
`$_internalFindAllValuesAtPath`, the expression that aggregation reads its
key with, against mongod 8.2 (RM-493, measured 8.2.11, 2026-11-13).

mongod 8.2 runs a `distinct` of a view as `[{$match: query},
{$replaceRoot: {newRoot: {_internalUnwoundArray:
{$_internalFindAllValuesAtPath: key}}}}, {$unwind: ...}, {$group: {_id:
null, distinct: {$addToSet: ...}}}]` over the view (its `explain` prints
it), so its line counts what that aggregation's plan examined and says its
cursor is exhausted. The expression is one a client may use too.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest
from bson import Decimal128
from pymongo.errors import OperationFailure

from tests.backlog.opdebug import fresh, logged, seed

pytestmark = pytest.mark.mongod_live

_DOCS = [
    {"_id": 1, "a": {"b": [1, 2, [3, 4], {"c": 1}]}, "x": 1},
    {"_id": 2, "a": [{"b": 1}, {"b": [5, 1.0]}, {"b": None}], "x": 2},
    {"_id": 3, "a": [[{"b": 9}]], "x": 3},
    {"_id": 4, "a": {"b": []}, "x": 4},
    {"_id": 5, "x": 5},
    {"_id": 6, "a": {"b": Decimal128("1.0")}, "x": 6},
    {"_id": 7, "a": [{"b": "x"}, {"b": "X"}], "x": 7},
]


def _answer(op: Callable[[], Any]) -> Any:
    try:
        return ("ok", op())
    except OperationFailure as e:
        return ("error", e.code, str(e.details.get("errmsg")) if e.details else str(e))


def _sorted(values: list[Any]) -> list[str]:
    # mongod's `$addToSet` answers in its own order: compare the values.
    return sorted(repr(v) for v in values)


_KEYS = ["a.b", "a", "a.b.c", "a.0.b", "a.0", "x", "nothere"]


@pytest.mark.parametrize("key", _KEYS)
def test_a_distinct_of_a_view_answers_as_mongods(rust_mongo: Any, mongod_rs: Any, key: str) -> None:
    """The values a view's `distinct` gives, with and without a query and
    under a case-insensitive collation."""
    got = {}
    for server, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        db = client[fresh("dv")]
        db.c.insert_many(_DOCS)
        db.command("create", "v", viewOn="c", pipeline=[{"$match": {"x": {"$gte": 1}}}])
        db.command(
            "create",
            "ci",
            viewOn="c",
            pipeline=[],
            collation={"locale": "en", "strength": 2},
        )
        got[server] = [
            _sorted(db.v.distinct(key)),
            _sorted(db.v.distinct(key, {"x": {"$lt": 4}})),
            _sorted(db.ci.distinct(key)),
        ]
    assert got["rust"] == got["mongod"]


_REFUSED: dict[str, Callable[[Any], Any]] = {
    "an empty key": lambda d: d.v.distinct(""),
    "a key with an empty name": lambda d: d.v.distinct("a..b"),
    "a key naming a $ field": lambda d: d.v.distinct("a.$b"),
    "a key ending in a dot": lambda d: d.v.distinct("a."),
    "a hint naming no index": lambda d: d.v.distinct("a", hint="nosuch"),
    "a query that fails as it runs": lambda d: d.v.distinct(
        "a", {"$expr": {"$divide": [1, {"$subtract": ["$x", "$x"]}]}}
    ),
}


@pytest.mark.parametrize("case", list(_REFUSED))
def test_a_distinct_of_a_view_refuses_as_mongod(rust_mongo: Any, mongod_rs: Any, case: str) -> None:
    got = {}
    for server, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        db = client[fresh("dv")]
        seed(db)
        db.command("create", "v", viewOn="c", pipeline=[{"$match": {"x": {"$gte": 1}}}])
        answer = _answer(lambda db=db: _REFUSED[case](db))
        got[server] = answer[:2] if answer[0] == "error" else answer
    assert got["mongod"][0] == "error"
    assert got["rust"] == got["mongod"]


def _view(pipeline: list[dict], on: str = "c") -> Callable[[Any], None]:
    def setup(db: Any) -> None:
        seed(db)
        db.command("create", "v", viewOn=on, pipeline=pipeline)

    return setup


_VIEWS: dict[str, Callable[[Any], None]] = {
    "a view bounding the index": _view([{"$match": {"x": {"$gte": 1}}}]),
    "a view that scans": _view([{"$project": {"y": 1}}]),
    "a view that sorts and limits by the index": _view([{"$sort": {"x": -1}}, {"$limit": 5}]),
    "a view of no collection": _view([{"$match": {"x": {"$gte": 1}}}], on="nothere"),
}

_QUERIES: dict[str, dict] = {
    "no query": {},
    "a query bounding the index": {"x": {"$lt": 6}},
    "a query on another field": {"z": 1},
}


@pytest.mark.parametrize("query", list(_QUERIES))
@pytest.mark.parametrize("view", list(_VIEWS))
def test_a_distinct_of_a_view_counts_what_it_examined(
    rust_mongo: Any, mongod_rs: Any, view: str, query: str
) -> None:
    """Its line and its profile entry count what the aggregation's plan
    examined, say its cursor is exhausted, and say no sort stage where the
    index gives the order."""
    fields = ("planSummary", "keysExamined", "docsExamined", "cursorExhausted", "hasSortStage")
    got = {}
    for server, client, is_mongod in (("rust", rust_mongo, False), ("mongod", mongod_rs, True)):
        lines, entries = logged(
            client, is_mongod, _VIEWS[view], lambda d: d.v.distinct("y", _QUERIES[query])
        )
        got[server] = (
            [
                tuple(attr.get(f) for f in fields)
                for _, attr in lines
                if next(iter(attr.get("command", {})), "") == "distinct"
            ],
            [
                tuple(e.get(f) for f in fields)
                for e in entries
                if "distinct" in e.get("command", {})
            ],
        )
    assert got["mongod"][0] and got["mongod"][0][0][3] is True
    assert got["rust"] == got["mongod"]


_ARGS: dict[str, Any] = {
    "a path": "a.b",
    "a path in a list": ["a.b"],
    "a literal path in a list": [{"$literal": "a.b"}],
    "a literal path": {"$literal": "a.b"},
    "a positional path": "a.0",
    "two arguments": ["a.b", "c"],
    "a number": 1,
    "null": None,
    "a field path": "$a.b",
    "an expression": {"$concat": ["a", ".b"]},
    "an empty path": "",
    "a path with an empty name": "a..b",
    "a path naming a $ field": "a.$b",
}


@pytest.mark.parametrize("arg", list(_ARGS))
def test_find_all_values_at_path_parses_as_mongods(
    rust_mongo: Any, mongod_rs: Any, arg: str
) -> None:
    """`$_internalFindAllValuesAtPath` in a `$project`, as a client may write
    it: its values, or the parse error that refuses it."""
    got = {}
    for server, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        db = client[fresh("fa")]
        db.c.insert_many(_DOCS)
        got[server] = _answer(
            lambda db=db: list(
                db.c.aggregate(
                    [
                        {"$sort": {"_id": 1}},
                        {"$project": {"f": {"$_internalFindAllValuesAtPath": _ARGS[arg]}}},
                    ]
                )
            )
        )
    assert got["rust"] == got["mongod"]


def test_find_all_values_at_path_inside_an_expression(rust_mongo: Any, mongod_rs: Any) -> None:
    got = {}
    for server, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        db = client[fresh("fa")]
        db.c.insert_many(_DOCS)
        got[server] = list(
            db.c.aggregate(
                [
                    {"$sort": {"_id": 1}},
                    {"$project": {"n": {"$size": {"$_internalFindAllValuesAtPath": "a.b"}}}},
                ]
            )
        )
    assert got["rust"] == got["mongod"]
