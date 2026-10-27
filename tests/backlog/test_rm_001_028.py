"""Gap tests for `tasks/backlog-rust-mongodb.md`, RM-001 to RM-028.

Each compares the Rust MongoDB server with mongod 8.2.11 (the `mongod`
fixture, a standalone), using the same database and collection names on both
so that messages naming a namespace compare equal.
"""

from __future__ import annotations

import datetime
import json
import re
import time
from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import mongo_answer

_REPLY_NOISE = ("$clusterTime", "operationTime")


def _strip(reply: Any) -> Any:
    if isinstance(reply, dict):
        return {k: v for k, v in reply.items() if k not in _REPLY_NOISE}
    return reply


def _both(rust_mongo: Any, mongod: Any, dbname: str) -> list[tuple[str, Any]]:
    """A fresh database of one name on each server."""
    out = []
    for name, client in (("rust", rust_mongo), ("mongod", mongod)):
        client.drop_database(dbname)
        out.append((name, client[dbname]))
    return out


def _ixscan_names(plan: Any, out: list[str]) -> list[str]:
    if isinstance(plan, dict):
        if plan.get("stage") == "IXSCAN":
            out.append(plan["indexName"])
        for v in plan.values():
            _ixscan_names(v, out)
    elif isinstance(plan, list):
        for v in plan:
            _ixscan_names(v, out)
    return out


@pytest.mark.backlog_fixed("RM-001")
def test_or_branch_index_choice_matches_mongod(rust_mongo: Any, mongod: Any) -> None:
    """Each `$or` branch below has two or more usable indexes; the IXSCAN
    index names in the winning plan must be the ones mongod 8.2.11 picks."""
    shapes = [
        [{"a": 3, "b": 2}, {"c": 5}],
        [{"a": 3}, {"c": 5, "a": 1}],
        [{"b": 2, "a": {"$gt": 1}}, {"c": {"$lt": 5}}],
        [{"a": 3, "c": 4}, {"b": 2, "c": 1}],
        [{"a": {"$in": [1, 2]}, "b": 2}, {"c": 5, "b": 1}],
    ]
    answers: dict[str, list] = {}
    for name, db in _both(rust_mongo, mongod, "rm001"):
        db.c.insert_many([{"a": i % 10, "b": i % 7, "c": i} for i in range(300)])
        for spec in (
            [("a", 1)],
            [("a", 1), ("b", 1)],
            [("b", 1)],
            [("c", 1)],
            [("c", 1), ("a", 1)],
        ):
            db.c.create_index(spec)
        got = []
        for branches in shapes:
            ex = db.command(
                "explain", {"find": "c", "filter": {"$or": branches}}, verbosity="queryPlanner"
            )
            got.append(_ixscan_names(ex["queryPlanner"]["winningPlan"], []))
        answers[name] = got
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-199")
def test_or_beside_other_clauses_is_a_candidate_plan(rust_mongo: Any, mongod: Any) -> None:
    """An `$or` beside another clause (measured on a fresh collection,
    mongod 8.2.11): mongod tries the plan that reads each branch's index,
    the other clauses pushed down into the branches, against the plans of
    one index for the whole query, and keeps the branches' plan here."""
    shapes = [
        {"$or": [{"a": 3, "b": 2}, {"c": 5}, {"d": 1, "b": 3}], "d": {"$ne": 5}},
        {"$or": [{"a": 3}, {"b": 4}, {"c": {"$gt": 290}}], "d": {"$ne": 5}},
    ]
    answers: dict[str, list] = {}
    for name, db in _both(rust_mongo, mongod, "rm199"):
        db.c.insert_many(
            [{"_id": i, "a": i % 10, "b": i % 7, "c": i, "d": i % 3} for i in range(300)]
        )
        for spec in (
            [("a", 1)],
            [("a", 1), ("b", 1)],
            [("b", 1)],
            [("c", 1)],
            [("c", 1), ("a", 1)],
            [("d", 1), ("b", 1)],
        ):
            db.c.create_index(spec)
        answers[name] = [
            _ixscan_names(
                db.command("explain", {"find": "c", "filter": f}, verbosity="queryPlanner")[
                    "queryPlanner"
                ]["winningPlan"],
                [],
            )
            for f in shapes
        ]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-002")
@pytest.mark.parametrize("value", ["s", float("nan"), -1], ids=["string", "nan", "negative"])
def test_bucket_auto_granularity_value_errors(rust_mongo: Any, mongod: Any, value: Any) -> None:
    """mongod 8.2.11 answers 40258 (non-numeric), 40259 (NaN) and 40260
    (negative) for a groupBy value `granularity` cannot place."""
    answers = {}
    for name, db in _both(rust_mongo, mongod, "rm002"):
        db.g.insert_many([{"x": value}, {"x": 1}])
        pipeline = [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": "R5"}}]
        answers[name] = mongo_answer(lambda db=db, p=pipeline: list(db.g.aggregate(p)))
    assert answers["rust"] == answers["mongod"]


_INGRESS_PARAMS = (
    "ingressConnectionEstablishmentRateLimiterEnabled",
    "ingressConnectionEstablishmentRatePerSec",
    "ingressConnectionEstablishmentBurstCapacitySecs",
    "ingressConnectionEstablishmentMaxQueueDepth",
)


@pytest.mark.backlog_fixed("RM-008")
def test_ingress_connection_rate_limiter_parameters(rust_mongo: Any, mongod: Any) -> None:
    """`TestConnectionPoolBackpressure` sets mongod's ingress connection rate
    limiter; mongod 8.2.11 reports all four parameters through `getParameter`."""
    answers = {}
    for name, client in (("rust", rust_mongo), ("mongod", mongod)):
        answers[name] = mongo_answer(
            lambda c=client: _strip(
                c.admin.command("getParameter", 1, **dict.fromkeys(_INGRESS_PARAMS, 1))
            )
        )
    assert answers["rust"] == answers["mongod"]


def _admitted(client: Any, queue: int) -> list:
    """Six connections opened at once with the limiter at one connection a
    second, a burst of one and `queue` allowed to wait: which were answered,
    and roughly how long each waited."""
    import socket
    import struct
    import threading
    import time

    import bson

    host, port = client.address
    admin = client.admin

    def hello() -> Any:
        s = socket.create_connection((host, port), timeout=10)
        body = bson.encode({"hello": 1, "$db": "admin"})
        s.sendall(
            struct.pack("<iiii", 21 + len(body), 1, 0, 2013) + struct.pack("<I", 0) + b"\x00" + body
        )
        started = time.monotonic()
        try:
            header = s.recv(16)
            return (
                ("answered", round(time.monotonic() - started))
                if len(header) == 16
                else ("closed", 0)
            )
        except OSError:
            return ("closed", 0)
        finally:
            s.close()

    admin.command(
        "setParameter",
        1,
        ingressConnectionEstablishmentRatePerSec=1,
        ingressConnectionEstablishmentBurstCapacitySecs=1,
        ingressConnectionEstablishmentMaxQueueDepth=queue,
    )
    admin.command("setParameter", 1, ingressConnectionEstablishmentRateLimiterEnabled=True)
    try:
        time.sleep(1.2)  # a full bucket
        results: list = [None] * 6
        threads = [
            threading.Thread(target=lambda i=i: results.__setitem__(i, hello())) for i in range(6)
        ]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        return sorted(results)
    finally:
        admin.command("setParameter", 1, ingressConnectionEstablishmentRateLimiterEnabled=False)
        admin.command(
            "setParameter",
            1,
            ingressConnectionEstablishmentRatePerSec=2147483647,
            ingressConnectionEstablishmentBurstCapacitySecs=1.7976931348623157e308,
            ingressConnectionEstablishmentMaxQueueDepth=0,
        )


@pytest.mark.backlog_fixed("RM-008")
def test_ingress_rate_limiter_admits_queues_and_closes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's limiter at one a second: the token in the bucket
    admits one connection, up to `maxQueueDepth` more wait a second each,
    and the rest are closed unanswered."""
    for queue in (0, 2):
        assert _admitted(rust_mongo, queue) == _admitted(mongod, queue), queue


@pytest.mark.backlog_fixed("RM-010")
def test_geo_intersects_unsorted_order(rust_mongo: Any, mongod: Any) -> None:
    """The Java `GeoJsonFilters` fixture over a 2dsphere index, no sort:
    mongod 8.2.11 returns `[1, 2, 4, 3]`."""

    def crs(n: str) -> dict:
        return {"type": "name", "properties": {"name": n}}

    docs = [
        {
            "_id": 1,
            "geo": {
                "type": "Point",
                "crs": crs("urn:ogc:def:crs:OGC:1.3:CRS84"),
                "coordinates": [1.0, 1.0],
            },
        },
        {"_id": 2, "geo": {"type": "Point", "crs": crs("EPSG:4326"), "coordinates": [2.0, 2.0]}},
        {"_id": 3, "geo": {"type": "Point", "coordinates": [3.0, 3.0]}},
        {
            "_id": 4,
            "geo": {
                "type": "Polygon",
                "coordinates": [[[2.0, 2.0], [6.0, 2.0], [6.0, 6.0], [2.0, 6.0], [2.0, 2.0]]],
            },
        },
    ]
    poly = {
        "type": "Polygon",
        "coordinates": [[[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0], [0.0, 0.0]]],
    }
    answers = {}
    for name, db in _both(rust_mongo, mongod, "rm010"):
        db.g.create_index([("geo", "2dsphere")])
        db.g.insert_many(docs)
        answers[name] = [
            d["_id"] for d in db.g.find({"geo": {"$geoIntersects": {"$geometry": poly}}})
        ]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-016")
def test_upsert_seeds_query_fields_in_an_order_mongod_uses(rust_mongo: Any) -> None:
    """mongod 8.2.11 has NO fixed order here: `findAndModify` with
    `query: {b: 1, a: 2}, update: {$set: {z: 1}}, upsert: true, new: true`
    gave `{_id, a, b, z}` 21 times and `{_id, b, a, z}` 19 times in 40 fresh
    databases (measured 2026-10-14). So the check is that every order the
    Rust server gives -- in the reply and in the stored document -- is one
    of those two."""
    mongod_orders = {("_id", "a", "b", "z"), ("_id", "b", "a", "z")}
    seen = set()
    for i in range(10):
        dbname = f"rm016_{i}"
        rust_mongo.drop_database(dbname)
        db = rust_mongo[dbname]
        reply = db.command(
            "findAndModify",
            "u",
            query={"b": 1, "a": 2},
            update={"$set": {"z": 1}},
            upsert=True,
            new=True,
        )
        seen.add(tuple(reply["value"]))
        seen.add(tuple(db.u.find_one()))
        rust_mongo.drop_database(dbname)
    assert seen <= mongod_orders


_SLOW_LINE_ATTRS = (
    "planSummary",
    "keysExamined",
    "docsExamined",
    "nreturned",
    "reslen",
    "storage",
)


def _slow_query_attr(line: str) -> dict | None:
    """The attributes of a `Slow query` line: a JSON document with an
    `attr`, on both servers since RM-302."""
    if "Slow query" not in line:
        return None
    if line.startswith("{"):
        return json.loads(line).get("attr")
    return json.loads(line.split("Slow query ", 1)[1])


def _find_slow_lines(rust_mongo: Any, mongod: Any) -> dict:
    """Per server, the attributes of the `Slow query` line for one indexed
    `find`, read back through `getLog: "global"`."""
    lines = {}
    for name, db in _both(rust_mongo, mongod, "rm023"):
        client = db.client
        db.s.insert_many([{"x": i} for i in range(5)])
        db.s.create_index("x")
        # mongod logs every operation at -1; the Rust server refuses a
        # negative slowms and logs every operation at 0.
        slowms = -1 if name == "mongod" else 0
        db.command("profile", 0, slowms=slowms)
        try:
            list(db.s.find({"x": {"$gt": 1}}))
            log = client.admin.command("getLog", "global")["log"]
        finally:
            db.command("profile", 0, slowms=100)
        attrs = [
            attr
            for attr in (_slow_query_attr(line) for line in log)
            if attr is not None
            and attr.get("ns") == "rm023.s"
            and (
                attr.get("command") == "find"
                or isinstance(attr.get("command"), dict)
                and "find" in attr["command"]
            )
        ]
        assert attrs, f"{name}: no Slow query line for the find"
        lines[name] = attrs[-1]
    return lines


@pytest.mark.backlog_fixed("RM-023")
def test_slow_query_line_names_the_plan(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `Slow query` line for a `find` carries the command
    DOCUMENT, `planSummary` (here `IXSCAN { x: 1 }`), `nreturned` and
    `reslen`."""
    lines = _find_slow_lines(rust_mongo, mongod)
    answers = {
        name: (
            sorted(k for k in ("planSummary", "nreturned", "reslen") if k in attr),
            isinstance(attr.get("command"), dict),
            attr.get("planSummary"),
            attr.get("nreturned"),
        )
        for name, attr in lines.items()
    }
    assert answers["mongod"] == (["nreturned", "planSummary", "reslen"], True, "IXSCAN { x: 1 }", 3)
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-355")
def test_slow_query_line_says_where_the_time_went(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `Slow query` line for a `find` carries
    `keysExamined`, `docsExamined` and `storage`, as well as what
    `test_slow_query_line_names_the_plan` checks.

    Since RM-023 (2026-10-27) this server's line has the counts, and a
    `storage` section whenever WiredTiger's statistics for the operation's
    sessions count something. mongod's always has one, for its execution
    admission control's `storageExecutionMicros` (RM-355), which this
    server does not have: a `find` of documents in the cache has no
    `storage` here. The test is RM-355's now."""
    lines = _find_slow_lines(rust_mongo, mongod)
    answers = {
        name: sorted(k for k in _SLOW_LINE_ATTRS if k in attr) for name, attr in lines.items()
    }
    assert answers["rust"] == answers["mongod"]


_BATCH_ATTRS = (
    "planSummary",
    "cursorid",
    "keysExamined",
    "docsExamined",
    "nBatches",
    "cursorExhausted",
    "nreturned",
)


_FORWARD_READS = [
    lambda c: c.find({"x": {"$gte": 2}}).batch_size(3),
    lambda c: c.find({"x": {"$lt": 4}}).batch_size(2),
    lambda c: c.find({"y": 1}).batch_size(2),
    lambda c: c.find({"y": 1, "z": {"$gte": 0}}).batch_size(1),
    lambda c: c.find({"x": {"$gt": 3}, "z": 0}).batch_size(2),
    lambda c: c.find({"x": {"$gte": 0}}).skip(2).limit(3).batch_size(2),
    lambda c: c.find({"x": {"$in": [1, 5, 9, 99]}}).batch_size(2),
    lambda c: c.find({"x": {"$gte": 9}}, {"x": 1}).batch_size(2),
]

# Backward scans, the `_id` index, a backward collection scan, covered plans.
_MORE_READS = [
    lambda c: c.find({"x": {"$gte": 2}}).sort("x", -1).batch_size(3),
    lambda c: c.find().sort("_id", 1).batch_size(4),
    lambda c: c.find({"_id": {"$gte": 3}}).batch_size(2),
    lambda c: c.find({"_id": {"$in": [1, 4, 7, 99]}}).sort("_id", -1).batch_size(1),
    lambda c: c.find().sort("$natural", -1).batch_size(5),
    lambda c: c.find({"z": 1}).hint([("$natural", -1)]).batch_size(2),
    lambda c: c.find({"x": {"$lt": 9}}).sort("x", -1).skip(1).limit(4).batch_size(2),
    lambda c: c.find({"y": {"$in": [0, 2]}}).sort([("y", -1), ("z", -1)]).batch_size(3),
    # Covered: answered from the keys, no document examined.
    lambda c: c.find({"x": {"$gte": 3}}, {"_id": 0, "x": 1}).batch_size(2),
    lambda c: c.find({"x": {"$lt": 8}}, {"_id": 0, "x": 1}).sort("x", -1).skip(1).batch_size(3),
    lambda c: c.find({"y": 2}, {"_id": 0, "y": 1, "z": 1}).limit(3).batch_size(1),
]


def _batch_counts(name: str, db: Any, reads: list = _FORWARD_READS) -> list:
    """For a few `find`s read in small batches, each `find` and `getMore`
    line's counts, in the order the line has them (`cursorid` as present or
    not, since its value is the server's own). Each read has a collection of
    its own, so its lines are told apart by namespace: two of the profiler's
    own lines can be identical to the millisecond, which defeats a window
    that starts after the last line before the read."""
    client = db.client
    out = []
    for i, read in enumerate(reads):
        coll = db[f"s{i}"]
        coll.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])
        coll.create_index("x")
        coll.create_index([("y", 1), ("z", 1)])
        db.command("profile", 0, slowms=-1 if name == "mongod" else 0)
        try:
            list(read(coll))
        finally:
            db.command("profile", 0, slowms=100)
        log = client.admin.command("getLog", "global")["log"]
        lines = []
        for attr in (_slow_query_attr(line) for line in log):
            if attr is None or attr.get("ns") != coll.full_name:
                continue
            first = next(iter(attr.get("command", {})), "")
            if first not in ("find", "getMore"):
                continue
            lines.append(
                (
                    first,
                    [(k, "id" if k == "cursorid" else attr[k]) for k in attr if k in _BATCH_ATTRS],
                    "originatingCommand" in attr,
                )
            )
        out.append(lines)
    return out


@pytest.mark.backlog_fixed("RM-023")
def test_slow_query_lines_count_each_batch(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts the keys and documents each batch examined, with
    no lookahead: an index scan of 10 read 3 at a time examines 3, 3, 3,
    then 1 on the `getMore` that finds the end (`cursorExhausted: true`).
    Every `getMore` line repeats the cursor's `planSummary` and names the
    command that opened it."""
    got = {name: _batch_counts(name, db) for name, db in _both(rust_mongo, mongod, "rm023b")}
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-023")
def test_slow_query_lines_count_each_batch_of_backward_and_id_scans(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 counts each batch as it reads it on a backward index
    scan, an `_id` index scan and a backward collection scan too, and a
    covered plan examines no document."""
    got = {
        name: _batch_counts(name, db, _MORE_READS)
        for name, db in _both(rust_mongo, mongod, "rm023c")
    }
    assert got["rust"] == got["mongod"]


_PROFILE_ATTRS = (
    "op",
    "originatingCommand",
    "cursorid",
    "keysExamined",
    "docsExamined",
    "nBatches",
    "cursorExhausted",
    "nreturned",
    "planSummary",
)


def _profile_counts(db: Any) -> list:
    """Each `query` / `getmore` profile entry of three reads: its fields of
    `_PROFILE_ATTRS` in the entry's order, with the values that do not name
    a cursor."""
    db.s.insert_many([{"_id": i, "x": i} for i in range(6)])
    db.s.create_index("x")
    db.command("profile", 2)
    try:
        list(db.s.find({"x": {"$gte": 1}}).batch_size(2))
        list(db.s.find({"x": 99}))
        list(db.s.find({}).batch_size(4))
    finally:
        db.command("profile", 0)
    return [
        [
            (k, "-" if k in ("cursorid", "originatingCommand") else e[k])
            for k in e
            if k in _PROFILE_ATTRS
        ]
        for e in db.system.profile.find({"op": {"$in": ["query", "getmore"]}})
    ]


@pytest.mark.backlog_fixed("RM-023")
def test_profile_entries_count_each_batch(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's profile entry for a `find` or `getMore` carries the
    keys and documents its batch examined, the cursor's plan, and on a
    `getMore` the command that opened the cursor."""
    got = {name: _profile_counts(db) for name, db in _both(rust_mongo, mongod, "rm023p")}
    assert got["rust"] == got["mongod"]


def _log_shape(client: Any) -> list:
    """The shape of each `getLog` line: whether it is JSON, and its fields."""
    client.admin.command("ping")
    out = []
    for line in client.admin.command("getLog", "global")["log"][-5:]:
        try:
            entry = json.loads(line)
        except ValueError:
            out.append(("not JSON", line.split(" ", 3)[:3]))
            continue
        out.append(("JSON", sorted(k for k in entry if k in ("t", "s", "c", "id", "ctx", "msg"))))
    return out


def _log_kinds(client: Any) -> dict:
    """What `getLog` says about one find, an update of two statements, an
    index build and a connection opened and closed: for each kind of line --
    `(s, c, id, msg)` -- the threads that wrote it (digits as `N`), whether
    every one is padded as mongod's formatter pads, and its attribute names."""
    db = client.rm302
    db.drop_collection("c")
    db.c.insert_many([{"_id": i} for i in range(3)])
    # Only this case's lines: the window starts after the last line before
    # it, since mongod's `getLog` keeps its latest 1023 only.
    log = client.admin.command("getLog", "global")["log"]
    last = log[-1] if log else None
    client.admin.command("profile", 0, slowms=0)
    try:
        db.c.find_one({"_id": 1})
        db.command(
            "update",
            "c",
            updates=[{"q": {"_id": i}, "u": {"$set": {"a": 1}}} for i in (1, 2)],
        )
        db.c.create_index("x")
        # mongod writes the build's line on its build thread, which can run
        # after `createIndexes` has answered: wait for it (a bounded wait)
        # while every operation is still logged, or it is never written.
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            window = client.admin.command("getLog", "global")["log"]
            if last in window:
                window = window[len(window) - window[::-1].index(last) :]
            if any('"c":"STORAGE"' in line and "Slow query" in line for line in window):
                break
            time.sleep(0.05)
    finally:
        client.admin.command("profile", 0, slowms=100)
    host, port = client.address
    other = pymongo.MongoClient(host=host, port=port)
    other.admin.command("ping")
    other.close()
    log = client.admin.command("getLog", "global")["log"]
    if last in log:
        log = log[len(log) - log[::-1].index(last) :]
    kinds: dict = {}
    for line in log:
        e = json.loads(line)
        key = (e["s"], e["c"], e["id"], e["msg"])
        if key not in _LINE_KINDS:
            continue
        head = re.sub(r'^\{"t":\{"\$date":"[^"]+"\},', "", line).split('"ctx"')[0]
        padded = head == (
            f'"s":"{e["s"]}",'.ljust(10)
            + f'"c":"{e["c"]}",'.ljust(16)
            + f'"id":{e["id"]},'.ljust(14)
        )
        got = kinds.setdefault(key, (set(), set(), set()))
        got[0].add(re.sub(r"\d+", "N", e["ctx"]))
        got[1].add(padded)
        if key[3] != "Slow query":
            got[2].update(e.get("attr", {}))
    return kinds


_LINE_KINDS = {
    ("I", "NETWORK", 22943, "Connection accepted"),
    ("I", "NETWORK", 22944, "Connection ended"),
    ("I", "COMMAND", 51803, "Slow query"),
    ("I", "WRITE", 51803, "Slow query"),
    ("I", "STORAGE", 51803, "Slow query"),
}

#: The reference mongod runs `--quiet`, which leaves out its connection lines;
#: a default mongod 8.2.11 writes these (measured 2026-10-24).
_CONNECTION = {"connectionCount", "connectionId", "isLoadBalanced", "remote", "uuid"}
_CONNECTION_KINDS = {
    ("I", "NETWORK", 22943, "Connection accepted"): ({"listener"}, {True}, _CONNECTION),
    ("I", "NETWORK", 22944, "Connection ended"): ({"connN"}, {True}, _CONNECTION),
}


@pytest.mark.backlog_fixed("RM-302")
def test_get_log_lines_are_structured_json(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `getLog` lines are its structured log, one JSON
    document each with `t`, `s`, `c`, `id`, `ctx` and `msg`, padded into
    columns, with mongod's id for each message and the thread that wrote it
    (`listener`, `connN`, `IndexBuildsCoordinatorMongod-0`); a connection's
    lines name its peer, a UUID, its id and how many are open (measured
    2026-10-24). This server's were text: `<date> <severity> <component>
    <message>`."""
    want = [("JSON", ["c", "ctx", "id", "msg", "s", "t"])] * 5
    assert _log_shape(mongod) == want
    assert _log_shape(rust_mongo) == want
    ours, theirs = _log_kinds(rust_mongo), _log_kinds(mongod)
    connection = {k: ours.pop(k, None) for k in _CONNECTION_KINDS}
    assert connection == _CONNECTION_KINDS
    assert ours == {k: v for k, v in theirs.items() if k not in _CONNECTION_KINDS}


def _client_metadata(client: Any) -> list:
    """The `client metadata` lines a new connection's first `hello` writes."""
    host, port = client.address
    other = pymongo.MongoClient(host=host, port=port, appname="rm-client-metadata")
    try:
        other.admin.command("ping")
    finally:
        other.close()
    out = []
    for line in client.admin.command("getLog", "global")["log"]:
        e = json.loads(line)
        if e["msg"] == "client metadata" and "rm-client-metadata" in line:
            out.append((e["s"], e["c"], e["id"], sorted(e["attr"])))
    return out[-1:]


@pytest.mark.backlog("RM-305")
def test_client_metadata_is_logged(rust_mongo: Any) -> None:
    """mongod 8.2.11 logs a connection's first `hello` as `client metadata`
    (NETWORK, 51800) with the driver's document (measured 2026-10-24 on a
    default mongod; the reference runs `--quiet`, which leaves the line
    out). This server writes no such line."""
    want = [("I", "NETWORK", 51800, ["client", "doc", "negotiatedCompressors", "remote"])]
    assert _client_metadata(rust_mongo) == want


@pytest.mark.backlog_fixed("RM-025")
def test_fcv_80_refuses_a_feature_82_added(rust_mongo: Any) -> None:
    """mongod 8.2.11 at FCV 8.0 refuses `$sigmoid` (added in 8.2) with 224
    QueryFeatureNotAllowed."""
    expected = (
        "err",
        224,
        "Invalid $project :: caused by :: $sigmoid is not allowed in the current feature "
        "compatibility version. See "
        "https://docs.mongodb.com/master/release-notes/8.0-compatibility/"
        "#feature-compatibility for more information.",
    )
    rust_mongo.drop_database("rm025")
    db = rust_mongo.rm025
    db.s.insert_one({"x": 1})
    rust_mongo.admin.command("setFeatureCompatibilityVersion", "8.0", confirm=True)
    try:
        got = mongo_answer(
            lambda: list(db.s.aggregate([{"$project": {"_id": 0, "r": {"$sigmoid": 1}}}]))
        )
    finally:
        rust_mongo.admin.command("setFeatureCompatibilityVersion", "8.2", confirm=True)
    assert got == expected


@pytest.mark.backlog_fixed("RM-026")
def test_dbhash_includes_timeseries_buckets(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `dbHash` lists `system.buckets.<name>` for a
    time-series collection."""
    answers = {}
    for name, db in _both(rust_mongo, mongod, "rm026"):
        db.create_collection("ts", timeseries={"timeField": "t"})
        db.ts.insert_one({"t": datetime.datetime(2026, 1, 1), "v": 1})
        answers[name] = sorted(db.command("dbHash")["collections"])
    assert answers["rust"] == answers["mongod"]


# A `--noscripting` mongod 8.2.11's answer to `mapReduce` with a named `out`
# (measured 2026-10-20): the refusal this server gives by Joe's decision.
NO_SCRIPTING_MAP_REDUCE = (
    "err",
    31264,
    "Executor error during MapReduce command on namespace: rm027.k :: caused by :: "
    "Cannot run server-side javascript without the javascript engine enabled",
)


@pytest.mark.backlog_fixed("RM-027")
def test_commands_mongod_runs(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs `compact` (with `force: true`, which a replica-set
    primary needs too) and `setParameter notablescan`. `mapReduce` with a
    named `out` is refused as a mongod with no JavaScript engine refuses it
    (Joe, 2026-10-16: no JavaScript engine). (`reIndex` is left out: mongod
    refuses it on a replica-set member, which is what the Rust server
    reports itself as.)"""
    answers = {}
    for name, db in _both(rust_mongo, mongod, "rm027"):
        db.k.insert_one({"x": 1})
        client = db.client
        answers[name] = [
            mongo_answer(lambda db=db: db.command("compact", "k", force=True)["ok"]),
            mongo_answer(
                lambda c=client: _strip(c.admin.command("setParameter", 1, notablescan=False))
            ),
        ]
    assert answers["rust"] == answers["mongod"]
    db = rust_mongo["rm027"]
    got = mongo_answer(
        lambda: db.command(
            "mapReduce",
            "k",
            map="function() { emit(this.x, 1); }",
            reduce="function(k, v) { return Array.sum(v); }",
            out="mrout",
        )
    )
    assert got == NO_SCRIPTING_MAP_REDUCE
