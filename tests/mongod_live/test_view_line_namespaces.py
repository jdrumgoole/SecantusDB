"""What a `Slow query` line and a profile entry name around a view or a
time-series collection, against mongod 8.2 (RM-380, measured 8.2.11,
2026-11-10).

mongod names a namespace's kind as its operation acquired it: `view` or
`timeseries` only on the line of a read of one that succeeded, `normal` for
a write, a `getMore`, DDL and any read that failed. A read of a view runs on
the collection beneath it, and when it fails there its line and entry name
that collection. An `explain` of a read names what it explains.
"""

from __future__ import annotations

import contextlib
import datetime
from collections.abc import Callable
from typing import Any

import pytest
from pymongo.errors import PyMongoError

from tests.backlog.opdebug import logged, seed

pytestmark = pytest.mark.mongod_live

#: An expression that fails only once a document reaches it.
_FAILS = {"$expr": {"$divide": [1, {"$subtract": ["$x", "$x"]}]}}


def _setup(db: Any) -> None:
    seed(db)
    db.command("create", "v", viewOn="c", pipeline=[{"$match": {"x": {"$gte": 1}}}])
    db.command("create", "w", viewOn="v", pipeline=[])
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    db.ts.insert_many([{"t": datetime.datetime(2020, 1, 1), "m": i} for i in range(10)])


def _refused(op: Callable[[Any], Any]) -> Callable[[Any], None]:
    def run(db: Any) -> None:
        with contextlib.suppress(PyMongoError):
            op(db)

    return run


_OPS: dict[str, Callable[[Any], Any]] = {
    "explain of a find of a collection": lambda d: d.command(
        "explain", {"find": "c", "filter": {}}
    ),
    "explain of an aggregate of a view": lambda d: d.command(
        "explain", {"aggregate": "v", "pipeline": [], "cursor": {}}
    ),
    "explain of a count of a view": lambda d: d.command("explain", {"count": "v"}),
    "explain of a find of a time-series collection": lambda d: d.command(
        "explain", {"find": "ts", "filter": {}}
    ),
    "explain of an update": lambda d: d.command(
        "explain", {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}}]}
    ),
    "explain of a find of a view that fails": _refused(
        lambda d: d.command("explain", {"find": "v", "filter": _FAILS}, verbosity="executionStats")
    ),
    "find of a nested view that fails": _refused(lambda d: list(d.w.find(_FAILS))),
    "aggregate of a view that fails": _refused(lambda d: list(d.v.aggregate([{"$match": _FAILS}]))),
    "count of a view that fails": _refused(lambda d: d.command("count", "v", query=_FAILS)),
    "distinct of a view that fails": _refused(lambda d: d.v.distinct("x", _FAILS)),
    "find of a time-series collection that fails": _refused(
        lambda d: list(d.ts.find({"$expr": {"$divide": [1, {"$subtract": ["$m", "$m"]}]}}))
    ),
    "find of a view that does not parse": _refused(lambda d: list(d.v.find({"x": {"$bogus": 1}}))),
    "getMore of an aggregate of a view": lambda d: list(d.v.aggregate([], batchSize=4)),
    "insert into a view": _refused(lambda d: d.v.insert_one({"a": 1})),
    "insert into a time-series collection": lambda d: d.ts.insert_one(
        {"t": datetime.datetime(2020, 1, 2), "m": 1}
    ),
    "update of a time-series collection": lambda d: d.ts.update_many({"m": 1}, {"$set": {"m": 2}}),
    "findAndModify remove of a view": _refused(lambda d: d.v.find_one_and_delete({})),
    "drop of a view": lambda d: d.drop_collection("v"),
    "collMod of a view": lambda d: d.command("collMod", "v", viewOn="c", pipeline=[]),
    "listIndexes of a time-series collection": lambda d: list(d.ts.list_indexes()),
    "distinct of a view": lambda d: d.v.distinct("x"),
    "distinct of a time-series collection": lambda d: d.ts.distinct("m"),
}


#: Cases whose profile entries are left out of the comparison, each an
#: open item with a gap test of its own: their lines are still compared.
_ENTRY_GAPS = {
    # mongod profiles a drop of a view; this server writes no entry.
    "drop of a view": "RM-384",
    # mongod profiles no explain of a write; this server profiles one.
    "explain of an update": "RM-486",
}


def _named(lines: list[tuple[str, dict]], entries: list[dict]) -> dict[str, list]:
    """What each line and entry names: its command, its namespace without
    the database, its kind (a line's), whether it failed, and whether it
    counted a batch."""

    def ns(attr: dict) -> str:
        return str(attr.get("ns", "")).split(".", 1)[1]

    def first(attr: dict) -> str:
        return next(iter(attr.get("command", {})), "")

    return {
        "lines": [
            (first(a), ns(a), a.get("collectionType"), a.get("ok", 1), a.get("nBatches"))
            for _, a in lines
        ],
        "entries": [(first(e), ns(e), e.get("ok", 1), e.get("nBatches")) for e in entries],
    }


@pytest.mark.parametrize("case", list(_OPS))
def test_a_line_names_what_mongods_does(case: str, rust_mongo: Any, mongod_rs: Any) -> None:
    got = {
        server: _named(*logged(client, is_mongod, _setup, _OPS[case]))
        for server, client, is_mongod in (
            ("rust", rust_mongo, False),
            ("mongod", mongod_rs, True),
        )
    }
    assert got["mongod"]["lines"], "mongod wrote no line: the case measures nothing"
    if case in _ENTRY_GAPS:
        for answer in got.values():
            answer.pop("entries")
    assert got["rust"] == got["mongod"]
