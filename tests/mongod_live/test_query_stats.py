"""The query stats store against mongod 8.2 (RM-215): what `$queryStats`
lists once `internalQueryStatsRateLimit` lets a query be recorded.

Each case runs one command on both servers and compares the entries it made
or added to: the key (the query shape with debug type strings, and the
command's other components in mongod's order), `keyHash` (the SHA-256 of
the key with representative literals) and `queryShapeHash`. The cases are
`test_query_shape_hash.py`'s, which sweep the shapes, and the key's own
components. Both clients connect directly, so both send mongod's
`$readPreference` for a single server.

The servers record from the module's start, so this module keeps its own.
"""

from __future__ import annotations

import datetime
from typing import Any

import bson
import pymongo
import pytest
from bson import Int64
from bson.son import SON

from tests.mongod_live.conftest import canonical, same
from tests.mongod_live.test_query_shape_hash import (
    DISTINCTS,
    EXPRESSIONS,
    FILTERS,
    FIND_OPTIONS,
    GEO_FILTERS,
    PIPELINES,
    STAGES,
)

_DB = "qstats_live"


def _cases() -> list[tuple[str, SON]]:
    out: list[tuple[str, SON]] = []
    for f in FILTERS:
        out.append((f"filter {f}", SON([("find", "c"), ("filter", f)])))
    for o in FIND_OPTIONS:
        command = SON([("find", "c"), ("filter", {"x": {"$gt": 1}})])
        command.update(o)
        out.append((f"find options {o}", command))
    for p in PIPELINES + STAGES:
        out.append((f"pipeline {p}", SON([("aggregate", "c"), ("pipeline", p), ("cursor", {})])))
    for e in EXPRESSIONS:
        pipeline = [{"$project": {"r": e}}]
        out.append(
            (f"expression {e}", SON([("aggregate", "c"), ("pipeline", pipeline), ("cursor", {})]))
        )
    for f in GEO_FILTERS:
        out.append((f"geo {f}", SON([("find", "geo"), ("filter", f)])))
    for key, query in DISTINCTS:
        command = SON([("distinct", "c"), ("key", key)])
        if query is not None:
            command["query"] = query
        out.append((f"distinct {key} {query}", command))
    # The key's own components.
    extra = [
        SON([("count", "c")]),
        SON([("count", "c"), ("query", {"x": {"$gt": 1}}), ("limit", 3), ("skip", 1)]),
        SON(
            [
                ("count", "c"),
                ("query", {"x": 1}),
                ("collation", {"locale": "fr"}),
                ("hint", {"x": 1}),
            ]
        ),
        SON([("find", "c"), ("filter", {"x": 1}), ("comment", "why"), ("maxTimeMS", 5000)]),
        SON([("find", "c"), ("filter", {"x": 1}), ("comment", {"tag": 1})]),
        SON([("find", "c"), ("filter", {"x": 1}), ("hint", {"x": 1}), ("batchSize", 2)]),
        SON([("find", "c"), ("filter", {"x": 1}), ("hint", "x_1")]),
        SON(
            [
                ("find", "c"),
                ("filter", {"x": 1}),
                ("allowPartialResults", False),
                ("noCursorTimeout", True),
            ]
        ),
        SON([("find", "c"), ("filter", {"x": 1}), ("readConcern", {"level": "local"})]),
        SON([("find", "c"), ("filter", {"x": 1}), ("readConcern", {"level": "majority"})]),
        SON([("find", "c"), ("filter", {"x": 1}), ("apiVersion", "1"), ("apiStrict", False)]),
        SON(
            [("find", "c"), ("filter", {"x": 1}), ("limit", 2), ("skip", 1), ("singleBatch", True)]
        ),
        SON(
            [
                ("find", "c"),
                ("filter", {"x": 1}),
                ("sort", {"x": -1}),
                ("min", {"x": 0}),
                ("max", {"x": 9}),
                ("hint", {"x": 1}),
            ]
        ),
        SON(
            [
                ("find", "c"),
                ("filter", {"x": 1}),
                ("let", {"v": 1}),
                ("collation", {"locale": "fr"}),
            ]
        ),
        SON([("find", "c"), ("filter", {"x": 1}), ("returnKey", True), ("showRecordId", True)]),
        SON([("find", "nothere"), ("filter", {"x": 1})]),
        SON([("find", "v"), ("filter", {"x": 1})]),
        SON(
            [
                ("aggregate", "c"),
                ("pipeline", [{"$match": {"x": 1}}]),
                ("cursor", {"batchSize": 2}),
                ("comment", "c"),
                ("maxTimeMS", 5000),
                ("hint", {"x": 1}),
            ]
        ),
        SON(
            [
                ("aggregate", "c"),
                (
                    "pipeline",
                    [{"$lookup": {"from": "o", "localField": "x", "foreignField": "k", "as": "j"}}],
                ),
                ("cursor", {}),
            ]
        ),
        SON(
            [
                ("aggregate", "c"),
                ("pipeline", [{"$unionWith": {"coll": "o", "pipeline": [{"$match": {"k": 1}}]}}]),
                ("cursor", {}),
            ]
        ),
        SON(
            [
                ("aggregate", "c"),
                ("pipeline", [{"$match": {"x": 1}}]),
                ("cursor", {}),
                ("bypassDocumentValidation", True),
                ("allowDiskUse", True),
                ("let", {"v": "s"}),
            ]
        ),
        SON(
            [
                ("aggregate", 1),
                ("pipeline", [{"$documents": [{"x": 1}]}, {"$match": {"x": 1}}]),
                ("cursor", {}),
            ]
        ),
        SON([("aggregate", "v"), ("pipeline", [{"$match": {"x": 1}}]), ("cursor", {})]),
        SON(
            [
                ("distinct", "c"),
                ("key", "x"),
                ("query", {"x": {"$lt": 4}}),
                ("comment", "c"),
                ("hint", {"x": 1}),
            ]
        ),
    ]
    out += [(f"key {c}", c) for c in extra]
    return [(name, c) for name, c in out if not any(x in name for x in _NOT_RUN)]


# Cases of the corpus that cannot run here, so record nothing to compare --
# what is lost is each one's recorded key, which `test_query_shape_hash.py`
# still checks the hash of:
_NOT_RUN = {
    # Needs a JavaScript engine, which this server does not ship (scope
    # decision, Joe 2026-10-16): it refuses the pipeline (31264).
    "$accumulator",
    # Fails here where mongod runs it: RM-533, whose gap test covers it.
    "{'$isoWeek': ['$t']}",
}


CASES = _cases()


def _clients(rust_mongo: Any, mongod_rs: Any) -> list[pymongo.MongoClient]:
    out = []
    for client in (rust_mongo, mongod_rs):
        host, port = client.address
        out.append(pymongo.MongoClient(host, port, directConnection=True))
    return out


def _setup(client: pymongo.MongoClient) -> None:
    db = client[_DB]
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "a": [1, 2]} for n in range(6)])
    db.c.create_index("x")
    db.o.insert_one({"k": 1})
    db.geo.insert_one({"loc": [0, 0], "g": {"type": "Point", "coordinates": [0, 0]}})
    db.geo.create_index([("loc", "2d")])
    db.geo.create_index([("g", "2dsphere")])
    db.create_collection("v", viewOn="c", pipeline=[{"$match": {"x": {"$gt": 0}}}])


def _ours(entry: dict) -> bool:
    """An entry a case made: not `$queryStats`' own, nor one of the
    reads mongod makes of its internal collections now and then."""
    return entry["key"]["queryShape"]["cmdNs"]["db"] == _DB


def _recorded(client: pymongo.MongoClient, since: datetime.datetime) -> list[dict]:
    return [
        e
        for e in client.admin.aggregate(
            [{"$queryStats": {}}, {"$match": {"metrics.latestSeenTimestamp": {"$gte": since}}}]
        )
        if _ours(e)
    ]


def _run(client: pymongo.MongoClient, command: SON) -> Any:
    try:
        reply = client[_DB].command(command)
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code)
    except bson.errors.InvalidBSON:
        # A reply pymongo cannot decode, as both servers send for
        # `$toUUID` of a number (a four-byte UUID): the command ran.
        return "undecodable"
    # Read a cursor to its end, so its entry is written.
    cursor = reply.get("cursor") or {}
    while cursor.get("id"):
        reply = client[_DB].command(
            "getMore", cursor["id"], collection=cursor["ns"].split(".", 1)[1]
        )
        cursor = reply["cursor"]
    return "ok"


@pytest.fixture(scope="module")
def recorded(rust_mongo: Any, mongod_rs: Any) -> Any:  # noqa: F811
    """For each case, the entries it made or added to on each server, with
    whether it succeeded."""
    clients = _clients(rust_mongo, mongod_rs)
    try:
        for client in clients:
            _setup(client)
            client.admin.command("setParameter", 1, internalQueryStatsRateLimit=-1)
        seen: list[dict[str, int]] = [{}, {}]
        out: dict[str, Any] = {}
        for name, command in CASES:
            got = []
            for at, client in enumerate(clients):
                since = client.admin.command("hello")["localTime"] - datetime.timedelta(seconds=1)
                outcome = _run(client, command)
                changed = []
                for e in _recorded(client, since):
                    n = e["metrics"]["execCount"]
                    if seen[at].get(e["keyHash"]) != n:
                        seen[at][e["keyHash"]] = n
                        changed.append(
                            canonical(
                                SON(
                                    [
                                        ("key", e["key"]),
                                        ("keyHash", e["keyHash"]),
                                        ("queryShapeHash", e["queryShapeHash"]),
                                    ]
                                )
                            )
                        )
                got.append((outcome, sorted(changed)))
            out[name] = got
        yield clients, out
    finally:
        for client in clients:
            client.admin.command("setParameter", 1, internalQueryStatsRateLimit=0)
            client.drop_database(_DB)
            client.close()


@pytest.mark.parametrize("name", [n for n, _ in CASES])
def test_a_query_is_recorded_under_mongods_key(recorded: Any, name: str) -> None:
    _, out = recorded
    ours, theirs = out[name]
    assert theirs[0] != "ok" or theirs[1], f"mongod recorded nothing for {name}"
    same(ours, theirs)


def _entry(client: pymongo.MongoClient, coll: str) -> dict:
    entries = [
        e
        for e in client.admin.aggregate([{"$queryStats": {}}])
        if e["key"]["queryShape"]["cmdNs"] == {"db": _DB, "coll": coll}
    ]
    assert len(entries) == 1, entries
    return entries[0]


def _types(value: Any) -> Any:
    if isinstance(value, dict):
        return [(k, _types(v)) for k, v in value.items()]
    return type(value).__name__


def test_a_cursor_is_recorded_when_it_closes(recorded: Any) -> None:
    """A cursor's query is recorded when the cursor closes, with what its
    `getMore`s added: the first batch's time is `firstResponseExecMicros`;
    a cursor left open is not recorded yet, and one killed is, with what it
    returned. The metrics are mongod's, field for field and type for type."""
    clients, _ = recorded
    answers = []
    for client in clients:
        db = client[_DB]
        db.m.insert_many([{"i": i} for i in range(7)])
        list(db.m.find({"i": {"$gte": 0}}, batch_size=2))
        killed = db.m.find({"i": {"$lt": 100}}, batch_size=3)
        next(killed)
        killed.close()
        held = db.m.find({"i": {"$ne": -1}}, batch_size=2)
        next(held)
        entries = [
            e
            for e in client.admin.aggregate([{"$queryStats": {}}])
            if e["key"]["queryShape"]["cmdNs"] == {"db": _DB, "coll": "m"}
        ]
        held.close()
        by_filter = {canonical(e["key"]["queryShape"]["filter"]): e["metrics"] for e in entries}
        counts = {
            f: (m["execCount"], m["docsReturned"]["sum"], m["docsExamined"]["sum"])
            for f, m in by_filter.items()
        }
        timing = [
            m["firstResponseExecMicros"]["sum"] <= m["totalExecMicros"]["sum"]
            for m in by_filter.values()
        ]
        answers.append((sorted(counts.items()), timing, [_types(m) for m in by_filter.values()]))
    same(answers[0], answers[1])


def test_a_failed_command_is_not_recorded(recorded: Any) -> None:
    """mongod records a command only once it has succeeded: a parse error
    and a runtime error leave no entry."""
    clients, _ = recorded
    answers = []
    for client in clients:
        db = client[_DB]
        db.f.insert_many([{"a": i} for i in range(3)])
        outcomes = [
            _run(client, SON([("find", "f"), ("filter", {"$bad": 1})])),
            _run(
                client,
                SON(
                    [
                        ("aggregate", "f"),
                        ("pipeline", [{"$project": {"q": {"$divide": [1, "$a"]}}}]),
                        ("cursor", {}),
                    ]
                ),
            ),
            _run(client, SON([("count", "f"), ("query", {"$expr": {"$divide": [1, "$a"]}})])),
        ]
        entries = [
            e
            for e in client.admin.aggregate([{"$queryStats": {}}])
            if e["key"]["queryShape"]["cmdNs"] == {"db": _DB, "coll": "f"}
        ]
        answers.append((outcomes, len(entries)))
    same(answers[0], answers[1])


def test_the_planner_is_counted(recorded: Any) -> None:
    """`fromMultiPlanner` and `fromPlanCache` count how each run chose its
    plan: a trial among two indexes, then the plan it cached."""
    clients, _ = recorded
    answers = []
    for client in clients:
        db = client[_DB]
        db.p.insert_many([{"a": i % 50, "b": i % 7} for i in range(500)])
        db.p.create_index("a")
        db.p.create_index("b")
        for _ in range(4):
            list(db.p.find({"a": 3, "b": 3}))
        m = _entry(client, "p")["metrics"]
        answers.append((m["execCount"], m["fromMultiPlanner"], m["fromPlanCache"]))
    same(answers[0], answers[1])


@pytest.mark.parametrize(
    "value",
    [5, -1, -2, Int64(7), 1.5, "3", True, None, 2**31],
    ids=str,
)
def test_the_rate_limit_parameter(recorded: Any, value: Any) -> None:
    """`internalQueryStatsRateLimit` takes a whole number of at least -1,
    as mongod coerces one."""
    clients, _ = recorded
    answers = []
    for client in clients:
        try:
            was = client.admin.command("setParameter", 1, internalQueryStatsRateLimit=value)["was"]
            now = client.admin.command("getParameter", 1, internalQueryStatsRateLimit=1)
            answers.append((was, now["internalQueryStatsRateLimit"]))
        except pymongo.errors.OperationFailure as e:
            answers.append(("err", e.code, str(e.details.get("errmsg"))))
        finally:
            client.admin.command("setParameter", 1, internalQueryStatsRateLimit=-1)
    same(answers[0], answers[1])


def test_a_change_stream_is_recorded_at_each_operation(recorded: Any) -> None:
    """A change stream never exhausts: mongod records it at its `aggregate`
    and again at each `getMore`."""
    clients, _ = recorded
    answers = []
    for client in clients:
        db = client[_DB]
        stream = db.cs.watch([{"$match": {"operationType": "insert"}}])
        db.cs.insert_one({"x": 1})
        stream.try_next()
        stream.try_next()
        entries = [
            e
            for e in client.admin.aggregate([{"$queryStats": {}}])
            if e["key"]["queryShape"]["cmdNs"] == {"db": _DB, "coll": "cs"}
        ]
        stream.close()
        answers.append(
            [(e["key"]["collectionType"], e["metrics"]["execCount"] >= 2) for e in entries]
        )
    same(answers[0], answers[1])
