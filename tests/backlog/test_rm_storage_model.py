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
    dbpath: Path, port: int | None = None, set_name: str = "rs0"
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
#: backlog item and a gap test below). `hashed` and text indexes are refused
#: by name when created, so the scenario's store never holds one.
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
    return (result["valid"], result.get("errors"), warnings)


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
        # Refused by name when created (phase 4), so the store has none.
        assert [s.split(":")[0] for s in steps if s.startswith("FAILED")] == [
            "FAILED kinds",
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


def _killed_secantus_store(store: Path, setup: Any) -> int:
    """`setup(client)` on a new `secantusd-mdb` store, then SIGKILL: the port
    it bound."""
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store)],
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


@pytest.mark.backlog("RM-741")
def test_mongod_validates_a_2d_index_secantus_wrote(tmp_path: Path) -> None:
    """mongod 8.2.11 keys a `2d` index by the point's GeoHash at the index's
    `bits`, x then y interleaved in the high bits, as BinData subtype 128
    (phase 0b: `[-10, 20]` is `6ad4ad4ad4ad4000`) and keeps it non-multikey
    with no multikey paths. This server keys it by a cell of its own (y
    first, in the low bits, BinData subtype 0), so mongod's `validate` finds
    every entry missing and one extra each."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.k.insert_many([{"_id": 1, "flat": [1.5, 2.5]}, {"_id": 2, "flat": [-10, 20]}])
        c.app.k.create_index([("flat", "2d")], name="flat_2d")

    with _mongod_on_secantus_store(tmp_path, setup) as mongod:
        assert _validation(mongod, "app", "k") == (True, [], [])


@pytest.mark.backlog("RM-742")
def test_mongod_validates_a_2dsphere_index_secantus_wrote(tmp_path: Path) -> None:
    """mongod 8.2.11 keys a point of a `2dsphere` (version 3) index by one S2
    cell id as a NumberLong (phase 0b: two points, two entries,
    `201384b1db336fa071` and `328fccdc3d444a7656`). This server writes its
    own covering (levels 4 to 16, every ancestor of each cell, BinData), so
    mongod's `validate` finds the entries missing and dozens extra."""

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


@pytest.mark.backlog("RM-743")
def test_mongod_validates_a_wildcard_index_secantus_wrote(tmp_path: Path) -> None:
    """mongod 8.2.11 keys a wildcard index by `($_path, value)` for each leaf
    path under it (phase 0b: `{w: {x: 1, y: {z: 2}}}` gives `w.x`/1 and
    `w.y.z`/2). This server writes one null entry per document, so mongod's
    `validate` finds the entries missing and the nulls extra."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.k.insert_many([{"_id": 1, "w": {"x": 1, "y": {"z": 2}}}, {"_id": 2, "w": {"x": "s"}}])
        c.app.k.create_index([("w.$**", 1)], name="w_wildcard")

    with _mongod_on_secantus_store(tmp_path, setup) as mongod:
        assert _validation(mongod, "app", "k") == (True, [], [])


@pytest.mark.backlog("RM-744")
def test_mongod_finds_the_users_and_roles_secantus_made(tmp_path: Path) -> None:
    """mongod 8.2.11 keeps users and roles as documents of
    `admin.system.users` and `admin.system.roles` (phase 0b). This server
    keeps them in tables of its own (`secantus_users`, `secantus_roles`), so
    mongod on its store has neither the user nor the role."""

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


@pytest.mark.backlog("RM-745")
def test_a_views_ddl_is_logged_as_mongod_logs_it() -> None:
    """mongod 8.2.11 logs a view's DDL as writes to `system.views`, not as
    commands (measured 2026-11-21): the first view's database gets
    `{op: c, o: {create: "system.views", idIndex}}`, then each create is an
    insert of the definition, a `collMod` an update replacing it and a drop
    its delete. This server logs `create` (with its internal `viewPipeline`
    and an `idIndex`), `collMod` and `drop` commands naming the view."""
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


#: The `_id` index spec an oplog `create` entry carries.
ID_INDEX_SPEC = {"v": 2, "key": {"_id": 1}, "name": "_id_"}


@pytest.mark.backlog("RM-746")
def test_mongod_opens_a_killed_secantus_store(tmp_path: Path) -> None:
    """mongod 8.2.11's stable timestamp is always an oplog entry's (its
    commit point), so after a crash its checkpoint's recovery timestamp is at
    or below the oplog's last entry. This server's stable timestamp is its
    all-committed point, which timestamps minted without an entry move past
    the last entry; killed, its store has a recovery timestamp and an
    `oplogTruncateAfterPoint` past the oplog's end, and mongod, opening it,
    truncates, forces the stable and oldest timestamps back, then aborts on
    its next `oldest_timestamp` ("set_timestamp: oldest timestamp ... must
    not be older than current oldest timestamp", invariant in
    `WiredTigerKVEngine::setOldestTimestamp`; measured 2026-11-21)."""

    def setup(c: pymongo.MongoClient) -> None:
        c.app.c.insert_many([{"_id": i, "a": i % 3} for i in range(100)])
        c.app.c.create_index("a", name="a_1")
        time.sleep(1)

    with _mongod_on_secantus_store(tmp_path, setup, kill=True) as mongod:
        time.sleep(2)  # mongod moves its oldest timestamp as it serves
        assert mongod.app.c.count_documents({}) == 100
        assert _validation(mongod, "app", "c") == (True, [], [])


def _mongod_store(tmp_path: Path, setup: Any, kill: bool = False) -> Path:
    """`setup(client)` on a new mongod 8.2.11 one-member set, then a clean
    stop (or SIGKILL with `kill`, its journal and oplog recovery pending)."""
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
    return store


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


@pytest.mark.backlog("RM-723")
def test_secantus_opens_a_killed_mongod_store(tmp_path: Path) -> None:
    """A store mongod 8.2.11 was killed on (SIGKILL: journal and oplog
    recovery pending), opened by `secantusd-mdb`, serves every acknowledged
    write: WiredTiger recovers the logged tables, and the oplog after the
    stable checkpoint is replayed, as mongod's own restart does."""

    def setup(rs: pymongo.MongoClient) -> None:
        rs["app"]["c"].insert_many([{"_id": i, "a": i % 3} for i in range(50)])
        rs["app"]["c"].create_index("a", name="a_1")

    store = _mongod_store(tmp_path, setup, kill=True)
    with rust_mongo_server(store) as rust:
        coll = rust["app"]["c"]
        answer = mongo_answer(lambda: sorted(d["_id"] for d in coll.find()))
        assert answer == ("ok", list(range(50)))


@pytest.mark.backlog("RM-723")
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


@pytest.mark.backlog("RM-723")
def test_secantus_refuses_an_index_kind_it_does_not_serve_by_name(tmp_path: Path) -> None:
    """A mongod store holding an index this server does not write as mongod
    does (here `hashed`, which it refuses to create) is refused when
    `secantusd-mdb` opens it, naming the index, never served without it."""

    def setup(rs: pymongo.MongoClient) -> None:
        rs["app"]["c"].insert_one({"_id": 1, "h": "x"})
        rs["app"]["c"].create_index([("h", "hashed")], name="h_hashed")

    store = _mongod_store(tmp_path, setup)
    outcome, said = _secantus_open(store)
    assert (outcome, "app.c" in said, "h_hashed" in said) == ("refused", True, True), said
