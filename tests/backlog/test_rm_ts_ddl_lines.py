"""What a `Slow query` line and a profile entry name around an index build,
a failed `collMod` and a time-series collection's buckets, against mongod
8.2.11 (found while working RM-484, 2026-11-13)."""

from __future__ import annotations

import contextlib
import datetime
from collections.abc import Callable
from typing import Any

import pytest
from pymongo.errors import PyMongoError

from tests.backlog.opdebug import logged, seed

#: The index builds' lines are this file's subject.
_KEEP = frozenset({"createIndexes"})


def _setup(db: Any) -> None:
    seed(db)
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    db.ts.insert_many([{"t": datetime.datetime(2020, 1, 1), "m": i} for i in range(10)])


def _both(rust_mongo: Any, mongod_rs: Any, op: Callable[[Any], Any]) -> dict:
    return {
        server: logged(client, is_mongod, _setup, op, keep=_KEEP)
        for server, client, is_mongod in (
            ("rust", rust_mongo, False),
            ("mongod", mongod_rs, True),
        )
    }


def _first(attr: dict) -> str:
    return next(iter(attr.get("command", {})), "")


def _ns(attr: dict) -> str:
    return str(attr.get("ns", "")).split(".", 1)[1]


def _named(rust_mongo: Any, mongod_rs: Any, op: Callable[[Any], Any]) -> dict:
    """`(command, ns, collectionType, ok)` of each line `op` wrote, and
    `(command, ns, ok)` of each profile entry."""
    return {
        server: {
            "lines": [
                (_first(a), _ns(a), a.get("collectionType"), a.get("ok", 1)) for _, a in lines
            ],
            "entries": [(_first(e), _ns(e), e.get("ok", 1)) for e in entries],
        }
        for server, (lines, entries) in _both(rust_mongo, mongod_rs, op).items()
    }


@pytest.mark.backlog("RM-582")
def test_a_create_indexes_that_builds_nothing_names_cmd(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `createIndexes` of an index that is there
    already -- `{x: 1}` on a collection, a time-series collection's own
    `{m: 1, t: 1}` -- names `<db>.$cmd`; this server's names the
    collection."""

    def op(db: Any) -> None:
        db.c.create_index([("x", 1)])
        db.ts.create_index([("m", 1), ("t", 1)])

    got = _named(rust_mongo, mongod_rs, op)
    assert got["mongod"]["lines"] == [
        ("createIndexes", "$cmd", "normal", 1),
        ("createIndexes", "$cmd", "normal", 1),
    ]
    assert got["rust"]["lines"] == got["mongod"]["lines"]


@pytest.mark.backlog("RM-583")
def test_a_time_series_index_build_has_its_line(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 writes two lines for a `createIndexes` that builds an
    index of a time-series collection, the command's and its build
    thread's, each naming `<db>.system.buckets.ts`; this server writes the
    command's alone (it keeps a time-series collection's documents in its
    column store, so the build finds the collection empty)."""
    got = _named(rust_mongo, mongod_rs, lambda d: d.ts.create_index("m"))
    assert got["mongod"]["lines"] == [
        ("createIndexes", "system.buckets.ts", "timeseriesBuckets", 1),
        ("createIndexes", "system.buckets.ts", "timeseriesBuckets", 1),
    ]
    assert got["rust"]["lines"] == got["mongod"]["lines"]


@pytest.mark.backlog("RM-584")
def test_a_coll_mod_of_a_missing_collection_names_cmd(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `collMod` of a collection that does not
    exist (26, `NamespaceNotFound`) names `<db>.$cmd`, and it writes no
    profile entry; this server's line names the collection, and it
    profiles it."""

    def op(db: Any) -> None:
        with contextlib.suppress(PyMongoError):
            db.command("collMod", "nots", expireAfterSeconds=100)

    got = _named(rust_mongo, mongod_rs, op)
    assert got["mongod"] == {"lines": [("collMod", "$cmd", "normal", 0)], "entries": []}
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-585")
def test_an_insert_into_buckets_is_profiled(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 profiles an `insert` into `system.buckets.ts`; this
    server writes its line and no profile entry."""
    got = _named(
        rust_mongo,
        mongod_rs,
        lambda d: d["system.buckets.ts"].insert_one({"control": {"version": 1}}),
    )
    assert got["mongod"]["entries"] == [("insert", "system.buckets.ts", 1)]
    assert got["rust"]["entries"] == got["mongod"]["entries"]


#: The fields of a read's line that say what it examined and returned.
_EXAMINED = ("planSummary", "keysExamined", "docsExamined", "nBatches", "nreturned")


@pytest.mark.backlog("RM-586")
def test_a_find_of_buckets_says_what_it_examined(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `find` of `<db>.system.buckets.ts` says
    what it examined, as one of a collection's does (`keysExamined: 0`,
    `docsExamined: 10`, `nBatches: 1` for ten buckets); this server's says
    `COLLSCAN` and `nreturned` and leaves the rest out."""
    got = {
        server: [{k: a.get(k) for k in _EXAMINED} for _, a in lines]
        for server, (lines, _) in _both(
            rust_mongo, mongod_rs, lambda d: list(d["system.buckets.ts"].find())
        ).items()
    }
    assert got["mongod"] == [
        {
            "planSummary": "COLLSCAN",
            "keysExamined": 0,
            "docsExamined": 10,
            "nBatches": 1,
            "nreturned": 10,
        }
    ]
    assert got["rust"] == got["mongod"]
