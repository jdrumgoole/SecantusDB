"""Ingress admission control against a live mongod 8.2 (RM-355).

mongod admits every command through its ingress admission controller
before it runs: a pool of tickets (`ingressAdmissionControllerTicketPoolSize`,
1,000,000 by default) that the commands it subjects to the control take at
normal priority, waiting in a queue when none is free, while every other
command is admitted exempt. `serverStatus.queues.ingress` reports the pool,
and an operation's `Slow query` line its admissions and time queued. Each
test runs the same thing on both servers and compares the answers; the
servers are this module's own, so emptying the pool reaches no other test.

Execution control's `queues.execution` is `test_execution_admission.py`'s;
what it adds to the line (its `execution` admissions and
`storage.timeWaitingMicros.storageExecutionMicros`) is RM-355's remainder,
left out here.
"""

from __future__ import annotations

import json
import threading
import time
from collections.abc import Callable, Iterator
from typing import Any

import pymongo
import pytest

from tests.backlog.opdebug import fresh, logged, seed

from .conftest import same

_POOL = "ingressAdmissionControllerTicketPoolSize"
_DEPTH = "ingressAdmissionControllerMaxQueueDepth"
_ENABLED = "ingressAdmissionControlEnabled"


@pytest.fixture
def servers(rust_mongo: Any, mongod_rs: Any) -> Iterator[dict[str, Any]]:
    """Both servers, their ingress parameters put back afterwards."""
    both = {"rust": rust_mongo, "mongod": mongod_rs}
    yield both
    for client in both.values():
        client.admin.command(
            {"setParameter": 1, _POOL: 1_000_000, _DEPTH: 1_000_000, _ENABLED: True}
        )


def _ingress(client: Any) -> dict:
    return client.admin.command("serverStatus")["queues"]["ingress"]


def _kinds(value: Any) -> Any:
    """`value` with every number replaced by its BSON type's name."""
    if isinstance(value, dict):
        return {k: _kinds(v) for k, v in value.items()}
    return type(value).__name__


def _reply(call: Callable[[], Any]) -> dict:
    """What a command answered: its error's code, name, message and labels."""
    try:
        call()
    except pymongo.errors.OperationFailure as e:
        d = e.details or {}
        return {k: d.get(k) for k in ("code", "codeName", "errmsg", "errorLabels")}
    return {"ok": 1}


def test_the_parameters_have_mongods_defaults_and_bounds(servers: dict) -> None:
    """The three parameters, their defaults and types, and the refusal of a
    negative pool (measured 8.2.11, 2026-11-08)."""
    got = {}
    for name, client in servers.items():
        params = client.admin.command({"getParameter": 1, _POOL: 1, _DEPTH: 1, _ENABLED: 1})
        got[name] = (
            {k: params[k] for k in (_ENABLED, _DEPTH, _POOL)},
            _reply(lambda c=client: c.admin.command("setParameter", 1, **{_POOL: -1})),
            client.admin.command("setParameter", 1, **{_POOL: 0})["was"],
            client.admin.command("setParameter", 1, **{_POOL: 1_000_000})["was"],
        )
    same(got["rust"], got["mongod"])
    assert got["mongod"][0] == {_ENABLED: True, _DEPTH: 1_000_000, _POOL: 1_000_000}


def test_server_status_reports_the_pool(servers: dict) -> None:
    """`queues.ingress`: the pool's tickets as ints and each priority's
    statistics as longs (`totalDelinquentAcquisitions` an int), in mongod's
    order; and nothing while the control is off."""
    shapes = {}
    for name, client in servers.items():
        on = _kinds(_ingress(client))
        client.admin.command("setParameter", 1, **{_ENABLED: False})
        off = "ingress" in client.admin.command("serverStatus")["queues"]
        client.admin.command("setParameter", 1, **{_ENABLED: True})
        shapes[name] = (list(on), on["normalPriority"], on["exempt"], on["out"], off)
    same(shapes["rust"], shapes["mongod"])


#: Commands and how many normal-priority admissions each takes: mongod's
#: `isSubjectToIngressAdmissionControl` (a `$currentOp` pipeline, `ping`,
#: `dbStats` and `buildInfo` are exempt).
_COMMANDS: dict[str, Callable[[Any], Any]] = {
    "find": lambda d: list(d.c.find({"x": {"$gt": 1}})),
    "find in batches": lambda d: list(d.c.find().batch_size(5)),
    "insert": lambda d: d.c.insert_one({"x": 100}),
    "update": lambda d: d.c.update_one({"x": 1}, {"$set": {"y": 1}}),
    "delete": lambda d: d.c.delete_one({"x": 2}),
    "findAndModify": lambda d: d.c.find_one_and_update({"x": 3}, {"$set": {"y": 2}}),
    "count": lambda d: d.command("count", "c"),
    "distinct": lambda d: d.c.distinct("x"),
    "aggregate": lambda d: list(d.c.aggregate([{"$match": {}}])),
    "aggregate of $currentOp": lambda d: list(
        d.client.admin.aggregate([{"$currentOp": {}}, {"$limit": 1}])
    ),
    "listIndexes": lambda d: list(d.c.list_indexes()),
    "listCollections": lambda d: d.list_collection_names(),
    "createIndexes": lambda d: d.c.create_index("y"),
    "explain": lambda d: d.command("explain", {"find": "c"}),
    "dbStats": lambda d: d.command("dbStats"),
    "ping": lambda d: d.command("ping"),
    "buildInfo": lambda d: d.command("buildInfo"),
}


def test_the_commands_mongod_subjects_take_a_normal_ticket(servers: dict) -> None:
    """Each command's count of normal-priority admissions, read from the
    pool's `normalPriority.startedProcessing` and `newAdmissions` before
    and after it. (Exempt admissions are not compared: a client's monitor
    is admitted exempt whenever it checks the server.)"""
    got: dict[str, dict] = {}
    for name, client in servers.items():
        db = client[fresh("ingress")]
        seed(db)
        counts = {}
        for label, op in _COMMANDS.items():
            before = _ingress(client)["normalPriority"]
            op(db)
            after = _ingress(client)["normalPriority"]
            counts[label] = [
                after[k] - before[k] for k in ("startedProcessing", "newAdmissions", "canceled")
            ]
        got[name] = counts
    same(got["rust"], got["mongod"])


def test_a_line_says_what_ingress_admitted(rust_mongo: Any, mongod_rs: Any) -> None:
    """Every line's `queues.ingress`: one admission, for an exempt command
    and an update's statement as for a read."""
    ops = {
        "find": lambda d: list(d.c.find({"x": {"$gt": 1}}).batch_size(2)),
        "ping": lambda d: d.command("ping"),
        "update": lambda d: d.c.update_one({"x": 1}, {"$set": {"y": 1}}),
        "insert": lambda d: d.c.insert_one({"x": 100}),
    }
    got = {}
    for name, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        lines = {}
        for label, op in ops.items():
            found, _ = logged(client, name == "mongod", seed, op)
            lines[label] = [
                (next(iter(attr.get("command", {})), ""), attr.get("queues", {}).get("ingress"))
                for _, attr in found
            ]
        got[name] = lines
    same(got["rust"], got["mongod"])
    assert got["mongod"]["ping"] == [("ping", {"admissions": 1})]


def _queued(client: Any, label: str, results: dict, **extra: Any) -> threading.Thread:
    """A `find` on a connection of its own, which waits for a ticket."""
    other = pymongo.MongoClient(*client.address, directConnection=True)
    other.admin.command("ping")

    def run() -> None:
        try:
            results[label] = _reply(lambda: other.ingress.command("find", "c", comment=label))
        finally:
            other.close()

    thread = threading.Thread(target=run)
    thread.start()
    return thread


def _wait_queued(client: Any, n: int) -> None:
    deadline = time.monotonic() + 10
    while _ingress(client)["normalPriority"]["queueLength"] < n:
        assert time.monotonic() < deadline, "the find did not queue"
        time.sleep(0.01)


def test_an_empty_pool_queues_the_commands_it_subjects(servers: dict) -> None:
    """With no tickets: an exempt command still runs; a subject one queues,
    and ends when its `maxTimeMS` is spent (50, noticed when the wait next
    wakes), when it is killed (11601), when the queue is already as deep as
    allowed (433, labelled `SystemOverloadedError`), or when the pool grows
    and admits it, its line saying how long it queued."""
    got = {}
    for name, client in servers.items():
        client.ingress.c.insert_one({"x": 1})
        client.admin.command("setParameter", 1, **{_POOL: 0})
        out: dict[str, Any] = {"ping": _reply(lambda c=client: c.ingress.command("ping"))}
        started = time.monotonic()
        out["maxTimeMS"] = _reply(lambda c=client: c.ingress.command("find", "c", maxTimeMS=100))
        out["waited for maxTimeMS"] = time.monotonic() - started >= 0.1
        results: dict = {}
        killed = _queued(client, "killed", results)
        _wait_queued(client, 1)
        client.admin.command("setParameter", 1, **{_DEPTH: 1})
        out["overflow"] = _reply(lambda c=client: c.ingress.command("find", "c"))
        client.admin.command("setParameter", 1, **{_DEPTH: 1_000_000})
        rows = client.admin.command("currentOp", {"command.comment": "killed"})["inprog"]
        for row in rows:
            client.admin.command("killOp", op=row["opid"])
        killed.join(30)
        out["killed"] = results["killed"]
        admitted = _queued(client, "admitted", results)
        _wait_queued(client, 1)
        stats = _ingress(client)
        out["while queued"] = (stats["out"], stats["available"], stats["totalTickets"])
        client.admin.command("setParameter", 1, **{_POOL: 1_000_000})
        admitted.join(30)
        out["admitted"] = results["admitted"]
        normal = _ingress(client)["normalPriority"]
        out["queue"] = (normal["queueLength"], normal["canceled"] >= 3)
        got[name] = out
    same(got["rust"], got["mongod"])
    assert got["mongod"]["overflow"]["code"] == 433
    assert got["mongod"]["killed"]["code"] == 11601


def test_a_queued_commands_line_says_how_long_it_waited(servers: dict) -> None:
    """A command admitted after waiting has `totalTimeQueuedMicros` beside
    its admission; one that never got a ticket has only the time."""
    got = {}
    for name, client in servers.items():
        db = client[fresh("queued")]
        db.c.insert_one({"x": 1})
        db.command("profile", 0, slowms=-1 if name == "mongod" else 0)
        try:
            client.admin.command("setParameter", 1, **{_POOL: 0})
            with pytest.raises(pymongo.errors.OperationFailure):
                db.command("find", "c", maxTimeMS=50, comment="spent")
            other = pymongo.MongoClient(*client.address, directConnection=True)
            waited = other[db.name]
            thread = threading.Thread(
                target=waited.command, args=("find", "c"), kwargs={"comment": "waited"}
            )
            thread.start()
            _wait_queued(client, 1)
            time.sleep(0.05)
            client.admin.command("setParameter", 1, **{_POOL: 1_000_000})
            thread.join(30)
            other.close()
        finally:
            db.command("profile", 0, slowms=100)
        lines = {}
        for raw in client.admin.command("getLog", "global")["log"]:
            entry = json.loads(raw)
            attr = entry.get("attr", {})
            comment = attr.get("command", {}).get("comment")
            if entry.get("msg") == "Slow query" and comment in ("spent", "waited"):
                ingress = attr["queues"]["ingress"]
                lines[comment] = (
                    ingress.get("admissions"),
                    ingress.get("totalTimeQueuedMicros", 0) >= 40_000,
                    attr.get("errName"),
                )
        got[name] = lines
    same(got["rust"], got["mongod"])
    assert got["mongod"]["waited"] == (1, True, None)
