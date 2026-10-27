"""Two-phase index builds (RM-187, RM-224) against a live mongod 8.2.

mongod builds an index over documents while the collection takes writes:
the build is recorded (`config.system.indexBuilds`, `startIndexBuild`),
scans with the collection unlocked, applies the writes made meanwhile, and
commits (`commitIndexBuild`) or aborts (`abortIndexBuild` with a cause). A
duplicate written during a unique build fails it at commit; `dropIndexes`,
`drop`, `dropDatabase` and `killOp` abort it; `collMod` and a rename are
refused while it runs. Each case runs the same steps on both servers and
compares what a client sees.

The builds are held still with mongod's own pause failpoints
(`hangAfterStartingIndexBuild`, `hangIndexBuildBeforeCommit`), so both
servers run with test commands on.
"""

from __future__ import annotations

import os
import re
import signal
import statistics
import subprocess
import tempfile
import threading
import time
from collections.abc import Callable, Iterator
from typing import Any

import pymongo
import pytest
from bson import Binary

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output
from tests.mongod_live.conftest import same

_UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
#: Reply fields that are per-server by nature.
_PER_SERVER = {"operationTime", "$clusterTime", "electionId", "opTime", "lastCommittedOpTime"}


def norm(value: Any) -> Any:
    """`value` with what differs between any two runs taken out: UUIDs
    (binary and in messages), timestamps, the member's address."""
    if isinstance(value, dict):
        return {k: norm(v) for k, v in value.items() if k not in _PER_SERVER}
    if isinstance(value, list):
        return [norm(v) for v in value]
    if isinstance(value, Binary) and value.subtype == 4:
        return "<uuid>"
    if isinstance(value, str):
        value = _UUID.sub("<uuid>", value)
        value = re.sub(r"'[\w.-]+:\d+'", "'<member>'", value)
        # A build killed while paused stops either at the pause or in its
        # scan, as mongod's thread happens to be placed; the scan's account
        # of where it stopped (and how long it took) is the only difference.
        value = re.sub(r"collection scan stopped\. .*? :: caused by :: ", "", value)
        return re.sub(r"^[\w.-]+:\d+$", "<member>", value)
    return value


def _start_rust(store: str) -> tuple[subprocess.Popen, pymongo.MongoClient]:
    config = os.path.join(store, "secantusd.toml")
    with open(config, "w") as f:
        f.write("[server]\nenable_test_commands = true\n")
    proc = subprocess.Popen(
        [
            str(RS_BINARY),
            "--config",
            config,
            "--port",
            "0",
            "--storage-path",
            os.path.join(store, "data"),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    assert proc.stdout is not None
    line, m = "", None
    for _ in range(50):
        line = proc.stdout.readline()
        m = _RS_BANNER.search(line)
        if m or not line:
            break
    assert m, f"secantusd-rs did not start: {line!r}"
    drain_output(proc)
    client = pymongo.MongoClient(
        host=m.group(1), port=int(m.group(2)), serverSelectionTimeoutMS=20_000
    )
    return proc, client


@pytest.fixture(scope="module")
def rust_fp() -> Iterator[Any]:
    """A `secantusd-rs` with test commands on (`configureFailPoint`)."""
    assert RS_BINARY.exists(), f"{RS_BINARY} is not built (./inv rust-binary-build)"
    store = tempfile.mkdtemp(prefix="live-index-builds-")
    proc, client = _start_rust(store)
    try:
        yield client
    finally:
        client.close()
        proc.terminate()
        proc.wait(timeout=30)
        subprocess.run(["rm", "-rf", store], check=True)


@pytest.fixture(scope="module")
def mongod_fp() -> Iterator[Any]:
    """A one-member mongod replica set with test commands on."""
    from tests.mongod_reference import MONGOD, _host_port, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(
        extra_args=("--replSet", "builds", "--setParameter", "enableTestCommands=1")
    )
    try:
        boot = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        host, port = _host_port(uri)
        boot.admin.command(
            {
                "replSetInitiate": {
                    "_id": "builds",
                    "members": [{"_id": 0, "host": f"{host}:{port}"}],
                }
            }
        )
        deadline = time.monotonic() + 60
        while not boot.admin.command("hello").get("isWritablePrimary"):
            assert time.monotonic() < deadline, "the single-node set never became primary"
            time.sleep(0.2)
        boot.close()
        client = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        yield client
        client.close()
    finally:
        _stop_mongod(proc, dbpath)


def _answer(fn: Callable[[], Any]) -> Any:
    try:
        return ("ok", norm(fn()))
    except pymongo.errors.OperationFailure as e:
        return ("err", norm(e.details))


class _Build:
    """A `createIndexes` on a thread of its own, held at a pause failpoint."""

    def __init__(self, client: Any, db: str, indexes: list, pause: str, **kw: Any) -> None:
        self.client, self.pause = client, pause
        client.admin.command("configureFailPoint", pause, mode="alwaysOn")
        self.out: dict = {}
        own = pymongo.MongoClient(*client.address, directConnection=True)

        def run() -> None:
            try:
                self.out["r"] = own[db].command("createIndexes", "c", indexes=indexes, **kw)
            except pymongo.errors.PyMongoError as e:
                self.out["e"] = getattr(e, "details", None) or str(e)
            finally:
                own.close()

        self.thread = threading.Thread(target=run)
        self.thread.start()
        # Started: its operation is listed -- or, held before its commit,
        # it has recorded itself ready (mongod lists no build operation
        # then, and a small build gets there between two looks).
        deadline = time.monotonic() + 30
        while not (
            self.ops()
            or client.config["system.indexBuilds"].find_one(
                {"commitReadyMembers": {"$exists": True}}
            )
        ):
            assert time.monotonic() < deadline, "the build never started"
            time.sleep(0.02)

    def ops(self) -> list:
        return [
            o
            for o in self.client.admin.command("currentOp", {"$all": True})["inprog"]
            if str(o.get("desc", "")).startswith("IndexBuildsCoordinator")
        ]

    def finish(self) -> Any:
        self.client.admin.command("configureFailPoint", self.pause, mode="off")
        self.thread.join(timeout=120)
        assert not self.thread.is_alive(), "the build never finished"
        return norm(self.out)


def _seed(client: Any, db: str, n: int = 100) -> None:
    client.drop_database(db)
    client[db].c.insert_many([{"_id": i, "a": i} for i in range(n)])


def _oplog(client: Any, start: Any, db: str) -> list:
    return [
        norm({k: e[k] for k in ("op", "ns", "o", "o2") if k in e})
        for e in client.local["oplog.rs"].find(
            {"ts": {"$gt": start}, "ns": {"$in": [f"{db}.$cmd", "config.system.indexBuilds"]}}
        )
    ]


def _last_ts(client: Any) -> Any:
    return client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]


# ------------------------------------------------------------- the log --


def _lifecycle(client: Any) -> dict:
    db = client.live_ib
    client.drop_database("live_ib")
    start = _last_ts(client)
    stream = db.watch(show_expanded_events=True, start_at_operation_time=start)
    db.c.insert_many([{"_id": i, "a": i % 3, "b": i} for i in range(10)])
    built = _answer(
        lambda: db.command(
            "createIndexes",
            "c",
            indexes=[
                {"key": {"a": 1}, "name": "a_1"},
                {"key": {"b": -1}, "name": "b_-1", "unique": True, "sparse": True},
            ],
        )
    )
    failed = _answer(
        lambda: db.command(
            "createIndexes", "c", indexes=[{"key": {"a": 1}, "name": "au", "unique": True}]
        )
    )
    db.create_collection("e")
    empty = _answer(
        lambda: db.command(
            "createIndexes",
            "e",
            indexes=[{"key": {"x": 1}, "name": "x_1"}, {"key": {"y": 1}, "name": "y_1"}],
        )
    )
    entries = _oplog(client, start, "live_ib")
    events = []
    while (ev := stream.try_next()) is not None:
        if ev["operationType"] != "insert":
            events.append(
                norm({k: ev[k] for k in ("operationType", "ns", "operationDescription") if k in ev})
            )
    stream.close()
    client.drop_database("live_ib")
    return {"built": built, "failed": failed, "empty": empty, "oplog": entries, "events": events}


def test_a_build_is_logged_as_mongod_logs_it(rust_fp: Any, mongod_fp: Any) -> None:
    """RM-224: an index build over documents is `config.system.indexBuilds`
    rows around `startIndexBuild` and `commitIndexBuild` -- or
    `abortIndexBuild` with its cause -- and one `createIndexes` event; an
    empty collection's indexes are a `createIndexes` each."""
    same(_lifecycle(rust_fp), _lifecycle(mongod_fp))


# ----------------------------------------------- writes during a build --


def _writes_meanwhile(client: Any, remove_duplicate: bool) -> dict:
    _seed(client, "live_ibw")
    db = client.live_ibw
    build = _Build(
        client,
        "live_ibw",
        [{"key": {"a": 1}, "name": "ua", "unique": True}],
        "hangAfterStartingIndexBuild",
    )
    # The build's own operation, as `currentOp` lists it.
    op = build.ops()[0]
    listed_op = norm({k: op.get(k) for k in ("isFromUserConnection", "op", "ns")}) | {
        "indexes": norm(op["command"]["indexes"])
    }
    took = []
    writes = []
    for cmd in [
        {"insert": "c", "documents": [{"_id": 1000, "a": 5}]},
        {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 7_000}}}]},
        {"delete": "c", "deletes": [{"q": {"_id": 2}, "limit": 1}]},
    ] + (
        [{"delete": "c", "deletes": [{"q": {"_id": 1000}, "limit": 1}]}] if remove_duplicate else []
    ):
        t = time.perf_counter()
        writes.append(_answer(lambda cmd=cmd: db.command(cmd)))
        took.append(time.perf_counter() - t)
    # Every write went through while the build was held.
    assert max(took) < 5, took
    result = build.finish()
    out = {
        "op": listed_op,
        "writes": writes,
        "result": result,
        "indexes": [ix["name"] for ix in db.c.list_indexes()],
        "a5": sorted(d["_id"] for d in db.c.find({"a": 5})),
        "valid": norm(db.command("validate", "c", full=True)).get("valid"),
    }
    client.drop_database("live_ibw")
    return out


def test_a_duplicate_written_during_a_unique_build_fails_it(rust_fp: Any, mongod_fp: Any) -> None:
    """The insert of a duplicate succeeds while the build runs; the build
    fails at commit with E11000 and leaves no index (mongod 8.2.11)."""
    same(_writes_meanwhile(rust_fp, False), _writes_meanwhile(mongod_fp, False))


def test_a_duplicate_removed_before_commit_does_not(rust_fp: Any, mongod_fp: Any) -> None:
    """The duplicate's key is checked again at commit, so a duplicate
    deleted before then does not fail the build."""
    same(_writes_meanwhile(rust_fp, True), _writes_meanwhile(mongod_fp, True))


# ----------------------------------------------- what ends a build --


def _ended_by(client: Any, how: str) -> dict:
    _seed(client, "live_ibd")
    db = client.live_ibd
    start = _last_ts(client)
    two = how == "dropIndexes one of two"
    indexes = [{"key": {"a": 1}, "name": "a_1"}] + (
        [{"key": {"b": 1}, "name": "b_1"}] if two else []
    )
    build = _Build(client, "live_ibd", indexes, "hangAfterStartingIndexBuild")
    if how.startswith("dropIndexes"):
        target: Any = {"by name": "a_1", "*": "*", "by key": {"a": 1}, "one of two": "a_1"}[
            how.split(" ", 1)[1]
        ]
        reply = _answer(lambda: db.command("dropIndexes", "c", index=target))
    elif how == "drop":
        reply = _answer(lambda: db.command("drop", "c"))
    elif how == "dropDatabase":
        reply = _answer(lambda: db.command("dropDatabase"))
    elif how == "killOp build":
        opid = build.ops()[0]["opid"]
        reply = _answer(lambda: client.admin.command("killOp", op=opid))
    elif how == "killOp command":
        ops = client.admin.command("currentOp", {"$all": True})["inprog"]
        opid = next(
            o["opid"]
            for o in ops
            if o.get("command", {}).get("createIndexes") == "c"
            and not str(o.get("desc", "")).startswith("IndexBuilds")
        )
        reply = _answer(lambda: client.admin.command("killOp", op=opid))
    elif how == "killAllSessions":
        reply = _answer(lambda: client.admin.command("killAllSessions", []))
    else:
        raise AssertionError(how)
    result = build.finish()
    time.sleep(0.3)
    out = {
        "reply": reply,
        "result": result,
        "indexes": [ix["name"] for ix in db.c.list_indexes()],
        "oplog": [
            e
            for e in _oplog(client, start, "live_ibd")
            if e["op"] != "i" or e["ns"] != "live_ibd.c"
        ],
    }
    client.drop_database("live_ibd")
    return out


@pytest.mark.parametrize(
    "how",
    [
        "dropIndexes by name",
        "dropIndexes *",
        "dropIndexes by key",
        "dropIndexes one of two",
        "drop",
        "dropDatabase",
        "killOp build",
        "killOp command",
        "killAllSessions",
    ],
)
def test_what_ends_a_build(rust_fp: Any, mongod_fp: Any, how: str) -> None:
    """mongod 8.2.11: dropping every index of a build, its collection or its
    database aborts it with 276 and the cause; dropping one of two of its
    indexes is refused (27) and the build goes on; `killOp` of the build
    fails it with 11601, of the command interrupts both."""
    same(_ended_by(rust_fp, how), _ended_by(mongod_fp, how))


def _timed_out(client: Any) -> dict:
    _seed(client, "live_ibt")
    db = client.live_ibt
    start = _last_ts(client)
    client.admin.command("configureFailPoint", "hangAfterStartingIndexBuild", mode="alwaysOn")
    try:
        reply = _answer(
            lambda: db.command(
                "createIndexes", "c", indexes=[{"key": {"a": 1}, "name": "a_1"}], maxTimeMS=300
            )
        )
    finally:
        client.admin.command("configureFailPoint", "hangAfterStartingIndexBuild", mode="off")
    time.sleep(0.5)
    out = {
        "reply": reply,
        "indexes": [ix["name"] for ix in db.c.list_indexes()],
        "oplog": [e for e in _oplog(client, start, "live_ibt") if e["ns"] != "live_ibt.c"],
    }
    client.drop_database("live_ibt")
    return out


def test_a_build_out_of_time_is_aborted(rust_fp: Any, mongod_fp: Any) -> None:
    """`maxTimeMS` spent while the build runs: 50 with a `writeConcernError`,
    and the build aborted with it."""
    same(_timed_out(rust_fp), _timed_out(mongod_fp))


# -------------------------------------------- the build, as listed --


def _while_building(client: Any) -> dict:
    _seed(client, "live_ibl")
    db = client.live_ibl
    build = _Build(
        client,
        "live_ibl",
        [{"key": {"a": 1}, "name": "a_1", "sparse": True}],
        "hangIndexBuildBeforeCommit",
    )
    # Held before its commit: the build has recorded itself ready.
    deadline = time.monotonic() + 30
    while not client.config["system.indexBuilds"].find_one(
        {"commitReadyMembers": {"$exists": True}}
    ):
        assert time.monotonic() < deadline, "the build never got ready to commit"
        time.sleep(0.05)
    info = db.command("listIndexes", "c", includeIndexBuildInfo=True)["cursor"]["firstBatch"]
    for ix in info:
        # The build's operation's id, whatever its number.
        if "indexBuildInfo" in ix:
            ix["indexBuildInfo"]["opid"] = "<opid>"
    out = {
        "listed": norm(list(db.c.list_indexes())),
        "uuids": norm(
            db.command("listIndexes", "c", includeBuildUUIDs=True)["cursor"]["firstBatch"]
        ),
        "info": norm(info),
        "row": norm(list(client.config["system.indexBuilds"].find())),
        "hint": _answer(lambda: list(db.c.find({"a": 5}).hint("a_1"))),
        "collMod": _answer(lambda: db.command("collMod", "c", validator={})),
        "rename": _answer(
            lambda: client.admin.command("renameCollection", "live_ibl.c", to="live_ibl.d")
        ),
        "conflict": _answer(
            lambda: db.command("createIndexes", "c", indexes=[{"key": {"z": 1}, "name": "a_1"}])
        ),
    }
    out["result"] = build.finish()
    client.drop_database("live_ibl")
    return out


def test_a_build_in_progress_as_listed(rust_fp: Any, mongod_fp: Any) -> None:
    """While it runs, a build's index is listed (with its build's UUID or
    progress when asked), its `config.system.indexBuilds` row is there, a
    hint cannot use it, `collMod` and a rename are refused (12587) and a
    spec with its name and another key conflicts with it (86)."""
    same(_while_building(rust_fp), _while_building(mongod_fp))


def _memory_parameter(client: Any) -> list:
    out = [
        _answer(
            lambda: client.admin.command("getParameter", 1, maxIndexBuildMemoryUsageMegabytes=1)
        )
    ]
    for value in [0, -1, 1, 49, 50, 50.5, "x", 200]:
        out.append(
            _answer(
                lambda v=value: client.admin.command(
                    "setParameter", 1, maxIndexBuildMemoryUsageMegabytes=v
                )
            )
        )
    return out


def test_the_build_memory_parameter(rust_fp: Any, mongod_fp: Any) -> None:
    """`maxIndexBuildMemoryUsageMegabytes`: 200 by default, a double, at
    least 50, settable at runtime."""
    same(_memory_parameter(rust_fp), _memory_parameter(mongod_fp))


# ------------------------------------------------ concurrency budget --


def test_writes_go_on_while_an_index_is_built(rust_fp: Any) -> None:
    """RM-187's budget: a build over 100,000 documents does not hold the
    collection's writes off. Another client's inserts, updates and deletes
    finish while it runs, in milliseconds, and the index that commits holds
    exactly the collection's keys (`validate` full). Before the hybrid
    build the first write waited for the whole build (5.4 s of a 5.5 s
    build over 300,000 documents, debug build, 2026-10-26)."""
    db = rust_fp.live_ibc
    rust_fp.drop_database("live_ibc")
    n = 100_000
    for start in range(0, n, 10_000):
        db.c.insert_many(
            [
                {"_id": i, "a": i % 1000, "s": f"{i:09d}{'x' * 100}"}
                for i in range(start, start + 10_000)
            ]
        )
    done = threading.Event()
    out: dict = {}

    def build() -> None:
        own = pymongo.MongoClient(*rust_fp.address, directConnection=True)
        try:
            out["r"] = own.live_ibc.command(
                "createIndexes", "c", indexes=[{"key": {"s": 1, "a": 1}, "name": "s_a"}]
            )
        finally:
            own.close()
            done.set()

    t = threading.Thread(target=build)
    t.start()
    latencies = []
    i = 0
    writer = pymongo.MongoClient(*rust_fp.address, directConnection=True).live_ibc.c
    while not done.is_set():
        t0 = time.perf_counter()
        if i % 3 == 0:
            writer.insert_one({"_id": n + i, "a": -i, "s": f"new{i:09d}"})
        elif i % 3 == 1:
            writer.update_one({"_id": (i * 7919) % n}, {"$set": {"s": f"upd{i:09d}"}})
        else:
            writer.delete_one({"_id": (i * 104_729) % n})
        if not done.is_set():
            latencies.append(time.perf_counter() - t0)
        i += 1
    t.join()
    assert out.get("r", {}).get("ok") == 1.0, out
    assert len(latencies) >= 20, f"only {len(latencies)} writes finished during the build"
    assert statistics.median(latencies) < 0.25, statistics.median(latencies)
    v = db.command("validate", "c", full=True)
    assert v["valid"], v
    assert v["keysPerIndex"] == {"_id_": v["nrecords"], "s_a": v["nrecords"]}, v
    assert db.c.count_documents({}, hint="s_a") == v["nrecords"]
    rust_fp.drop_database("live_ibc")


def test_a_build_cut_off_by_a_crash_is_restarted() -> None:
    """A build the process was killed in the middle of is run again when
    the store opens, as mongod 8.2.11 restarts it after `kill -9`: the
    writes made during the first attempt are in the index, and the log
    goes on from its `startIndexBuild` with the row's update,
    `commitIndexBuild` and the row's delete (measured on mongod)."""
    store = tempfile.mkdtemp(prefix="live-index-crash-")
    proc, client = _start_rust(store)
    try:
        db = client.live_ibk
        db.c.insert_many([{"_id": i, "a": i} for i in range(20_000)])
        build = _Build(
            client,
            "live_ibk",
            [{"key": {"a": 1}, "name": "a_1", "unique": True}],
            "hangAfterStartingIndexBuild",
        )
        db.c.insert_one({"_id": -1, "a": -1})
        db.c.update_one({"_id": 3}, {"$set": {"a": 1_000_003}})
        db.c.delete_one({"_id": 4})
        proc.send_signal(signal.SIGKILL)
        proc.wait(timeout=30)
        build.thread.join(timeout=60)
        client.close()
        proc, client = _start_rust(store)
        db = client.live_ibk
        assert [ix["name"] for ix in db.c.list_indexes()] == ["_id_", "a_1"]
        v = db.command("validate", "c", full=True)
        assert v["valid"] and v["keysPerIndex"] == {"_id_": 20_000, "a_1": 20_000}, v
        assert [d["_id"] for d in db.c.find({"a": 1_000_003}).hint("a_1")] == [3]
        assert list(client.config["system.indexBuilds"].find()) == []
        log = [
            (e["op"], e["ns"], next(iter(e["o"])))
            for e in client.local["oplog.rs"].find(
                {"ns": {"$in": ["live_ibk.$cmd", "config.system.indexBuilds"]}}
            )
        ]
        assert log[-5:] == [
            ("i", "config.system.indexBuilds", "_id"),
            ("c", "live_ibk.$cmd", "startIndexBuild"),
            ("u", "config.system.indexBuilds", "$v"),
            ("c", "live_ibk.$cmd", "commitIndexBuild"),
            ("d", "config.system.indexBuilds", "_id"),
        ], log
    finally:
        client.close()
        proc.terminate()
        proc.wait(timeout=30)
        subprocess.run(["rm", "-rf", store], check=True)
