"""Execution admission control against a live mongod 8.2 (RM-355).

mongod takes an execution ticket with each global lock: from its read pool
for IS or S, from its write pool for IX, waiting in the pool's queue when
none is free. Throughput probing sizes the pools (twice the logical cores,
split by `throughputProbingReadWriteRatio`, then moved to where throughput
is best), unless a concurrency given at startup fixes them.
`serverStatus.queues.execution` reports the pools and the monitor. Each test
runs the same thing on both servers and compares the answers.

What execution control adds to an operation's `Slow query` line (its
`execution` admissions, `storageExecutionMicros`, `delinquencyInfo`) is
RM-355's remainder, left out here.
"""

from __future__ import annotations

import contextlib
import shutil
import subprocess
import tempfile
import threading
import time
from collections.abc import Callable, Iterator
from typing import Any

import pymongo
import pytest
from bson import Int64

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output

from .conftest import same

_WRITE = "storageEngineConcurrentWriteTransactions"
_READ = "storageEngineConcurrentReadTransactions"

#: Every parameter, in name order.
_PARAMETERS = sorted(
    [
        "delinquentAcquisitionIntervalMillis",
        "storageEngineConcurrencyAdjustmentAlgorithm",
        "storageEngineConcurrencyAdjustmentIntervalMillis",
        "storageEngineConcurrentReadTransactions",
        "storageEngineConcurrentWriteTransactions",
        "storageEngineReadMaxQueueDepth",
        "storageEngineWriteMaxQueueDepth",
        "throughputProbingConcurrencyMovingAverageWeight",
        "throughputProbingInitialConcurrency",
        "throughputProbingMaxConcurrency",
        "throughputProbingMinConcurrency",
        "throughputProbingReadWriteRatio",
        "throughputProbingStallDetectionTimeoutMs",
        "throughputProbingStepMultiple",
        "wiredTigerConcurrentReadTransactions",
        "wiredTigerConcurrentWriteTransactions",
    ]
)


@pytest.fixture
def servers(rust_mongo: Any, mongod_rs: Any) -> Iterator[dict[str, Any]]:
    """Both servers, the parameters a test changes put back afterwards."""
    both = {"rust": rust_mongo, "mongod": mongod_rs}
    yield both
    for client in both.values():
        client.admin.command(
            {
                "setParameter": 1,
                "storageEngineReadMaxQueueDepth": 1_000_000,
                "storageEngineWriteMaxQueueDepth": 1_000_000,
                "throughputProbingStepMultiple": 0.1,
                "throughputProbingMaxConcurrency": 128,
                "throughputProbingReadWriteRatio": 0.5,
                "throughputProbingConcurrencyMovingAverageWeight": 0.2,
                "throughputProbingStallDetectionTimeoutMs": 60_000,
            }
        )


def _reply(call: Callable[[], Any]) -> dict:
    """What a command answered: its error's code, name and message, or its
    `was`."""
    try:
        out = call()
    except pymongo.errors.OperationFailure as e:
        d = e.details or {}
        return {k: d.get(k) for k in ("code", "codeName", "errmsg")}
    return {"was": out.get("was")}


def _kinds(value: Any) -> Any:
    """`value` with every leaf replaced by its BSON type's name."""
    if isinstance(value, dict):
        return {k: _kinds(v) for k, v in value.items()}
    return type(value).__name__


def test_the_parameters_have_mongods_defaults_and_settability(servers: dict) -> None:
    """All sixteen, with mongod 8.2.11's defaults, types and when each can be
    set (measured 2026-11-09). The reference is started with a delinquency
    interval of its own, so that one's value is not compared."""
    got = {}
    for name, client in servers.items():
        params = client.admin.command(
            {"getParameter": {"showDetails": True}, **dict.fromkeys(_PARAMETERS, 1)}
        )
        got[name] = {
            p: {
                **params[p],
                "value": (type(params[p]["value"]).__name__, params[p]["value"])
                if p != "delinquentAcquisitionIntervalMillis"
                else None,
            }
            for p in _PARAMETERS
        }
    same(got["rust"], got["mongod"])
    assert got["mongod"]["throughputProbingStepMultiple"]["value"] == ("float", 0.1)
    assert got["mongod"]["storageEngineConcurrencyAdjustmentAlgorithm"]["value"] == (
        "str",
        "throughputProbing",
    )


#: `setParameter` values, in the order sent: each refusal's words, or the
#: value it replaced (measured 8.2.11, 2026-11-09).
_SETS = [
    ("storageEngineReadMaxQueueDepth", -1),
    ("storageEngineReadMaxQueueDepth", 5),
    ("storageEngineReadMaxQueueDepth", 2.7),
    ("storageEngineReadMaxQueueDepth", "x"),
    ("storageEngineReadMaxQueueDepth", True),
    ("storageEngineReadMaxQueueDepth", Int64(1 << 40)),
    ("storageEngineWriteMaxQueueDepth", -1),
    ("throughputProbingStepMultiple", 0.6),
    ("throughputProbingStepMultiple", 0.001),
    ("throughputProbingStepMultiple", 0),
    ("throughputProbingStepMultiple", "x"),
    ("throughputProbingStepMultiple", True),
    ("throughputProbingStepMultiple", 0.2),
    ("throughputProbingMaxConcurrency", 2),
    ("throughputProbingMaxConcurrency", 3.9),
    ("throughputProbingMaxConcurrency", "x"),
    ("throughputProbingMaxConcurrency", 200),
    ("throughputProbingReadWriteRatio", 1.5),
    ("throughputProbingReadWriteRatio", -0.1),
    ("throughputProbingReadWriteRatio", 1),
    ("throughputProbingConcurrencyMovingAverageWeight", 0),
    ("throughputProbingConcurrencyMovingAverageWeight", 1.1),
    ("throughputProbingStallDetectionTimeoutMs", 0),
    ("throughputProbingStallDetectionTimeoutMs", 1.5),
    # Startup only.
    ("storageEngineConcurrencyAdjustmentAlgorithm", "fixedConcurrentTransactions"),
    ("storageEngineConcurrencyAdjustmentIntervalMillis", 50),
    ("throughputProbingInitialConcurrency", 8),
    ("throughputProbingMinConcurrency", 4),
    ("delinquentAcquisitionIntervalMillis", 40),
]


def test_set_parameter_coerces_and_refuses_as_mongod(servers: dict) -> None:
    """Each value is coerced as mongod's IDL coerces it (a bool or a
    fraction to a whole number, but no bool to a fraction), checked by the
    parameter's validator in mongod's words, and refused at runtime when
    it is startup only."""
    got = {}
    for name, client in servers.items():
        out = []
        for param, value in _SETS:
            answer = _reply(
                lambda c=client, p=param, v=value: c.admin.command("setParameter", 1, **{p: v})
            )
            now = client.admin.command("getParameter", 1, **{param: 1})[param]
            if param == "delinquentAcquisitionIntervalMillis":
                # The reference is started with an interval of its own.
                now = type(now).__name__
            out.append((param, repr(value), answer, type(now).__name__, now))
        got[name] = out
    same(got["rust"], got["mongod"])


def test_probing_refuses_a_concurrency_after_storing_it(servers: dict) -> None:
    """While probing sizes the pools a concurrency is refused with 20 -- after
    mongod has stored it, so `getParameter`, and its deprecated alias, read
    the refused value back."""
    got = {}
    for name, client in servers.items():
        out = []
        for param, value in ((_WRITE, 10), ("wiredTigerConcurrentReadTransactions", 12)):
            out.append(
                _reply(
                    lambda c=client, p=param, v=value: c.admin.command("setParameter", 1, **{p: v})
                )
            )
        params = client.admin.command(
            {
                "getParameter": 1,
                _WRITE: 1,
                _READ: 1,
                "wiredTigerConcurrentWriteTransactions": 1,
                "wiredTigerConcurrentReadTransactions": 1,
            }
        )
        out.append({k: params[k] for k in _PARAMETERS if k in params})
        # Back down: 0 is under the floor of 5, lifted only with test
        # commands, which neither server has here; 5 is stored and refused.
        for param, value in ((_WRITE, 0), (_WRITE, 5), (_READ, 5)):
            out.append(
                _reply(
                    lambda c=client, p=param, v=value: c.admin.command("setParameter", 1, **{p: v})
                )
            )
        got[name] = out
    same(got["rust"], got["mongod"])


def test_server_status_reports_the_pools_and_the_monitor(servers: dict) -> None:
    """`queues.execution`: the write and read pools, then the monitor, in
    mongod's order and types, ahead of `queues.ingress`; and
    `globalLock.currentQueue` counting the operations queued in them."""
    shapes = {}
    for name, client in servers.items():
        status = client.admin.command("serverStatus")
        queues = status["queues"]
        execution = queues["execution"]
        shapes[name] = (
            list(queues)[:2],
            _kinds(execution),
            _kinds(status["globalLock"]["currentQueue"]),
        )
        for pool in ("read", "write"):
            # Probing keeps each pool within its minimum and maximum.
            assert 4 <= execution[pool]["totalTickets"] <= 128, (name, execution)
    same(shapes["rust"], shapes["mongod"])
    assert shapes["mongod"][0] == ["execution", "ingress"]


# ------------------------------------------------ a fixed pool of tickets


@contextlib.contextmanager
def _rust_server(*args: str) -> Iterator[pymongo.MongoClient]:
    store = tempfile.mkdtemp(prefix="rm355-")
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", store, *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        line, m = "", None
        for _ in range(20):
            line = proc.stdout.readline()
            m = _RS_BANNER.search(line)
            if m or not line:
                break
        assert m, f"secantusd-rs did not start: {line!r}"
        drain_output(proc)
        client = pymongo.MongoClient(
            host=m.group(1), port=int(m.group(2)), serverSelectionTimeoutMS=10_000
        )
        try:
            yield client
        finally:
            client.close()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(store, ignore_errors=True)


@contextlib.contextmanager
def _mongod_set(*args: str) -> Iterator[pymongo.MongoClient]:
    from tests.mongod_reference import MONGOD, _host_port, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(extra_args=("--replSet", "rm355", *args))
    try:
        host, port = _host_port(uri)
        boot = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        boot.admin.command(
            {"replSetInitiate": {"_id": "rm355", "members": [{"_id": 0, "host": f"{host}:{port}"}]}}
        )
        deadline = time.monotonic() + 60
        while not boot.admin.command("hello").get("isWritablePrimary"):
            assert time.monotonic() < deadline, "the single-node set never became primary"
            time.sleep(0.2)
        boot.close()
        client = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        try:
            yield client
        finally:
            client.close()
    finally:
        _stop_mongod(proc, dbpath)


@pytest.fixture(scope="module")
def fixed() -> Iterator[dict[str, Any]]:
    """Both servers started with five write tickets, fixed."""
    arg = ("--setParameter", f"{_WRITE}=5")
    with _rust_server(*arg) as rust, _mongod_set(*arg) as mongod:
        yield {"rust": rust, "mongod": mongod}


def test_a_concurrency_given_at_startup_fixes_the_pools(fixed: dict) -> None:
    """Five write tickets and the default 128 read ones, no monitor, the
    algorithm reported as fixed; and resizable at runtime."""
    got = {}
    for name, client in fixed.items():
        out = []
        execution = client.admin.command("serverStatus")["queues"]["execution"]
        out.append(
            {k: v.get("totalTickets") if isinstance(v, dict) else v for k, v in execution.items()}
        )
        params = client.admin.command(
            {
                "getParameter": 1,
                "storageEngineConcurrencyAdjustmentAlgorithm": 1,
                _WRITE: 1,
                _READ: 1,
            }
        )
        out.append({k: params[k] for k in _PARAMETERS if k in params})
        out.append(_reply(lambda c=client: c.admin.command("setParameter", 1, **{_READ: 7})))
        out.append(_reply(lambda c=client: c.admin.command("setParameter", 1, **{_READ: 3})))
        execution = client.admin.command("serverStatus")["queues"]["execution"]
        out.append(execution["read"]["totalTickets"])
        out.append(_reply(lambda c=client: c.admin.command("setParameter", 1, **{_READ: 128})))
        got[name] = out
    same(got["rust"], got["mongod"])
    assert got["mongod"][0] == {"write": 5, "read": 128}


def test_a_writer_past_the_pool_queues_for_a_ticket(fixed: dict) -> None:
    """Five `drop`s waiting on a transaction's collection lock hold the five
    write tickets; a sixth writer, on another collection, queues for one
    (`globalLock.currentQueue.writers`, the write pool's `queueLength`)
    while a read goes on, and runs once the drops' `maxTimeMS` has let their
    tickets go."""
    got = {name: _queue_a_writer(client) for name, client in fixed.items()}
    same(got["rust"], got["mongod"])
    # The writer queued for a ticket, and the five drops holding theirs
    # while they wait for the collection's lock (mongod's
    # `Locker::getClientState`).
    assert got["mongod"][0][0] == {"total": 6, "readers": 0, "writers": 6}
    assert got["mongod"][1] == ["50"] * 5


def _queue_a_writer(client: pymongo.MongoClient) -> tuple:
    """Hold the five write tickets with drops waiting on a transaction's
    lock, queue a sixth writer behind them, and say what the server reported
    while it queued, what the drops answered and what the writer did."""
    db = client["rm355q"]
    db.drop_collection("locked")
    db.drop_collection("other")
    db.create_collection("locked")
    db.create_collection("other")
    db.locked.insert_one({"_id": 0})
    session = client.start_session()
    session.start_transaction()
    db.locked.insert_one({"_id": 1}, session=session)
    drops: list[Any] = []

    def drop() -> None:
        try:
            db.command("drop", "locked", maxTimeMS=3000)
            drops.append("ok")
        except pymongo.errors.OperationFailure as e:
            drops.append(e.code)

    threads = [threading.Thread(target=drop) for _ in range(5)]
    for t in threads:
        t.start()
    deadline = time.monotonic() + 10
    while client.admin.command("serverStatus")["queues"]["execution"]["write"]["out"] < 5:
        assert time.monotonic() < deadline, "the drops never took the five tickets"
        time.sleep(0.02)
    inserted: list[Any] = []
    writer = threading.Thread(target=lambda: inserted.append(db.other.insert_one({}).acknowledged))
    writer.start()
    deadline = time.monotonic() + 10
    while (
        client.admin.command("serverStatus")["queues"]["execution"]["write"]["normalPriority"][
            "queueLength"
        ]
        < 1
    ):
        assert time.monotonic() < deadline, "the sixth writer never queued"
        time.sleep(0.02)
    status = client.admin.command("serverStatus")
    queued = (
        status["globalLock"]["currentQueue"],
        status["queues"]["execution"]["write"]["normalPriority"]["queueLength"],
        list(inserted),
        db.other.count_documents({}),
    )
    for t in threads:
        t.join()
    writer.join()
    session.abort_transaction()
    session.end_session()
    return (queued, sorted(map(str, drops)), inserted)
