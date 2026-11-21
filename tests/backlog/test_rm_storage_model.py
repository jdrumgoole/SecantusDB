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
import socket
import subprocess
import time
from collections.abc import Iterator
from pathlib import Path

import bson
import pymongo
import pytest

from tests.backlog.conftest import mongo_answer, rust_mongo_server
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


def _mongod_on(dbpath: Path) -> tuple[subprocess.Popen, pymongo.MongoClient | None]:
    """Start mongod as a one-member replica set on `dbpath`, which may already
    hold a store. Returns the process and a client to the primary, or `None`
    for the client when mongod refused the store (it exits)."""
    assert MONGOD, "no mongod on PATH: the reference server is required"
    port = _free_port()
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
            "rs0",
            "--logpath",
            str(dbpath.parent / f"mongod-{port}.log"),
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
    deadline = time.monotonic() + 60
    while not client.admin.command("hello").get("isWritablePrimary"):
        assert time.monotonic() < deadline, "mongod never became primary"
        time.sleep(0.2)


def _stop_mongod(proc: subprocess.Popen, client: pymongo.MongoClient | None) -> None:
    if client is not None:
        # The connection drops as mongod stops.
        with contextlib.suppress(pymongo.errors.PyMongoError):
            client.admin.command("shutdown")
        client.close()
    try:
        proc.wait(timeout=60)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def _secantus_store(tmp_path: Path) -> Path:
    """A store written by `secantusd-mdb`: one collection with a secondary
    index, then a clean stop (SIGTERM)."""
    store = tmp_path / "store"
    with rust_mongo_server(store) as client:
        coll = client["app"]["c"]
        coll.insert_many([{"_id": i, "a": i % 3} for i in range(10)])
        coll.create_index("a", name="a_1")
    return store


@pytest.mark.backlog("RM-724")
def test_shutdown_command_stops_the_server(tmp_path: Path) -> None:
    """mongod 8.2.11: `shutdown` closes the connection (the driver sees a
    network error, not a reply) and the process exits. `secantusd-mdb`
    answers CommandNotFound (59) and keeps running."""
    with rust_mongo_server(tmp_path / "store") as client:
        try:
            client.admin.command("shutdown")
            answer: tuple = ("replied",)
        except pymongo.errors.OperationFailure as exc:
            answer = ("err", exc.code)
        except pymongo.errors.ConnectionFailure:
            answer = ("closed",)
        assert answer == ("closed",)
        address = client.address
        assert address is not None
        deadline = time.monotonic() + 30
        while True:
            try:
                with socket.create_connection(address, timeout=0.5):
                    pass
            except OSError:
                break  # nothing listens: the server stopped
            assert time.monotonic() < deadline, "the server still listens after `shutdown`"
            time.sleep(0.2)


@pytest.mark.backlog("RM-723")
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


@pytest.mark.backlog("RM-723")
def test_mongod_opens_a_secantus_store(tmp_path: Path) -> None:
    """mongod 8.2.11 started with `--replSet` on a store `secantusd-mdb`
    wrote finds its collection, its documents and its index, and
    `validate({full: true})` passes."""
    store = _secantus_store(tmp_path)
    proc, client = _mongod_on(store)
    try:
        assert client is not None, "mongod refused the store (see its log beside the store)"
        with _mongod_answers(store):
            _wait_primary(client, f"{client.address[0]}:{client.address[1]}")
            coll = client["app"]["c"]
            assert sorted(d["_id"] for d in coll.find()) == list(range(10))
            assert sorted(ix["name"] for ix in coll.list_indexes()) == ["_id_", "a_1"]
            result = client["app"].command("validate", "c", full=True)
            assert (result["valid"], result["nrecords"]) == (True, 10)
    finally:
        _stop_mongod(proc, client)


@pytest.mark.backlog("RM-723")
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


@pytest.mark.backlog("RM-736")
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
