"""Gap tests for the move to mongod's storage model
(`tasks/mongod-storage-model-plan.md`), and for the `shutdown` command found
missing while measuring it (phase 0c).

Each test asserts what a mongod 8.2.11 store is, as measured in phase 0
(`tests/fixtures/storage_model/mongod-8.2.11-layout.json`), or what mongod
does with a store. They start servers of their own: a store is inspected
after its server has stopped, and the `shutdown` test stops its server.
"""

from __future__ import annotations

import contextlib
import shutil
import signal
import socket
import subprocess
import time
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import bson
import pymongo
import pytest

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, mongo_answer, rust_mongo_server
from tests.mongod_reference import MONGOD

#: `storage.bson` in a mongod 8.2.11 store (phase 0b).
MONGOD_STORAGE_BSON = {
    "storage": {
        "engine": "wiredTiger",
        "options": {
            "directoryPerDB": False,
            "directoryForIndexes": False,
            "groupCollections": False,
        },
    }
}


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


#: The replica-set name `secantusd-mdb` records in `local.system.replset`.
SECANTUS_SET = "secantus"


def _mongod_on(
    dbpath: Path, port: int | None = None, set_name: str = "rs0", *extra: str
) -> tuple[subprocess.Popen, pymongo.MongoClient | None]:
    """Start mongod as a one-member replica set on `dbpath`, which may already
    hold a store. Returns the process and a client to the primary, or `None`
    for the client when mongod refused the store (it exits).

    A store that already holds a set's configuration is opened as that set's
    member: under its set name and on the port its one member names. mongod
    8.2.11 started under another name or port leaves such a store REMOVED
    ("NodeNotFound"), its own store included (measured 2026-11-21)."""
    assert MONGOD, "no mongod on PATH: the reference server is required"
    port = port or _free_port()
    proc = subprocess.Popen(
        [
            MONGOD,
            "--dbpath",
            str(dbpath),
            "--port",
            str(port),
            "--bind_ip",
            "127.0.0.1",
            "--replSet",
            set_name,
            "--logpath",
            str(dbpath.parent / f"mongod-{port}.log"),
            "--logappend",
            *extra,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return proc, None
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                break
        except OSError:
            time.sleep(0.2)
    client = pymongo.MongoClient(
        f"mongodb://127.0.0.1:{port}/?directConnection=true", serverSelectionTimeoutMS=10_000
    )
    return proc, client


@contextlib.contextmanager
def _mongod_answers(dbpath: Path) -> Iterator[None]:
    """Turn a mongod that dropped the connection or refused an operation into
    an assertion, with the end of its log: a gap test fails on an assert, and
    mongod failing on our store is the gap."""
    try:
        yield
    except pymongo.errors.PyMongoError as exc:
        logs = sorted(dbpath.parent.glob("mongod-*.log"))
        tail = logs[-1].read_text(errors="replace")[-3000:] if logs else "(no log)"
        raise AssertionError(f"mongod failed on the store: {exc}\n{tail}") from exc


def _wait_primary(client: pymongo.MongoClient, port_host: str) -> None:
    hello = client.admin.command("hello")
    if "setName" not in hello:
        client.admin.command(
            "replSetInitiate", {"_id": "rs0", "members": [{"_id": 0, "host": port_host}]}
        )
    _until_primary(client)


def _until_primary(client: pymongo.MongoClient) -> None:
    deadline = time.monotonic() + 60
    while not client.admin.command("hello").get("isWritablePrimary"):
        assert time.monotonic() < deadline, "mongod never became primary"
        time.sleep(0.2)


def _stop_mongod(proc: subprocess.Popen, client: pymongo.MongoClient | None) -> None:
    if client is not None:
        # The connection drops as mongod stops. A primary whose set names
        # another member refuses a plain `shutdown` ("No electable
        # secondaries caught up", mongod 8.2.11, 2026-11-22) and keeps
        # running; `force` stops it cleanly all the same.
        try:
            client.admin.command("shutdown")
        except pymongo.errors.OperationFailure:
            with contextlib.suppress(pymongo.errors.PyMongoError):
                client.admin.command("shutdown", force=True)
        except pymongo.errors.PyMongoError:
            pass
        client.close()
    try:
        proc.wait(timeout=60)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def _secantus_store(tmp_path: Path) -> Path:
    """A store written by `secantusd-mdb`: one collection with a secondary
    index, then a clean stop (SIGTERM)."""
    return _secantus_store_at(tmp_path)[0]


def _secantus_store_at(tmp_path: Path) -> tuple[Path, int]:
    """`_secantus_store`, and the port its server bound: the address the
    store's replica-set configuration names."""
    store = tmp_path / "store"
    with rust_mongo_server(store) as client:
        coll = client["app"]["c"]
        coll.insert_many([{"_id": i, "a": i % 3} for i in range(10)])
        coll.create_index("a", name="a_1")
        port = client.address[1]
    return store, port


@pytest.mark.backlog_fixed("RM-724")
def test_shutdown_command_stops_the_server(tmp_path: Path) -> None:
    """mongod 8.2.11: `shutdown` closes the connection (the driver sees a
    network error, not a reply) and the process exits. `secantusd-mdb`
    answered CommandNotFound (59) and kept running."""
    with rust_mongo_server(tmp_path / "store") as client:
        # Read the address while the server is up: after `shutdown` the
        # driver marks the server Unknown, and `client.address` then runs
        # server selection, which fails with "Connection refused" once the
        # process has gone (main's CI, run 45200250631, 2026-11-23).
        address = client.address
        assert address is not None
        try:
            client.admin.command("shutdown")
            answer: tuple = ("replied",)
        except pymongo.errors.OperationFailure as exc:
            answer = ("err", exc.code)
        except pymongo.errors.ConnectionFailure:
            answer = ("closed",)
        assert answer == ("closed",)
        deadline = time.monotonic() + 30
        while True:
            try:
                with socket.create_connection(address, timeout=0.5):
                    pass
            except OSError:
                break  # nothing listens: the server stopped
            assert time.monotonic() < deadline, "the server still listens after `shutdown`"
            time.sleep(0.2)


@pytest.mark.backlog_fixed("RM-723")
def test_store_has_mongods_files(tmp_path: Path) -> None:
    """A mongod 8.2.11 store holds `_mdb_catalog.wt`, `sizeStorer.wt`,
    `storage.bson` (with the content above) and its log in `journal/`."""
    store = _secantus_store(tmp_path)
    present = {
        name: (store / name).exists()
        for name in ("_mdb_catalog.wt", "sizeStorer.wt", "storage.bson", "journal")
    }
    assert present == dict.fromkeys(present, True)
    assert bson.decode((store / "storage.bson").read_bytes()) == MONGOD_STORAGE_BSON


@pytest.mark.backlog_fixed("RM-723")
def test_one_table_per_collection_and_per_index(tmp_path: Path) -> None:
    """mongod 8.2.11 gives a collection one `collection-<UUID>.wt` table and
    each of its indexes, `_id_` included, one `index-<UUID>.wt` table; no
    other table holds its documents or keys."""
    base = _secantus_store(tmp_path / "base")
    store = tmp_path / "store"
    with rust_mongo_server(store) as client:
        coll = client["app"]["c"]
        coll.insert_many([{"_id": i, "a": i % 3} for i in range(10)])
        coll.create_index("a", name="a_1")
        client["app"]["d"].insert_one({"_id": 1})

    def count(path: Path, prefix: str) -> int:
        return sum(1 for p in path.iterdir() if p.name.startswith(prefix) and p.suffix == ".wt")

    # `store` holds one collection (and one index) more than `base`.
    assert (
        count(store, "collection-") - count(base, "collection-"),
        count(store, "index-") - count(base, "index-"),
    ) == (1, 1)
    assert count(store, "collection-") >= 2
    assert count(store, "index-") >= 3


@pytest.mark.backlog_fixed("RM-723")
def test_a_clustered_collection_has_no_id_index_table(tmp_path: Path) -> None:
    """mongod 8.2.11 keys a clustered collection by the KeyString of `_id`
    and gives it no `_id_` table: it adds a `collection-<UUID>.wt` table and
    no `index-<UUID>.wt` one (an ordinary collection adds both)."""

    def tables(store: Path) -> tuple[int, int]:
        names = [p.name for p in store.iterdir() if p.suffix == ".wt"]
        return (
            sum(n.startswith("collection-") for n in names),
            sum(n.startswith("index-") for n in names),
        )

    base = tmp_path / "base"
    with rust_mongo_server(base) as client:
        client["app"]["plain"].insert_one({"_id": 1})
    store = tmp_path / "store"
    with rust_mongo_server(store) as client:
        client["app"]["plain"].insert_one({"_id": 1})
        client["app"].create_collection(
            "clustered", clusteredIndex={"key": {"_id": 1}, "unique": True}
        )
        client["app"]["clustered"].insert_many([{"_id": i} for i in range(5)])
    (c0, i0), (c1, i1) = tables(base), tables(store)
    assert (c1 - c0, i1 - i0) == (1, 0)


@pytest.mark.backlog_fixed("RM-723")
def test_mongod_opens_a_secantus_store(tmp_path: Path) -> None:
    """mongod 8.2.11 started with `--replSet` on a store `secantusd-mdb`
    wrote finds its collection, its documents and its index, and
    `validate({full: true})` passes."""
    store, port = _secantus_store_at(tmp_path)
    proc, client = _mongod_on(store, port, SECANTUS_SET)
    try:
        assert client is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(client)
            coll = client["app"]["c"]
            assert sorted(d["_id"] for d in coll.find()) == list(range(10))
            assert sorted(ix["name"] for ix in coll.list_indexes()) == ["_id_", "a_1"]
            result = client["app"].command("validate", "c", full=True)
            assert (result["valid"], result["nrecords"]) == (True, 10)
    finally:
        _stop_mongod(proc, client)


#: Index kinds whose keys this server does not write as mongod does: a
#: store holding one is not yet a store mongod validates (each has its
#: backlog item and a gap test below). Text indexes are refused by name
#: when created, so the scenario's store never holds one.
_KINDS_NOT_MONGODS = ("flat_2d", "loc_2dsphere", "w_wildcard")


def _index_filter(ix: dict) -> dict:
    """A filter an index can serve whole, for a hinted walk of it: a partial
    index's own filter, a sparse index's fields present, else everything."""
    if "partialFilterExpression" in ix:
        return ix["partialFilterExpression"]
    if ix.get("sparse"):
        return {field: {"$exists": True} for field in ix["key"]}
    return {}


def _store_contents(client: pymongo.MongoClient) -> dict:
    """What a server serves of a store: per namespace outside `local` (whose
    collections mongod writes to as it starts), its documents, and each
    index's hinted walk, as `mongo_answer`s."""
    out: dict = {}
    for db in ("admin", "app", "config"):
        for info in client[db].list_collections():
            name = info["name"]
            coll = client[db][name]
            key = f"{db}.{name}"
            out[key] = {"docs": mongo_answer(lambda c=coll: _sorted(c.find()))}
            if info.get("type") != "collection":
                continue
            for ix in coll.list_indexes():
                out[key][ix["name"]] = mongo_answer(
                    lambda c=coll, ix=ix: _sorted(c.find(_index_filter(ix), hint=ix["name"]))
                )
    return out


def _sorted(docs: Any) -> list:
    """The documents as their BSON bytes, sorted: compared byte for byte, so
    a NaN equals itself and an int32 is not an int64."""
    return sorted(bson.encode(d) for d in docs)


#: The collections mongod 8.2.11 cannot finish validating while it runs,
#: on a store it wrote itself as much as on this server's: after a restart
#: as primary, ten `validate({full: true})` of each, half a second apart,
#: all warned "Could not complete validation of table:... This is a
#: transient issue as the collection was actively in use by other
#: operations." (`startup_log` adds "Found warnings in _id_"), measured
#: 2026-11-21. Every other `local` collection validates clean there.
_BUSY_WHILE_MONGOD_RUNS = {
    "local.replset.election",
    "local.replset.oplogTruncateAfterPoint",
    "local.startup_log",
    "local.system.replset",
}


def _validation_findings(client: pymongo.MongoClient, db: str, name: str) -> Any:
    """What `validate({full: true})` finds wrong, beyond the answers mongod
    gives for its own store: `None` when nothing."""
    result = client[db].command("validate", name, full=True)
    warnings = [
        w
        for w in result.get("warnings", [])
        # mongod's own answer for any oplog, its own included.
        if w != "Skipping verification of the WiredTiger table for the oplog."
    ]
    if f"{db}.{name}" in _BUSY_WHILE_MONGOD_RUNS:
        warnings = [
            w for w in warnings if "transient issue" not in w and w != "Found warnings in _id_"
        ]
    if result["valid"] and not result.get("errors") and not warnings:
        return None
    return (result["valid"], result.get("errors"), warnings, result)


@pytest.mark.backlog_fixed("RM-723")
def test_mongod_opens_the_phase_0b_store(tmp_path: Path) -> None:
    """Storage-model phase 7: `secantusd-mdb` writes the store phase 0b's
    scenario makes (every collection and index kind, capped, clustered,
    time-series, pre-images, a view, a user and a role, a drop, a rename and
    a transaction; `tools/probes/mongod_store_layout.py`) and stops cleanly;
    mongod 8.2.11, started on it with `--replSet` as the set's one member,
    becomes primary, `validate({full: true})` passes on every collection,
    every document and every index walk reads as SecantusDB served it, and a
    change stream resumes from a token SecantusDB issued."""
    from tools.probes.mongod_store_layout import run_scenario

    store = tmp_path / "store"
    with rust_mongo_server(store) as client:
        steps = run_scenario(client, strict=False)
        # Refused by name when created (phase 4), so the store has none:
        # the text index (the hashed index is built, RM-732, and mongod
        # validates its keys below).
        assert [s.split(":")[0] for s in steps if s.startswith("FAILED")] == [
            "FAILED kinds",
        ], steps
        for name in _KINDS_NOT_MONGODS:
            client["app"]["kinds"].drop_index(name)
        with client["app"]["c"].watch() as stream:
            client["app"]["c"].insert_one({"_id": 500, "a": 1, "b": "x500"})
            token = next(stream)["_id"]
        client["app"]["c"].insert_one({"_id": 501, "a": 2, "b": "x501"})
        client["app"]["c"].update_one({"_id": 501}, {"$set": {"a": 3}})
        served = _store_contents(client)
        namespaces = [
            (db, info["name"])
            for db in client.list_database_names()
            for info in client[db].list_collections()
            if info.get("type") != "view"
        ]
        port = client.address[1]
    proc, mongod = _mongod_on(store, port, SECANTUS_SET)
    try:
        assert mongod is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(mongod)
            findings = {
                f"{db}.{name}": _validation_findings(mongod, db, name) for db, name in namespaces
            }
            assert {ns: f for ns, f in findings.items() if f is not None} == {}
            # mongod adds `admin.system.keys` as it becomes primary.
            reread = _store_contents(mongod)
            assert {ns: reread.get(ns) for ns in served} == served
            with mongod["app"]["c"].watch(resume_after=token) as stream:
                events = [next(stream), next(stream)]
            assert [(e["operationType"], e["documentKey"]) for e in events] == [
                ("insert", {"_id": 501}),
                ("update", {"_id": 501}),
            ]
    finally:
        _stop_mongod(proc, mongod)


@pytest.mark.backlog_fixed("RM-723")
def test_secantus_opens_a_mongod_store(tmp_path: Path) -> None:
    """A store mongod 8.2.11 wrote, opened by `secantusd-mdb`, serves the
    collection mongod wrote: its documents and its index."""
    store = tmp_path / "store"
    store.mkdir()
    proc, client = _mongod_on(store)
    assert client is not None, "mongod did not start on an empty directory"
    try:
        host, port = client.address or ("127.0.0.1", 0)
        _wait_primary(client, f"{host}:{port}")
        with pymongo.MongoClient(
            host, port, replicaSet="rs0", serverSelectionTimeoutMS=10_000
        ) as rs:
            rs["app"]["c"].insert_many([{"_id": i, "a": i % 3} for i in range(10)])
            rs["app"]["c"].create_index("a", name="a_1")
    finally:
        _stop_mongod(proc, client)
    with rust_mongo_server(store) as rust:
        coll = rust["app"]["c"]
        answer = mongo_answer(lambda: sorted(d["_id"] for d in coll.find()))
        assert answer == ("ok", list(range(10)))
        indexes = mongo_answer(lambda: sorted(ix["name"] for ix in coll.list_indexes()))
        assert indexes == ("ok", ["_id_", "a_1"])


def _collmodded_indexes(client: Any) -> None:
    """Indexes a `collMod` has changed, which mongod keeps in its catalog in
    build order with the spec fields as it wrote them (RM-734)."""
    db = client["app"]
    coll = db.create_collection("c")
    coll.insert_many([{"_id": i, "a": i, "b": i, "d": i} for i in range(5)])
    db.command(
        "createIndexes",
        "c",
        indexes=[
            {"name": "a_1", "key": {"a": 1}, "unique": True},
            {"name": "b_1", "key": {"b": 1}},
            {"name": "d_1", "key": {"d": 1}, "hidden": True},
            {"name": "e_1", "key": {"e": 1}},
        ],
    )
    db.command("collMod", "c", index={"name": "e_1", "prepareUnique": True})
    db.command("collMod", "c", index={"name": "a_1", "hidden": True})
    db.command("collMod", "c", index={"name": "b_1", "expireAfterSeconds": 100})
    db.command("collMod", "c", index={"name": "d_1", "hidden": False})


def _catalog_specs(client: Any) -> list[list[tuple[str, Any]]]:
    out = []
    for entry in client["app"]["c"].aggregate([{"$listCatalog": {}}]):
        for ix in entry["md"]["indexes"]:
            out.append(list(ix["spec"].items()))
    return out


def _drop_order_after_a_restart(store: Path, port: int | None) -> list[str]:
    """The `dropIndexes: "*"` entries of the oplog, in the order logged, once
    `store` is opened by mongod (so with no in-memory order moved)."""
    proc, client = _mongod_on(store, port, SECANTUS_SET if port else "rs0")
    try:
        assert client is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(client)
            client["app"]["c"].drop_indexes()
            rows = client["local"]["oplog.rs"].find({"o.dropIndexes": "c"}).sort("$natural", 1)
            return [r["o"]["index"] for r in rows][-4:]
    finally:
        _stop_mongod(proc, client)


@pytest.mark.backlog_fixed("RM-734")
def test_mongod_opens_the_collmodded_indexes_secantus_wrote(tmp_path: Path) -> None:
    """A store `secantusd-mdb` wrote, with indexes a `collMod` changed, is a
    store mongod 8.2.11 validates and reads in the same catalog order (build
    order), the spec fields as they were written."""
    store = tmp_path / "store"
    with rust_mongo_server(store) as client:
        _collmodded_indexes(client)
        ours = _catalog_specs(client)
        port = client.address[1]
    assert [[k for k, _ in spec] for spec in ours] == [
        ["v", "key", "name"],
        ["v", "unique", "key", "name", "hidden"],
        ["v", "key", "name", "expireAfterSeconds"],
        ["v", "key", "name"],
        ["v", "key", "name", "prepareUnique"],
    ]
    proc, client = _mongod_on(store, port, SECANTUS_SET)
    try:
        assert client is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(client)
            assert _catalog_specs(client) == ours
            result = client["app"].command("validate", "c", full=True)
            assert (result["valid"], result["nrecords"]) == (True, 5)
    finally:
        _stop_mongod(proc, client)
    assert _drop_order_after_a_restart(store, port) == ["a_1", "b_1", "d_1", "e_1"]


@pytest.mark.backlog_fixed("RM-734")
def test_secantus_keeps_the_catalog_order_of_collmodded_indexes_mongod_wrote(
    tmp_path: Path,
) -> None:
    """A store mongod 8.2.11 wrote, with indexes a `collMod` changed, opened
    by `secantusd-mdb`: the same specs in the same field order, `dropIndexes:
    "*"` in build order, and a `collMod` of its own moves one to the end."""
    seen: dict[str, Any] = {}

    def setup(client: Any) -> None:
        _collmodded_indexes(client)
        seen["catalog"] = _catalog_specs(client)

    store = _mongod_store(tmp_path, setup)
    with rust_mongo_server(store) as rust:
        assert _catalog_specs(rust) == seen["catalog"]
        assert [i["name"] for i in rust["app"]["c"].list_indexes()] == [
            "_id_",
            "a_1",
            "b_1",
            "d_1",
            "e_1",
        ]
    # This server's own `collMod` of what mongod wrote unhides `a_1` (its
    # field goes) and moves it to the end of the drop order.
    with rust_mongo_server(store) as rust:
        rust["app"].command("collMod", "c", index={"name": "a_1", "hidden": False})
        assert _catalog_specs(rust)[1] == [
            ("v", 2),
            ("unique", True),
            ("key", {"a": 1}),
            ("name", "a_1"),
        ]
        with rust["app"].watch(show_expanded_events=True, max_await_time_ms=200) as stream:
            rust["app"]["c"].drop_indexes()
            names = []
            end = time.monotonic() + 10
            while len(names) < 4 and time.monotonic() < end:
                ev = stream.try_next()
                if ev is not None and ev["operationType"] == "dropIndexes":
                    names.append(ev["operationDescription"]["indexes"][0]["name"])
        assert names == ["b_1", "d_1", "e_1", "a_1"]


@pytest.mark.backlog("RM-735")
def test_a_restart_raises_the_election_term(tmp_path: Path) -> None:
    """mongod 8.2.11 (measured 2026-11-20,
    `tests/fixtures/storage_model/mongod-8.2.11-replset-state.json`): the
    election document's `term` is 1 at initiation and 2 after the first clean
    restart of the store."""
    store = tmp_path / "store"
    with rust_mongo_server(store) as client:
        assert client.local["replset.election"].find_one()["term"] == 1
    with rust_mongo_server(store) as client:
        assert client.local["replset.election"].find_one()["term"] == 2


@pytest.mark.backlog_fixed("RM-736")
def test_the_truncate_after_point_follows_the_oplog(tmp_path: Path) -> None:
    """mongod 8.2.11 (measured 2026-11-20): two seconds after a write,
    `local.replset.oplogTruncateAfterPoint` is the timestamp of the oplog's
    last entry."""
    with rust_mongo_server(tmp_path / "store") as client:
        client.t.x.insert_many([{"i": i} for i in range(5)])
        time.sleep(2)
        point = client.local["replset.oplogTruncateAfterPoint"].find_one()
        last = next(client.local["oplog.rs"].find().sort("$natural", -1).limit(1))
        assert point["oplogTruncateAfterPoint"] == last["ts"]


def _truncation_shape() -> Any:
    """The writes of the truncation comparison: one with the default write
    concern (`w: "majority"`, journaled: its wait moves the point), then
    two at `w: 1, j: false`, then a second for the journal flusher."""

    def run(client: pymongo.MongoClient, pause: bool) -> None:
        coll = client.app.c
        coll.insert_one({"_id": "A"})
        if pause:
            client.admin.command(
                "configureFailPoint", "pauseJournalFlusherBeforeFlush", mode="alwaysOn"
            )
            time.sleep(0.3)
        w1 = coll.with_options(write_concern=pymongo.WriteConcern(w=1, j=False))
        w1.insert_one({"_id": "B"})
        w1.insert_one({"_id": "C"})
        time.sleep(1)

    return run


def _after_a_kill(server: str, tmp_path: Path, pause: bool) -> tuple:
    """The truncation comparison's shape on `server` (`"mongod"` or
    `"secantus"`), SIGKILL, a restart: the documents left and the oplog's
    entries for them."""
    run = _truncation_shape()
    store = tmp_path / server / "store"
    store.mkdir(parents=True)
    if server == "mongod":
        port = _free_port()
        proc, client = _mongod_on(store, port, "rs0", "--setParameter", "enableTestCommands=1")
        assert client is not None
        try:
            _wait_primary(client, f"127.0.0.1:{port}")
            run(client, pause)
        finally:
            proc.send_signal(signal.SIGKILL)
            proc.wait()
            client.close()
        proc, client = _mongod_on(store, port, "rs0")
        assert client is not None
        try:
            with _mongod_answers(store):
                _until_primary(client)
                return _survivors(client)
        finally:
            _stop_mongod(proc, client)
    _killed_secantus_store(store, lambda c: run(c, pause), "--enable-test-commands")
    with rust_mongo_server(store) as client:
        return _survivors(client)


def _survivors(client: pymongo.MongoClient) -> tuple:
    return (
        sorted(d["_id"] for d in client.app.c.find()),
        [e["o"]["_id"] for e in client.local["oplog.rs"].find({"ns": "app.c", "op": "i"})],
    )


@pytest.mark.backlog_fixed("RM-736")
@pytest.mark.parametrize("pause", [True, False], ids=["flusher-paused", "flusher-running"])
def test_recovery_truncates_the_oplog_after_the_point_as_mongod(
    tmp_path: Path, pause: bool
) -> None:
    """RM-736 (decided by Joe, 2026-11-22: "Truncate as mongod"). A write
    acknowledged at `w: 1, j: false` is in the journal file a SIGKILL leaves,
    but mongod 8.2.11's recovery truncates the oplog after
    `oplogTruncateAfterPoint`, which only its journal flusher (every 100 ms)
    and a journaled write's wait move: with the flusher paused
    (`pauseJournalFlusherBeforeFlush`) the two `w: 1` writes are gone after
    the restart, entries and documents ("Removing unapplied oplog entries
    after oplogTruncateAfterPoint", 21557), and the journaled one stays; with
    it running, a second later, all three stay (measured 2026-11-22). This
    server's recovery replayed every entry the journal held until phase 7c;
    now it gives mongod's answer in both shapes."""
    expected = (["A"], ["A"]) if pause else (["A", "B", "C"], ["A", "B", "C"])
    assert _after_a_kill("mongod", tmp_path, pause) == expected
    assert _after_a_kill("secantus", tmp_path, pause) == expected


# ------------------------------------------------- storage-model phase 7 --
# Found making `secantusd-mdb`'s stores ones mongod 8.2.11 opens, and
# measured against it (2026-11-21). Each forward test writes a store with
# `secantusd-mdb`, stops it, and starts mongod on it as the set's one member.


@contextlib.contextmanager
def _mongod_on_secantus_store(
    tmp_path: Path, setup: Any, kill: bool = False
) -> Iterator[pymongo.MongoClient]:
    """`setup(client)` on a new `secantusd-mdb` store, a clean stop (or a
    SIGKILL with `kill`), then mongod 8.2.11 on the store as its set's one
    member, primary: its client. A refusal is an assertion with mongod's log."""
    store = tmp_path / "store"
    if kill:
        port = _killed_secantus_store(store, setup)
    else:
        with rust_mongo_server(store) as client:
            setup(client)
            port = client.address[1]
    proc, mongod = _mongod_on(store, port, SECANTUS_SET)
    try:
        assert mongod is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(mongod)
            yield mongod
    finally:
        _stop_mongod(proc, mongod)


def _killed_secantus_store(store: Path, setup: Any, *extra: str) -> int:
    """`setup(client)` on a new `secantusd-mdb` store, then SIGKILL: the port
    it bound."""
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store), *extra],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        m = None
        for _ in range(20):
            m = _RS_BANNER.search(proc.stdout.readline())
            if m:
                break
        assert m, "secantusd-mdb did not start"
        with pymongo.MongoClient(m.group(1), int(m.group(2))) as client:
            setup(client)
        return int(m.group(2))
    finally:
        proc.send_signal(signal.SIGKILL)
        proc.wait()


def _validation(mongod: pymongo.MongoClient, db: str, coll: str) -> tuple:
    result = mongod[db].command("validate", coll, full=True)
    return (result["valid"], result.get("errors"), result.get("warnings"))


@pytest.mark.backlog_fixed("RM-741")
def test_mongod_validates_a_2d_index_secantus_wrote(tmp_path: Path) -> None:
    """mongod 8.2.11 keys a `2d` index by the point's GeoHash at the index's
    `bits`, x then y interleaved in the high bits, as BinData subtype 128
    (phase 0b: `[-10, 20]` is `6ad4ad4ad4ad4000`). This server keyed it by a
    cell of its own (y first, in the low bits, BinData subtype 0), so
    mongod's `validate` found every entry missing and one extra each; it now
    writes mongod's."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.k.insert_many([{"_id": 1, "flat": [1.5, 2.5]}, {"_id": 2, "flat": [-10, 20]}])
        c.app.k.create_index([("flat", "2d")], name="flat_2d")

    with _mongod_on_secantus_store(tmp_path, setup) as mongod:
        assert _validation(mongod, "app", "k") == (True, [], [])


@pytest.mark.backlog_fixed("RM-742")
def test_mongod_validates_a_2dsphere_index_secantus_wrote(tmp_path: Path) -> None:
    """mongod 8.2.11 keys a point of a `2dsphere` (version 3) index by one S2
    cell id as a NumberLong (phase 0b: two points, two entries,
    `201384b1db336fa071` and `328fccdc3d444a7656`). This server wrote its
    own covering (levels 4 to 16, every ancestor of each cell, BinData), so
    mongod's `validate` found the entries missing and dozens extra; it now
    writes mongod's."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.k.insert_many(
            [
                {"_id": 1, "loc": {"type": "Point", "coordinates": [-73.97, 40.77]}},
                {"_id": 2, "loc": {"type": "Point", "coordinates": [2.35, 48.86]}},
            ]
        )
        c.app.k.create_index([("loc", "2dsphere")], name="loc_2dsphere")

    with _mongod_on_secantus_store(tmp_path, setup) as mongod:
        assert _validation(mongod, "app", "k") == (True, [], [])


@pytest.mark.backlog_fixed("RM-743")
def test_mongod_validates_a_wildcard_index_secantus_wrote(tmp_path: Path) -> None:
    """mongod 8.2.11 keys a wildcard index by `($_path, value)` for each leaf
    path under it (phase 0b: `{w: {x: 1, y: {z: 2}}}` gives `w.x`/1 and
    `w.y.z`/2). This server wrote one null entry per document, so mongod's
    `validate` found the entries missing and the nulls extra; it now writes
    mongod's, multikey metadata keys included."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.k.insert_many([{"_id": 1, "w": {"x": 1, "y": {"z": 2}}}, {"_id": 2, "w": {"x": "s"}}])
        c.app.k.create_index([("w.$**", 1)], name="w_wildcard")

    with _mongod_on_secantus_store(tmp_path, setup) as mongod:
        assert _validation(mongod, "app", "k") == (True, [], [])


def _pt(x: float, y: float) -> dict:
    return {"type": "Point", "coordinates": [x, y]}


_SQUARE = {"type": "Polygon", "coordinates": [[[0, 0], [3, 0], [3, 3], [0, 3], [0, 0]]]}
_BIG = {
    "type": "Polygon",
    "coordinates": [[[-100, -60], [100, -60], [100, 60], [-100, 60], [-100, -60]]],
}

# Each case: collection -> (documents, key pattern, index options). Measured on
# mongod 8.2.11 (2026-11-23) by reading the index tables it wrote: every one
# is a store `validate` passes on mongod.
_GEO_CASES = {
    "flat_many": (
        [
            {"_id": 1, "loc": [1.5, 2.5]},
            {"_id": 2, "loc": [[1, 1], [30, 30]]},
            {"_id": 3, "loc": {"x": -50.25, "y": 80}},
            {"_id": 4},
        ],
        [("loc", "2d")],
        {},
    ),
    "flat_compound": (
        [
            {"_id": 1, "loc": [1, 1], "z": [2, 1, 2]},
            {"_id": 2, "loc": [1, 1]},
            {"_id": 3, "loc": [1, 1], "z": {"k": [7, 8]}},
            {"_id": 4, "loc": [1, 1], "z": [{"q": 1}, {"q": 2}]},
            {"_id": 5, "loc": [1, 1], "z": []},
        ],
        [("loc", "2d"), ("z", 1), ("q", -1)],
        {},
    ),
    "flat_bits": (
        [{"_id": 1, "loc": [3, -4]}, {"_id": 2, "loc": [-10, 10]}],
        [("loc", "2d")],
        {"bits": 10, "min": -10, "max": 10},
    ),
    "flat_bits32": (
        [{"_id": 1, "loc": [3, -4]}],
        [("loc", "2d")],
        {"bits": 32, "min": -100, "max": 100},
    ),
    "sphere_shapes": (
        [
            {"_id": 1, "loc": _pt(1.5, 2.5)},
            {"_id": 2, "loc": [10, 20]},
            {"_id": 3, "loc": {"type": "MultiPoint", "coordinates": [[1, 1], [2, 2]]}},
            {"_id": 4, "loc": _SQUARE},
            {"_id": 5, "loc": {"type": "LineString", "coordinates": [[0, 0], [1, 1]]}},
            {"_id": 6, "loc": _BIG},
            {"_id": 7},
            {"_id": 8, "loc": _pt(-179.9, 89.9)},
        ],
        [("loc", "2dsphere")],
        {},
    ),
    "sphere_collection": (
        [
            {
                "_id": 1,
                "loc": {
                    "type": "GeometryCollection",
                    "geometries": [
                        _pt(1, 1),
                        {"type": "LineString", "coordinates": [[0, 0], [1, 1]]},
                    ],
                },
            },
            {
                "_id": 2,
                "loc": {
                    "type": "MultiPolygon",
                    "coordinates": [
                        [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]],
                        [[[5, 5], [6, 5], [6, 6], [5, 6], [5, 5]]],
                    ],
                },
            },
            {
                "_id": 3,
                "loc": {
                    "type": "Polygon",
                    "coordinates": [
                        [[0, 0], [3, 0], [3, 3], [0, 3], [0, 0]],
                        [[1, 1], [1, 2], [2, 2], [2, 1], [1, 1]],
                    ],
                },
            },
        ],
        [("loc", "2dsphere")],
        {},
    ),
    "sphere_levels": (
        [{"_id": 1, "loc": _SQUARE}, {"_id": 2, "loc": _BIG}],
        [("loc", "2dsphere")],
        {"finestIndexedLevel": 10, "coarsestIndexedLevel": 6},
    ),
    "sphere_geo_first": (
        [
            {"_id": 1, "loc": _pt(1, 1), "z": [1, 2]},
            {"_id": 2, "loc": _pt(1, 1)},
            {"_id": 3, "loc": _pt(1, 1), "z": "s"},
            {"_id": 4, "loc": _SQUARE, "z": 4},
        ],
        [("loc", "2dsphere"), ("z", 1)],
        {},
    ),
    "sphere_geo_last": (
        [
            {"_id": 1, "loc": _pt(1, 1), "z": [1, 2]},
            {"_id": 2, "z": 2},
            {"_id": 3, "loc": [], "z": 3},
        ],
        [("z", 1), ("loc", "2dsphere")],
        {},
    ),
    "sphere_nested": (
        [{"_id": 1, "a": {"loc": _SQUARE}}, {"_id": 2, "a": {"loc": _pt(1, 1)}}],
        [("a.loc", "2dsphere")],
        {},
    ),
}

_WILDCARD_CASES = {
    "path": (
        [
            {"_id": 1, "w": {"x": 1, "y": [1, [2, 3]]}},
            {"_id": 2, "w": [{"a": 1}, {"a": 2}]},
            {"_id": 3, "w": [[1, 2], [3]]},
            {"_id": 4, "w": {"a": {"b": [{"c": 1}]}}},
            {"_id": 5, "w": 5, "o": 9},
            {"_id": 6, "w": {}},
            {"_id": 7, "w": []},
            {"_id": 8, "w": None},
            {"_id": 9},
        ],
        [("w.$**", 1)],
        {},
    ),
    "descending": (
        [{"_id": 1, "w": {"x": 1, "y": [1, [2, 3]]}}, {"_id": 2, "w": [{"a": 1}, {"a": 2}]}],
        [("w.$**", -1)],
        {},
    ),
    "all_fields": (
        [
            {"_id": 1, "a": None, "b": {"d": 1.5}, "c": [], "d": [[]], "e": [{}], "f": [None]},
            {"_id": 2, "a": {"b": {}}, "s": "Abc", "n": {"$numberDecimal": "1"}},
            {"_id": 3, "a": [1, 1, 2]},
        ],
        [("$**", 1)],
        {},
    ),
    "collated": (
        [{"_id": 1, "a": 1, "b": [1, 2], "s": "Abc"}],
        [("$**", 1)],
        {"collation": {"locale": "en", "strength": 2}},
    ),
    "projection_in": (
        [{"_id": 1, "a": 1, "b": [1, 2], "c": {"d": [3]}}],
        [("$**", 1)],
        {"wildcardProjection": {"a": 1, "c": 1}},
    ),
    "projection_out": (
        [{"_id": 1, "a": 1, "b": {"c": 2}}],
        [("$**", 1)],
        {"wildcardProjection": {"b": 0}},
    ),
    "projection_id": (
        [{"_id": 1, "a": 1, "b": {"c": 2}, "z": 1}],
        [("$**", -1)],
        {"wildcardProjection": {"b": 1, "_id": 1}},
    ),
    "compound": (
        [{"_id": 1, "a": 1, "b": [1, 2]}, {"_id": 2, "a": 2, "b": 3}],
        [("a", 1), ("$**", 1)],
        {"wildcardProjection": {"b": 1}},
    ),
    "partial": (
        [{"_id": 1, "a": 1, "b": 1}, {"_id": 2, "a": 3, "b": [1]}],
        [("$**", 1)],
        {"partialFilterExpression": {"a": {"$gt": 1}}},
    ),
}


def _validate_cases(tmp_path: Path, cases: dict) -> dict:
    def setup(c: pymongo.MongoClient) -> None:
        for name, (docs, keys, opts) in cases.items():
            c.app[name].insert_many(docs)
            c.app[name].create_index(keys, name="i", **opts)

    with _mongod_on_secantus_store(tmp_path, setup) as mongod:
        return {name: _validation(mongod, "app", name) for name in cases}


@pytest.mark.backlog_fixed("RM-741")
def test_mongod_validates_each_kind_of_2d_index_secantus_wrote(tmp_path: Path) -> None:
    """RM-741: `2d` indexes of every shape this server writes (several
    locations, compound with array fields, the `bits` / `min` / `max`
    options) hold the keys mongod 8.2.11 makes."""
    cases = {n: c for n, c in _GEO_CASES.items() if n.startswith("flat_")}
    assert _validate_cases(tmp_path, cases) == {name: (True, [], []) for name in cases}


@pytest.mark.backlog_fixed("RM-742")
def test_mongod_validates_each_kind_of_2dsphere_index_secantus_wrote(tmp_path: Path) -> None:
    """RM-742: `2dsphere` indexes of every shape this server writes (every
    GeoJSON kind, the covering levels, the geo field first or last or
    nested, array fields beside it) hold the keys, and the multikey flag and
    paths, mongod 8.2.11 makes."""
    cases = {n: c for n, c in _GEO_CASES.items() if n.startswith("sphere_")}
    assert _validate_cases(tmp_path, cases) == {name: (True, [], []) for name in cases}


@pytest.mark.backlog_fixed("RM-743")
def test_mongod_validates_each_kind_of_wildcard_index_secantus_wrote(tmp_path: Path) -> None:
    """RM-743: wildcard indexes of every shape this server writes (a path,
    descending, arrays and nested arrays, empty objects and arrays, a
    collation, an inclusion or exclusion projection, compound, partial) hold
    the keys and the multikey metadata keys mongod 8.2.11 makes."""
    assert _validate_cases(tmp_path, _WILDCARD_CASES) == {
        name: (True, [], []) for name in _WILDCARD_CASES
    }


@pytest.mark.backlog_fixed("RM-744")
def test_mongod_finds_the_users_and_roles_secantus_made(tmp_path: Path) -> None:
    """mongod 8.2.11 keeps users and roles as documents of
    `admin.system.users` and `admin.system.roles` (phase 0b). This server
    kept them in tables of its own (`secantus_users`, `secantus_roles`), so
    mongod on its store had neither the user nor the role; it now keeps
    them where mongod does."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.command(
            "createRole",
            "r1",
            privileges=[{"resource": {"db": "app", "collection": "c"}, "actions": ["find"]}],
            roles=[],
        )
        c.app.command("createUser", "u1", pwd="pw", roles=["r1"], mechanisms=["SCRAM-SHA-256"])

    with _mongod_on_secantus_store(tmp_path, setup) as mongod:
        users = [u["user"] for u in mongod.app.command("usersInfo", "u1")["users"]]
        roles = [r["role"] for r in mongod.app.command("rolesInfo", "r1")["roles"]]
        assert (users, roles) == (["u1"], ["r1"])


@pytest.mark.backlog_fixed("RM-745")
def test_a_views_ddl_is_logged_as_mongod_logs_it() -> None:
    """mongod 8.2.11 logs a view's DDL as writes to `system.views`, not as
    commands (measured 2026-11-21): the first view's database gets
    `{op: c, o: {create: "system.views", idIndex}}`, then each create is an
    insert of the definition, a `collMod` an update replacing it and a drop
    its delete. This server logged `create` (with its internal `viewPipeline`
    and an `idIndex`), `collMod` and `drop` commands naming the view, which
    mongod replaying a killed store's oplog refused ("'viewPipeline' is not
    a valid collection option"), aborting its recovery (RM-745)."""
    import tempfile

    with tempfile.TemporaryDirectory() as home, rust_mongo_server(Path(home) / "s") as c:
        c.app.c.insert_one({"_id": 1})
        start = next(c.local["oplog.rs"].find().sort("$natural", -1).limit(1))["ts"]
        c.app.command("create", "v1", viewOn="c", pipeline=[{"$match": {"a": 1}}])
        c.app.command("collMod", "v1", pipeline=[{"$match": {"a": 2}}])
        c.app.v1.drop()
        entries = [
            (e["op"], e["ns"], e["o"])
            for e in c.local["oplog.rs"].find({"ts": {"$gt": start}, "ns": {"$regex": "^app"}})
        ]
    assert entries == [
        ("c", "app.$cmd", {"create": "system.views", "idIndex": ID_INDEX_SPEC}),
        (
            "i",
            "app.system.views",
            {"_id": "app.v1", "viewOn": "c", "pipeline": [{"$match": {"a": 1}}]},
        ),
        (
            "u",
            "app.system.views",
            {"_id": "app.v1", "viewOn": "c", "pipeline": [{"$match": {"a": 2}}]},
        ),
        ("d", "app.system.views", {"_id": "app.v1"}),
    ]


@pytest.mark.backlog_fixed("RM-745")
def test_mongod_opens_a_killed_store_whose_workload_made_views(tmp_path: Path) -> None:
    """The kill probe RM-745 was measured with (2026-11-22): views made,
    changed and dropped, then SIGKILL. mongod 8.2.11 replays the oplog after
    the stable checkpoint, and a view's `create` command carrying
    `viewPipeline` aborted its recovery. Logged as `system.views` writes,
    the views replay as mongod's own do, and the live one reads back."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.c.insert_many([{"_id": i, "a": i % 3} for i in range(30)])
        c.app.command("create", "v1", viewOn="c", pipeline=[{"$match": {"a": 1}}])
        c.app.command("create", "gone", viewOn="c", pipeline=[])
        c.app.command("collMod", "v1", viewOn="c", pipeline=[{"$match": {"a": 2}}])
        c.app.gone.drop()

    with _mongod_on_secantus_store(tmp_path, setup, kill=True) as mongod:
        names = sorted(
            (i["name"], i["type"]) for i in mongod.app.list_collections() if i["name"] != "c"
        )
        assert names == [("system.views", "collection"), ("v1", "view")]
        assert mongod.app.v1.count_documents({}) == 10


@pytest.mark.backlog_fixed("RM-745")
def test_a_killed_store_replays_its_views(tmp_path: Path) -> None:
    """The same workload, SIGKILLed, reopened by this server: its recovery
    replays the `system.views` writes as the views' DDL (RM-745)."""
    store = tmp_path / "store"

    def setup(c: pymongo.MongoClient) -> None:
        c.app.c.insert_many([{"_id": i, "a": i % 3} for i in range(30)])
        c.app.command("create", "v1", viewOn="c", pipeline=[{"$match": {"a": 1}}])
        c.app.command("create", "gone", viewOn="c", pipeline=[])
        c.app.command("collMod", "v1", viewOn="c", pipeline=[{"$match": {"a": 2}}])
        c.app.gone.drop()

    _killed_secantus_store(store, setup)
    with rust_mongo_server(store) as c:
        views = {i["name"]: i["options"] for i in c.app.list_collections() if i["type"] == "view"}
        assert views == {"v1": {"viewOn": "c", "pipeline": [{"$match": {"a": 2}}]}}
        assert c.app.v1.count_documents({}) == 10
        defs = list(c.app.system.views.find())
        assert defs == [{"_id": "app.v1", "viewOn": "c", "pipeline": [{"$match": {"a": 2}}]}]


#: The `_id` index spec an oplog `create` entry carries.
ID_INDEX_SPEC = {"v": 2, "key": {"_id": 1}, "name": "_id_"}


@pytest.mark.backlog_fixed("RM-746")
def test_mongod_opens_a_killed_secantus_store(tmp_path: Path) -> None:
    """mongod 8.2.11's stable timestamp is always an oplog entry's (its
    commit point), so after a crash its checkpoint's recovery timestamp is at
    or below the oplog's last entry, and its `oplogTruncateAfterPoint` names
    an entry it moved there before the journal's last sync. A store killed
    while this server's stable timestamp was its all-committed point (moved
    past the last entry by timestamps minted without one) made mongod
    truncate, force the stable and oldest timestamps back, then abort on its
    next `oldest_timestamp` (invariant in `WiredTigerKVEngine::
    setOldestTimestamp`; measured 2026-11-21). Since phase 7b (RM-746) the
    stable timestamp and the truncate point are entries', and mongod opening
    the killed store replays the oplog after its stable checkpoint and serves
    every document."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.c.insert_many([{"_id": i, "a": i % 3} for i in range(100)])
        c.app.c.create_index("a", name="a_1")
        time.sleep(1)

    with _mongod_on_secantus_store(tmp_path, setup, kill=True) as mongod:
        time.sleep(2)  # mongod moves its oldest timestamp as it serves
        assert mongod.app.c.count_documents({}) == 100
        # A table recovery wrote to is "actively in use" until a checkpoint
        # takes it: mongod's own killed store warns so too, and validates
        # clean after an `fsync` (measured 8.2.11, 2026-11-22).
        mongod.admin.command("fsync")
        assert _validation(mongod, "app", "c") == (True, [], [])


def _mongod_store(tmp_path: Path, setup: Any, kill: bool = False) -> Path:
    """`setup(client)` on a new mongod 8.2.11 one-member set, then a clean
    stop (or SIGKILL with `kill`, its journal and oplog recovery pending)."""
    return _mongod_store_at(tmp_path, setup, kill)[0]


def _mongod_store_at(
    tmp_path: Path, setup: Any, kill: bool = False, *extra: str
) -> tuple[Path, int]:
    """`_mongod_store`, and the port its one member is configured at."""
    store = tmp_path / "store"
    store.mkdir()
    port = _free_port()
    proc, client = _mongod_on(store, port, "rs0", *extra)
    assert client is not None, "mongod did not start on an empty directory"
    try:
        host, port = client.address or ("127.0.0.1", 0)
        _wait_primary(client, f"{host}:{port}")
        with pymongo.MongoClient(
            host, port, replicaSet="rs0", serverSelectionTimeoutMS=10_000
        ) as rs:
            setup(rs)
        if kill:
            time.sleep(1)
            proc.send_signal(signal.SIGKILL)
            proc.wait()
    finally:
        if not kill:
            _stop_mongod(proc, client)
        elif client is not None:
            client.close()
    return store, port


def _secantus_open(store: Path) -> tuple[str, str]:
    """`secantusd-mdb` on `store`: `("served", banner)` once it listens (then
    stopped), or `("refused", what it printed)` when it exits instead."""
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    assert proc.stdout is not None
    lines = []
    try:
        for line in proc.stdout:
            lines.append(line)
            if _RS_BANNER.search(line):
                return ("served", line)
        return ("refused", "".join(lines))
    finally:
        proc.terminate()
        proc.wait(timeout=30)


#: The index kinds this server does not key as mongod does (refused by name
#: when a store holds one), which the servable scenario drops.
_MONGOD_ONLY_KINDS = ("loc_2dsphere", "flat_2d", "body_text", "w_wildcard")

#: What both servers rewrite as they run, whoever wrote the store: each
#: server's logical session cache refreshes the sessions it has seen into
#: `config.system.sessions`.
_LIVE_NAMESPACES = {"config.system.sessions"}


def _servable_scenario(rs: pymongo.MongoClient) -> None:
    """Phase 0b's scenario on mongod (every collection and index kind,
    capped, clustered, collated, pre-images, a view, a drop, a rename, a
    transaction), less what this server refuses by name in a store: the index
    kinds it keys otherwise (RM-741 to RM-743, text) and the
    time-series collection (RM-754). Its user and role stay: this server
    keeps them in `admin.system.users` / `system.roles`, as mongod does
    (RM-744)."""
    from tools.probes.mongod_store_layout import run_scenario

    run_scenario(rs, strict=True)
    for name in _MONGOD_ONLY_KINDS:
        rs.app.kinds.drop_index(name)
    rs.app.ts.drop()


def _comparable(contents: dict) -> dict:
    return {ns: v for ns, v in contents.items() if ns not in _LIVE_NAMESPACES}


@pytest.mark.backlog_fixed("RM-723")
def test_secantus_serves_the_phase_0b_store_mongod_wrote(tmp_path: Path) -> None:
    """Storage-model phase 7c, the reverse of
    `test_mongod_opens_the_phase_0b_store`: mongod 8.2.11 writes phase 0b's
    scenario (less what this server refuses by name) and stops cleanly;
    `secantusd-mdb` opens the store and serves every document and every
    hinted index walk byte for byte as mongod served them, resumes a change
    stream from a token mongod issued, and writes to it; mongod opens the
    store again, `validate({full: true})` passes on every collection and it
    reads what this server wrote."""
    served: dict = {}
    token: Any = None

    def setup(rs: pymongo.MongoClient) -> None:
        nonlocal served, token
        _servable_scenario(rs)
        with rs.app.c.watch() as stream:
            rs.app.c.insert_one({"_id": 500, "a": 1, "b": "x500"})
            token = next(stream)["_id"]
        rs.app.c.insert_one({"_id": 501, "a": 2, "b": "x501"})
        rs.app.c.update_one({"_id": 501}, {"$set": {"a": 3}})
        served = _store_contents(rs)

    store, _ = _mongod_store_at(tmp_path, setup)
    with rust_mongo_server(store) as rust:
        assert _comparable(_store_contents(rust)) == _comparable(served)
        assert rust.admin.command("hello").get("setName") == "rs0"
        with rust.app.c.watch(resume_after=token) as stream:
            events = [next(stream), next(stream)]
        assert [(e["operationType"], e["documentKey"]) for e in events] == [
            ("insert", {"_id": 501}),
            ("update", {"_id": 501}),
        ]
        rust.app.c.insert_one({"_id": 700, "a": 7, "b": "x700", "t": [7, 8]})
        rust.app.made_here.insert_many([{"_id": i, "y": i % 2} for i in range(4)])
        rust.app.made_here.create_index("y", name="y_1")
        rust.app.renamed.drop()
        written = _store_contents(rust)
        port = rust.address[1]
    proc, mongod = _mongod_on(store, port, "rs0")
    try:
        assert mongod is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(mongod)
            namespaces = [
                (db, info["name"])
                for db in mongod.list_database_names()
                for info in mongod[db].list_collections()
                if info.get("type") != "view"
            ]
            findings = {
                f"{db}.{name}": _validation_findings(mongod, db, name) for db, name in namespaces
            }
            assert {ns: f for ns, f in findings.items() if f is not None} == {}
            assert _comparable(_store_contents(mongod)) == _comparable(written)
    finally:
        _stop_mongod(proc, mongod)


def _after_the_stable_checkpoint(rs: pymongo.MongoClient) -> None:
    """Writes made after mongod's stable checkpoint (an `fsync`), which a
    restart after a kill replays from the oplog: a `create` with `idIndex`
    (an implicit one), a two-phase index build (`startIndexBuild`,
    `commitIndexBuild` and their `config.system.indexBuilds` writes), a
    transaction (`applyOps`), updates (`$v: 2` diffs), a delete, an index
    drop and a capped insert."""
    rs.admin.command("fsync")
    rs.app.after.insert_many([{"_id": i, "z": i % 2} for i in range(20)])
    rs.app.after.create_index("z", name="z_1")
    with rs.start_session() as s:
        s.with_transaction(
            lambda sess: (
                rs.app.c.insert_one({"_id": 98, "a": 8, "b": "x98"}, session=sess),
                rs.app.after.update_one({"_id": 1}, {"$set": {"z": 9}}, session=sess),
            )
        )
    rs.app.c.update_many({"a": 1}, {"$inc": {"n": 1}})
    rs.app.c.delete_one({"_id": 5})
    rs.app.ks.drop_index("v_-1")
    rs.app.capped.insert_one({"_id": 10})


@pytest.mark.backlog_fixed("RM-723")
def test_secantus_opens_a_killed_mongod_store(tmp_path: Path) -> None:
    """A store mongod 8.2.11 was killed on (SIGKILL: journal and oplog
    recovery pending), opened by `secantusd-mdb`, serves what mongod's own
    restart on a copy of the same killed store serves, document for document
    and index walk for index walk: WiredTiger recovers the logged tables,
    the oplog is truncated after `oplogTruncateAfterPoint`, and every entry
    after the stable checkpoint is replayed (`ReplicationRecovery`). Then
    mongod opens what this server wrote, and validates it."""

    def setup(rs: pymongo.MongoClient) -> None:
        _servable_scenario(rs)
        _after_the_stable_checkpoint(rs)

    store, port = _mongod_store_at(tmp_path, setup, kill=True)
    copy = tmp_path / "copy" / "store"
    shutil.copytree(store, copy)
    proc, mongod = _mongod_on(copy, port, "rs0")
    try:
        assert mongod is not None, "mongod refused its own killed store"
        with _mongod_answers(copy):
            _until_primary(mongod)
            restarted = _store_contents(mongod)
    finally:
        _stop_mongod(proc, mongod)
    with rust_mongo_server(store) as rust:
        assert _comparable(_store_contents(rust)) == _comparable(restarted)
        replayed = {
            (e["op"], next(iter(e.get("o", {})), None))
            for e in rust.local["oplog.rs"].find({"op": {"$in": ["c", "n"]}})
        }
        assert {("c", "create"), ("c", "startIndexBuild"), ("c", "commitIndexBuild")} <= replayed
        assert ("c", "applyOps") in replayed
        port = rust.address[1]
    proc, mongod = _mongod_on(store, port, "rs0")
    try:
        assert mongod is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(mongod)
            mongod.admin.command("fsync")
            assert _validation(mongod, "app", "after") == (True, [], [])
            assert _validation(mongod, "app", "c") == (True, [], [])
    finally:
        _stop_mongod(proc, mongod)


def _served_after_mongods_restart(store: Path, port: int, tmp_path: Path) -> dict:
    """What mongod 8.2.11 serves restarted on a copy of `store` (killed, or
    stopped), as `_store_contents`."""
    copy = tmp_path / "copy" / "store"
    shutil.copytree(store, copy)
    proc, mongod = _mongod_on(copy, port, "rs0")
    try:
        assert mongod is not None, "mongod refused its own store"
        with _mongod_answers(copy):
            _until_primary(mongod)
            return _store_contents(mongod)
    finally:
        _stop_mongod(proc, mongod)


@pytest.mark.backlog("RM-754")
def test_secantus_opens_a_mongod_time_series_collection(tmp_path: Path) -> None:
    """A store mongod 8.2.11 wrote holding a time-series collection (a view
    in `system.views` over `system.buckets.<name>`, a clustered collection
    with zstd blocks and its own catalog options, measured in phase 0b):
    `secantusd-mdb` serves its measurements as mongod does. It refuses the
    store by name instead: the registry rows of a time-series collection
    (its options, its buckets' catalog options and the indexes listed on it)
    are not rebuilt from mongod's catalog."""

    def setup(rs: pymongo.MongoClient) -> None:
        import datetime as dt

        rs.app.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        base = dt.datetime(2024, 1, 1, tzinfo=dt.timezone.utc)
        rs.app.ts.insert_many(
            [{"t": base + dt.timedelta(seconds=i), "m": i % 2, "v": i} for i in range(6)]
        )

    store, port = _mongod_store_at(tmp_path, setup)
    served = _served_after_mongods_restart(store, port, tmp_path)
    outcome, said = _secantus_open(store)
    assert outcome == "served", said
    with rust_mongo_server(store) as rust:
        assert _comparable(_store_contents(rust)) == _comparable(served)


@pytest.mark.backlog("RM-755")
def test_secantus_finishes_an_index_build_mongod_left(tmp_path: Path) -> None:
    """mongod 8.2.11 stopped cleanly during an index build (held by
    `hangAfterStartingIndexBuild`) leaves the index in its catalog with
    `ready: false` and its build in `config.system.indexBuilds`; its restart
    finishes the build. `secantusd-mdb` refuses the store by name ("whose
    build has not finished") instead of finishing it."""
    import threading

    def setup(rs: pymongo.MongoClient) -> None:
        rs.app.c.insert_many([{"_id": i, "a": i % 7} for i in range(200)])
        rs.admin.command("configureFailPoint", "hangAfterStartingIndexBuild", mode="alwaysOn")
        building = threading.Thread(target=_build_quietly, args=(rs,), daemon=True)
        building.start()
        deadline = time.monotonic() + 30
        while not list(rs.config["system.indexBuilds"].find()):
            assert time.monotonic() < deadline, "the build never started"
            time.sleep(0.1)

    store, port = _mongod_store_at(tmp_path, setup, False, "--setParameter", "enableTestCommands=1")
    served = _served_after_mongods_restart(store, port, tmp_path)
    assert "a_1" in served["app.c"], "mongod's restart did not finish the build"
    outcome, said = _secantus_open(store)
    assert outcome == "served", said
    with rust_mongo_server(store) as rust:
        assert _comparable(_store_contents(rust)) == _comparable(served)


def _build_quietly(rs: pymongo.MongoClient) -> None:
    with contextlib.suppress(pymongo.errors.PyMongoError):
        rs.app.c.create_index("a", name="a_1")


@pytest.mark.backlog("RM-756")
def test_secantus_opens_a_killed_mongod_store_whose_oplog_drops_what_it_refuses(
    tmp_path: Path,
) -> None:
    """A killed mongod 8.2.11 store whose stable checkpoint lists a `text`
    index that the oplog after it drops: mongod's restart replays the drop
    and serves the collection without it. `secantusd-mdb` judges the store
    at the checkpoint, before its replay, and refuses it by name."""

    def setup(rs: pymongo.MongoClient) -> None:
        rs.app.c.insert_many([{"_id": i, "h": f"word {i}"} for i in range(5)])
        rs.app.c.create_index([("h", "text")], name="h_text")
        rs.admin.command("fsync")
        rs.app.c.drop_index("h_text")
        rs.app.c.insert_one({"_id": 5, "h": "word 5"})

    store, port = _mongod_store_at(tmp_path, setup, kill=True)
    served = _served_after_mongods_restart(store, port, tmp_path)
    outcome, said = _secantus_open(store)
    assert outcome == "served", said
    with rust_mongo_server(store) as rust:
        assert _comparable(_store_contents(rust)) == _comparable(served)


@pytest.mark.backlog_fixed("RM-757")
def test_secantus_keeps_what_mongod_wrote_to_its_store(tmp_path: Path) -> None:
    """A store `secantusd-mdb` wrote and stopped, opened by mongod 8.2.11,
    which makes a collection and an index, writes to an existing collection
    (arrays, making an index multikey), changes a validator and drops a
    collection and an index; `secantusd-mdb` opening the store again serves
    what mongod served, byte for byte, index walks included. Until phase 7c
    it served its own registry: mongod's new collection was gone (its tables
    dropped at the open as orphans) and the dropped one was still served,
    from its drop-pending table (measured 2026-11-22)."""
    store = tmp_path / "store"
    with rust_mongo_server(store) as client:
        client.app.keep.insert_many([{"_id": i, "t": i} for i in range(3)])
        client.app.keep.create_index("t", name="t_1")
        client.app.keep.create_index("u", name="u_1")
        client.app.gone.insert_one({"_id": 1})
        port = client.address[1]
    proc, mongod = _mongod_on(store, port, SECANTUS_SET)
    try:
        assert mongod is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(mongod)
            mongod.app.made.insert_many([{"_id": i, "x": i % 2} for i in range(5)])
            mongod.app.made.create_index("x", name="x_1")
            mongod.app.keep.insert_one({"_id": 9, "t": [1, 2, 3]})
            mongod.app.keep.drop_index("u_1")
            mongod.app.command("collMod", "keep", validator={"t": {"$exists": True}})
            mongod.app.gone.drop()
            served = _store_contents(mongod)
            options = {i["name"]: i["options"] for i in mongod.app.list_collections()}
    finally:
        _stop_mongod(proc, mongod)
    with rust_mongo_server(store) as rust:
        assert _comparable(_store_contents(rust)) == _comparable(served)
        assert {i["name"]: i["options"] for i in rust.app.list_collections()} == options
        # The multikey index answers as mongod's does: one document, once.
        assert [d["_id"] for d in rust.app.keep.find({"t": {"$gt": 1}}, hint="t_1")] == [2, 9]


@pytest.mark.backlog_fixed("RM-758")
@pytest.mark.parametrize("server", ["mongod", "secantus"])
def test_a_collection_keeps_its_uuid_through_crash_recovery(tmp_path: Path, server: str) -> None:
    """A collection made after the stable checkpoint, then a SIGKILL: the
    restart replays its `create` entry, and the collection has the UUID the
    entry names (`ui`), as before the kill (mongod 8.2.11, measured
    2026-11-22). This server's replay made it with no UUID, so its catalog
    entry had none (mongod refused the recovered store, "MustDowngrade:
    Collection does not have UUID in KVCatalog") and the next write minted a
    new one."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.fresh.insert_one({"_id": 1})
        setup.uuid = next(c.app.list_collections(filter={"name": "fresh"}))["info"]["uuid"]

    store = tmp_path / server / "store"
    store.parent.mkdir(parents=True)
    if server == "mongod":
        store, port = _mongod_store_at(store.parent, setup, kill=True)
        proc, client = _mongod_on(store, port, "rs0")
        assert client is not None
        try:
            with _mongod_answers(store):
                _until_primary(client)
                after = next(client.app.list_collections(filter={"name": "fresh"}))
        finally:
            _stop_mongod(proc, client)
    else:
        _killed_secantus_store(store, setup)
        with rust_mongo_server(store) as client:
            after = next(client.app.list_collections(filter={"name": "fresh"}))
    assert after["info"]["uuid"] == setup.uuid


@pytest.mark.backlog_fixed("RM-760")
@pytest.mark.parametrize("server", ["mongod", "secantus"])
def test_crash_recovery_keeps_a_transactions_session_row(tmp_path: Path, server: str) -> None:
    """A transaction committed after the stable checkpoint, then a SIGKILL:
    the restart's replay of its `applyOps` entry writes its session's
    `config.transactions` row, `{_id, txnNum, lastWriteOpTime: {ts, t},
    lastWriteDate, state: "committed"}`, with the entry's timestamp and wall
    time (mongod 8.2.11, measured 2026-11-22 comparing its recovery with this
    server's in the phase 7c kill-stress probe). This server's replay rebuilt
    a retryable write's row but not a transaction's: after a crash the
    session had no row."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.t.insert_one({"_id": 0})
        with c.start_session() as s:
            s.with_transaction(lambda sess: c.app.t.insert_one({"_id": 1}, session=sess))
            setup.lsid = s.session_id

    store = tmp_path / server / "store"
    store.parent.mkdir(parents=True)
    if server == "mongod":
        store, port = _mongod_store_at(store.parent, setup, kill=True)
        proc, client = _mongod_on(store, port, "rs0")
        assert client is not None
        try:
            with _mongod_answers(store):
                _until_primary(client)
                rows = list(client.config.transactions.find({"_id.id": setup.lsid["id"]}))
                entry = client.local["oplog.rs"].find_one({"o.applyOps": {"$exists": True}})
        finally:
            _stop_mongod(proc, client)
    else:
        _killed_secantus_store(store, setup)
        with rust_mongo_server(store) as client:
            rows = list(client.config.transactions.find({"_id.id": setup.lsid["id"]}))
            entry = client.local["oplog.rs"].find_one({"o.applyOps": {"$exists": True}})
    assert [list(r) for r in rows] == [
        ["_id", "txnNum", "lastWriteOpTime", "lastWriteDate", "state"]
    ]
    row = rows[0]
    assert (row["txnNum"], row["state"], row["lastWriteOpTime"], row["lastWriteDate"]) == (
        entry["txnNumber"],
        "committed",
        {"ts": entry["ts"], "t": entry["t"]},
        entry["wall"],
    )


@pytest.mark.backlog_fixed("RM-723")
def test_secantus_refuses_a_set_of_two_members_by_name(tmp_path: Path) -> None:
    """A mongod store whose `local.system.replset` names two members is not
    one a single-node server can serve: `secantusd-mdb` refuses it when it
    opens it, naming the configuration, and writes nothing."""

    def setup(rs: pymongo.MongoClient) -> None:
        rs["app"]["c"].insert_one({"_id": 1})
        config = rs.admin.command("replSetGetConfig")["config"]
        config["members"].append({"_id": 1, "host": "127.0.0.1:1", "priority": 0, "votes": 0})
        config["version"] += 1
        rs.admin.command("replSetReconfig", config, force=True)

    store = _mongod_store(tmp_path, setup)
    outcome, said = _secantus_open(store)
    assert (outcome, "local.system.replset" in said, "2 members" in said) == (
        "refused",
        True,
        True,
    ), said


@pytest.mark.backlog_fixed("RM-723")
def test_secantus_refuses_an_index_kind_it_does_not_serve_by_name(tmp_path: Path) -> None:
    """A mongod store holding an index this server does not write as mongod
    does (here `text`, which it refuses to create) is refused when
    `secantusd-mdb` opens it, naming the index, never served without it."""

    def setup(rs: pymongo.MongoClient) -> None:
        rs["app"]["c"].insert_one({"_id": 1, "h": "x"})
        rs["app"]["c"].create_index([("h", "text")], name="h_text")

    store = _mongod_store(tmp_path, setup)
    outcome, said = _secantus_open(store)
    assert (outcome, "app.c" in said, "h_text" in said) == ("refused", True, True), said


# ------------------------------------------------ storage-model phase 7b --
# Found comparing the entries mongod 8.2.11 and `secantusd-mdb` log for the
# same writes (2026-11-22), working RM-746.


@pytest.mark.backlog_fixed("RM-747")
def test_a_read_only_transaction_logs_its_majority_no_op(tmp_path: Path) -> None:
    """mongod 8.2.11 commits a read-only transaction by logging a no-op naming
    the commit's write concern (measured 2026-11-22 and 2026-11-24, the
    implicit default and a client-supplied `w: 1`), so the commit waits on a
    write of its own. This server logged nothing for it."""
    from pymongo.write_concern import WriteConcern

    def logged(client, wc):
        start = next(client.local["oplog.rs"].find().sort("$natural", -1).limit(1))["ts"]
        with client.start_session() as session, session.start_transaction(write_concern=wc):
            client.app.c.find_one({"_id": 1}, session=session)
        return [
            (e["op"], e["ns"], e["o"])
            for e in client.local["oplog.rs"].find({"ts": {"$gt": start}})
        ]

    with rust_mongo_server(tmp_path / "store") as client:
        client.app.c.insert_one({"_id": 1})
        implicit = logged(client, None)
        w1 = logged(client, WriteConcern(w=1))
    assert implicit == [
        (
            "n",
            "",
            {
                "msg": 'read-only transaction with writeConcern { w: "majority", wtimeout: 0, '
                'provenance: "implicitDefault" }'
            },
        )
    ]
    assert w1 == [
        (
            "n",
            "",
            {
                "msg": "read-only transaction with writeConcern { w: 1, wtimeout: 0, "
                'provenance: "clientSupplied" }'
            },
        )
    ]


@pytest.mark.backlog_fixed("RM-748")
def test_the_initiation_logs_the_pre_images_collection(tmp_path: Path) -> None:
    """mongod 8.2.11's initiation logs the creation of
    `config.system.preimages` (`{op: "c", ns: "config.$cmd", o: {create:
    "system.preimages", clusteredIndex: ...}}`, measured 2026-11-22), as it
    logs every collection it creates then. This server created it
    unreplicated, with no entry, until RM-748 was fixed."""
    with rust_mongo_server(tmp_path / "store") as client:
        creates = [
            e["o"]["create"]
            for e in client.local["oplog.rs"].find({"op": "c", "ns": "config.$cmd"})
            if "create" in e["o"]
        ]
    assert "system.preimages" in creates, creates


def _geo_wildcard_docs() -> list[dict]:
    """The documents of the geo / wildcard hand-off store (RM-775)."""
    docs: list[dict] = []
    for i in range(60):
        # Jittered, so no two documents are at one distance: mongod's order at
        # a tie is its platform's heap (the Linux build is the exemplar).
        x = (i * 7) % 40 - 20 + ((i * 37) % 101) / 1009
        y = (i * 11) % 30 - 15 + ((i * 53) % 103) / 1013
        docs.append(
            {
                "_id": i,
                "t": i % 4,
                "flat": [x, y],
                "pt": {"type": "Point", "coordinates": [x + 0.5, y + 0.25]},
                "pts": {"type": "MultiPoint", "coordinates": [[x, y], [y / 2, x / 2]]},
                "places": [
                    {"loc": {"type": "Point", "coordinates": [x, y]}, "n": i},
                    {"loc": {"type": "Point", "coordinates": [y / 2, x / 2]}, "n": i + 1},
                ],
                "poly": {
                    "type": "Polygon",
                    "coordinates": [[[x, y], [x + 2, y], [x + 2, y + 2], [x, y + 2], [x, y]]],
                },
                "w": {"a": i % 5, "b": [i % 3, f"s{i % 4}"], "c": {"d": i % 7, "e": None}},
                "z": i % 9,
                "arr": [{"k": i % 6}, {"k": (i + 1) % 6}],
                "keep": i % 2 == 0,
            }
        )
    return docs


def _geo_wildcard_indexes(coll: Any) -> None:
    coll.create_index([("flat", "2d"), ("t", 1)], name="flat_t")
    coll.create_index(
        [("flat", "2d")],
        name="flat_b",
        bits=20,
        min=-100,
        max=100,
        partialFilterExpression={"keep": True},
    )
    coll.create_index([("pt", "2dsphere"), ("t", 1)], name="pt_t")
    coll.create_index([("t", 1), ("pts", "2dsphere")], name="t_pts")
    coll.create_index([("places.loc", "2dsphere"), ("t", -1)], name="pl_s")
    coll.create_index(
        [("poly", "2dsphere")], name="poly_s", partialFilterExpression={"t": {"$gte": 1}}
    )
    coll.create_index(
        [("poly", "2dsphere"), ("z", 1)],
        name="poly_lv",
        finestIndexedLevel=14,
        coarsestIndexedLevel=6,
    )
    coll.create_index([("$**", 1)], name="all_w")
    coll.create_index([("w.$**", 1)], name="w_w")
    coll.create_index(
        [("$**", 1)], name="proj_w", wildcardProjection={"w.a": 1, "arr.k": 1, "z": 1}
    )
    coll.create_index(
        [("$**", 1)],
        name="excl_w",
        wildcardProjection={"flat": 0, "pt": 0, "pts": 0, "poly": 0, "w": 0},
    )
    coll.create_index([("$**", 1)], name="part_w", partialFilterExpression={"keep": True})


def _sq(x: int, lo: int = 0) -> dict:
    return {
        "type": "Polygon",
        "coordinates": [[[lo, lo], [x, lo], [x, x], [lo, x], [lo, lo]]],
    }


#: (name, filter, hint, sort)
_GEO_QUERIES: list[tuple[str, dict, str | None, list | None]] = [
    ("near2d", {"flat": {"$near": [3, 4]}}, "flat_t", None),
    ("near2d_max", {"flat": {"$near": [3, 4], "$maxDistance": 12}, "t": 1}, "flat_t", None),
    ("nearSphere2d", {"flat": {"$nearSphere": [3, 4], "$maxDistance": 0.3}}, "flat_t", None),
    ("within_box", {"flat": {"$geoWithin": {"$box": [[-5, -5], [8, 9]]}}}, "flat_t", None),
    (
        "within_center",
        {"flat": {"$geoWithin": {"$center": [[0, 0], 9]}}, "t": {"$in": [0, 2]}},
        "flat_t",
        None,
    ),
    (
        "within_poly2d",
        {"flat": {"$geoWithin": {"$polygon": [[-9, -9], [12, -3], [3, 11]]}}},
        "flat_t",
        None,
    ),
    (
        "partial2d",
        {"flat": {"$geoWithin": {"$box": [[-20, -20], [20, 20]]}}, "keep": True},
        "flat_b",
        None,
    ),
    (
        "near_pt",
        {
            "pt": {
                "$near": {
                    "$geometry": {"type": "Point", "coordinates": [1, 2]},
                    "$maxDistance": 2_000_000,
                }
            }
        },
        "pt_t",
        None,
    ),
    (
        "nearSphere_pt_t",
        {
            "pt": {
                "$nearSphere": {
                    "$geometry": {"type": "Point", "coordinates": [1, 2]},
                    "$minDistance": 100_000,
                }
            },
            "t": 2,
        },
        "pt_t",
        None,
    ),
    ("within_sphere", {"pt": {"$geoWithin": {"$geometry": _sq(10, -10)}}}, "pt_t", None),
    ("within_centerSphere", {"pt": {"$geoWithin": {"$centerSphere": [[0, 0], 0.2]}}}, "pt_t", None),
    (
        "intersects_poly",
        {"poly": {"$geoIntersects": {"$geometry": _sq(9)}}, "t": {"$gte": 1}},
        "poly_s",
        None,
    ),
    (
        "intersects_pts",
        {"pts": {"$geoIntersects": {"$geometry": _sq(6, -6)}}, "t": 1},
        "t_pts",
        None,
    ),
    (
        "near_pts_array",
        {"pts": {"$near": {"$geometry": {"type": "Point", "coordinates": [5, 5]}}}, "t": 3},
        "t_pts",
        None,
    ),
    (
        "near_places",
        {"places.loc": {"$near": {"$geometry": {"type": "Point", "coordinates": [3, 3]}}}},
        "pl_s",
        None,
    ),
    ("within_places", {"places.loc": {"$geoWithin": {"$geometry": _sq(8, -8)}}}, "pl_s", None),
    (
        "intersects_lv",
        {"poly": {"$geoIntersects": {"$geometry": _sq(7, -3)}}, "z": {"$lt": 6}},
        "poly_lv",
        None,
    ),
    ("w_eq", {"w.a": 3}, "all_w", None),
    ("w_eq_arr", {"w.b": 2}, "all_w", None),
    ("w_str", {"w.b": "s1"}, "w_w", None),
    ("w_range", {"w.c.d": {"$gt": 2, "$lte": 5}}, "w_w", None),
    ("w_null", {"w.c.e": None}, "all_w", None),
    ("w_in", {"z": {"$in": [1, 4, 8]}}, "all_w", None),
    ("w_proj_eq", {"w.a": 2}, "proj_w", None),
    ("w_proj_arr", {"arr.k": {"$gte": 4}}, "proj_w", None),
    ("w_proj_z", {"z": {"$lt": 3}}, "proj_w", None),
    ("w_excl", {"z": {"$gte": 7}}, "excl_w", None),
    ("w_excl_arr", {"arr.k": 5}, "excl_w", None),
    ("w_part", {"w.a": 1, "keep": True}, "part_w", None),
    ("w_sort", {"w.c.d": {"$gte": 0}}, "w_w", [("w.c.d", 1), ("_id", 1)]),
    ("w_exists", {"w.c.d": {"$exists": True}}, "w_w", None),
]


def _geo_answers(coll: Any) -> dict:
    """Each query's ids (in order where mongod defines one) and count."""
    out: dict = {}
    for name, flt, hint, sort in _GEO_QUERIES:
        # A wildcard index is not planned yet (RM-777): its queries are asked
        # without a hint, as the answer is the same whatever plan serves it.
        hint = None if name.startswith("w_") else hint
        kw = {"hint": hint} if hint else {}
        ordered = "near" in name.lower()

        def run(flt=flt, kw=kw, sort=sort, ordered=ordered):
            cur = coll.find(flt, **kw)
            if sort:
                cur = cur.sort(sort)
            ids = [d["_id"] for d in cur]
            return ids if (ordered or sort) else sorted(ids)

        out[name] = mongo_answer(run)
        if "near" not in name.lower():
            out["count:" + name] = mongo_answer(
                lambda flt=flt, kw=kw: coll.count_documents(flt, **kw)
            )
    return out


def _winning(plan: dict | None, root_only: bool = False) -> list:
    """The winning plan's stage and index names, root first, down the
    `inputStage` chain (`root_only`: the root alone)."""
    out: list = []
    while plan:
        out.append((plan.get("stage"), plan.get("indexName")))
        if root_only:
            break
        plan = plan.get("inputStage")
    return out


#: `$near` queries with an equality on the index's other field: mongod plans
#: the equality as the near stage's bounds, this server as a FETCH above it,
#: or (the field before the geo one) a COLLSCAN (RM-777).
_PLANS_NOT_MATCHED = {"near2d_max", "nearSphere_pt_t", "near_pts_array"}


def _geo_plans(coll: Any) -> dict:
    """A `$near` is planned by the planner (stage and index); the other geo
    queries are asked their plan under their hint, as the planner does not
    yet choose a `$geoWithin` / `$geoIntersects` index scan (RM-777)."""
    out: dict = {}
    for name, flt, hint, _sort in _GEO_QUERIES:
        if name.startswith("w_") or name in _PLANS_NOT_MATCHED:
            continue
        near = "near" in name.lower()
        kw = {} if near else {"hint": hint}
        out[name] = mongo_answer(
            lambda flt=flt, kw=kw, near=near: _winning(
                coll.find(flt, **kw).explain()["queryPlanner"]["winningPlan"], near
            )
        )
    return out


_GEO_WRITES_DOC = {
    "_id": 900,
    "t": 1,
    "flat": [1, 1],
    "pt": {"type": "Point", "coordinates": [1, 1]},
    "pts": {"type": "MultiPoint", "coordinates": [[2, 2]]},
    "places": [{"loc": {"type": "Point", "coordinates": [2, 2]}, "n": 1}],
    "poly": _sq(3),
    "w": {"a": 3, "b": [2], "c": {"d": 4}},
    "z": 1,
    "arr": [{"k": 5}],
    "keep": True,
}


def _geo_writes(coll: Any) -> None:
    coll.insert_one(dict(_GEO_WRITES_DOC))
    coll.update_one({"_id": 5}, {"$set": {"flat": [2, 2], "w.a": 3, "z": 8, "keep": False}})
    coll.delete_one({"_id": 6})


@pytest.mark.backlog_fixed("RM-775")
def test_secantus_serves_mongods_geo_and_wildcard_indexes(tmp_path: Path) -> None:
    """A store mongod 8.2.11 wrote holding `2d`, `2dsphere` (compound, array,
    partial) and wildcard (whole, subtree, `wildcardProjection` both ways,
    partial) indexes is adopted by `secantusd-mdb`, which answers `$near` /
    `$nearSphere` / `$geoWithin` / `$geoIntersects` and wildcard queries as
    mongod does on the same data (documents, order where mongod defines it,
    winning plans), keeps the indexes consistent through its own writes, and
    hands the store back to a mongod whose `validate({full: true})` passes."""
    store = tmp_path / "store"
    store.mkdir()
    proc, client = _mongod_on(store)
    assert client is not None, "mongod did not start on an empty directory"
    host, port = client.address or ("127.0.0.1", 0)
    try:
        _wait_primary(client, f"{host}:{port}")
        with pymongo.MongoClient(
            host, port, replicaSet="rs0", serverSelectionTimeoutMS=10_000
        ) as rs:
            coll = rs["app"]["g"]
            coll.insert_many(_geo_wildcard_docs())
            _geo_wildcard_indexes(coll)
            expected = {"answers": _geo_answers(coll), "plans": _geo_plans(coll)}
    finally:
        _stop_mongod(proc, client)
    # A copy of the clean store takes the writes on mongod, the reference for
    # what the answers are afterwards.
    ref = tmp_path / "ref" / "store"
    shutil.copytree(store, ref)
    proc, client = _mongod_on(ref, port)
    assert client is not None, "mongod did not reopen its own store"
    try:
        _until_primary(client)
        _geo_writes(client["app"]["g"])
        expected["after"] = _geo_answers(client["app"]["g"])
    finally:
        _stop_mongod(proc, client)
    names = ["_id_", "flat_t", "flat_b", "pt_t", "t_pts", "pl_s", "poly_s"]
    names += ["poly_lv", "all_w", "w_w", "proj_w", "excl_w", "part_w"]
    with rust_mongo_server(store) as rust:
        coll = rust["app"]["g"]
        listed = mongo_answer(lambda: sorted(ix["name"] for ix in coll.list_indexes()))
        assert listed == ("ok", sorted(names))
        assert _geo_answers(coll) == expected["answers"]
        assert _geo_plans(coll) == expected["plans"]
        _geo_writes(coll)
        assert _geo_answers(coll) == expected["after"]
        rust_port = rust.address[1]
    # The store keeps the set mongod made (rs0), at the port the adopting
    # server listened on.
    proc, mongod = _mongod_on(store, rust_port, "rs0")
    try:
        assert mongod is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _until_primary(mongod)
            assert _validation_findings(mongod, "app", "g") is None
            assert _geo_answers(mongod["app"]["g"]) == expected["after"]
    finally:
        _stop_mongod(proc, mongod)


def _plans_of(client: pymongo.MongoClient) -> dict:
    coll = client["app"]["p"]
    cases = {
        "near_eq": ({"flat": {"$near": [3, 4]}, "t": 1}, None),
        "near_lead_eq": (
            {"pts": {"$near": {"$geometry": {"type": "Point", "coordinates": [5, 5]}}}, "t": 3},
            None,
        ),
        "within_box": ({"flat": {"$geoWithin": {"$box": [[-5, -5], [8, 9]]}}}, None),
        "intersects": ({"poly": {"$geoIntersects": {"$geometry": _sq(9)}}}, None),
        "wild_eq": ({"w.a": 3}, None),
        "wild_range": ({"w.c.d": {"$gt": 2, "$lte": 5}}, None),
        "wild_hinted": ({"w.a": 3}, "all_w"),
    }
    out: dict = {}
    for name, (flt, hint) in cases.items():
        kw = {"hint": hint} if hint else {}
        out[name] = mongo_answer(
            lambda flt=flt, kw=kw: (
                _winning(coll.find(flt, **kw).explain()["queryPlanner"]["winningPlan"]),
                sorted(d["_id"] for d in coll.find(flt, **kw)),
            )
        )
    return out


def _plan_collection(coll: Any) -> None:
    coll.insert_many(_geo_wildcard_docs())
    coll.create_index([("flat", "2d"), ("t", 1)], name="flat_t")
    coll.create_index([("t", 1), ("pts", "2dsphere")], name="t_pts")
    coll.create_index([("poly", "2dsphere")], name="poly_s")
    coll.create_index([("$**", 1)], name="all_w")


@pytest.mark.backlog("RM-777")
def test_geo_within_and_wildcard_indexes_are_planned(tmp_path: Path) -> None:
    """mongod 8.2.11 plans a `$geoWithin` / `$geoIntersects` onto its
    `2d` / `2dsphere` index (FETCH over IXSCAN), a wildcard predicate onto
    the wildcard index, an equality on another field of a compound `$near`
    index as the near stage's bounds, and reads a wildcard index under a
    hint. `secantusd-mdb` answers each of these with a COLLSCAN, or (a
    wildcard index named in a hint) an error."""
    store = tmp_path / "ref"
    store.mkdir()
    proc, client = _mongod_on(store)
    assert client is not None, "mongod did not start on an empty directory"
    try:
        host, port = client.address or ("127.0.0.1", 0)
        _wait_primary(client, f"{host}:{port}")
        with pymongo.MongoClient(
            host, port, replicaSet="rs0", serverSelectionTimeoutMS=10_000
        ) as rs:
            _plan_collection(rs["app"]["p"])
            expected = _plans_of(rs)
    finally:
        _stop_mongod(proc, client)
    with rust_mongo_server(tmp_path / "rust") as rust:
        _plan_collection(rust["app"]["p"])
        assert _plans_of(rust) == expected


@pytest.mark.backlog_fixed("RM-775")
def test_secantus_refuses_a_2dsphere_version_it_does_not_key_by_name(tmp_path: Path) -> None:
    """A mongod store holding a version 2 `2dsphere` index (keyed by strings
    this server does not write) is refused on adoption, naming the index;
    the version 3 indexes beside it are what is adopted."""

    def setup(rs: pymongo.MongoClient) -> None:
        coll = rs["app"]["c"]
        coll.insert_one({"_id": 1, "loc": {"type": "Point", "coordinates": [1, 2]}})
        coll.create_index([("loc", "2dsphere")], name="loc_v2", **{"2dsphereIndexVersion": 2})

    store = _mongod_store(tmp_path, setup)
    outcome, said = _secantus_open(store)
    assert (outcome, "app.c" in said, "loc_v2" in said) == ("refused", True, True), said
