"""The logical session cache and its transaction reaper (RM-224) against a
live mongod 8.2, and the session a retryable `bulkWrite` logs its writes
under.

mongod keeps a session's retryable-write and transaction state in
`config.transactions` (and a `findAndModify`'s image in
`config.image_collection`), writes the sessions it has seen to
`config.system.sessions` every `logicalSessionRefreshMillis`, and reaps the
state of a session whose record has gone: logged deletes, images first.
Both servers are started with mongod's test commands on, so
`refreshLogicalSessionCacheNow` / `reapLogicalSessionCacheNow` run the two
jobs when the test says, with `TransactionRecordMinimumLifetimeMinutes=0`
so a row is old enough to reap at once, and with the periodic refresh an
hour away so it never runs in the middle of a case.

Every command is a raw OP_MSG on a connection of its own: a driver would
replace the `lsid` with a session of its own.
"""

from __future__ import annotations

import contextlib
import socket
import struct
import subprocess
import tempfile
import time
import uuid
from collections.abc import Iterator
from typing import Any

import bson
import pymongo
import pytest
from bson import Binary, Int64, Timestamp, json_util

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output
from tests.mongod_live.conftest import same

_PARAMETERS = (
    "TransactionRecordMinimumLifetimeMinutes=0",
    "logicalSessionRefreshMillis=3600000",
)


@contextlib.contextmanager
def _rust(parameters: tuple[str, ...], extra: tuple[str, ...] = ()) -> Iterator[Any]:
    """A `secantusd-rs` with test commands on, started with `parameters`
    (and the further arguments `extra`)."""
    assert RS_BINARY.exists(), f"{RS_BINARY} is not built (./inv rust-binary-build)"
    store = tempfile.mkdtemp(prefix="live-sessions-")
    args = [str(RS_BINARY), "--port", "0", "--storage-path", store, "--enable-test-commands"]
    args += list(extra)
    for p in parameters:
        args += ["--setParameter", p]
    proc = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
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
        yield client
        client.close()
    finally:
        proc.terminate()
        proc.wait(timeout=30)
        subprocess.run(["rm", "-rf", store], check=True)


@contextlib.contextmanager
def _mongod(parameters: tuple[str, ...]) -> Iterator[Any]:
    """A one-member mongod 8.2 replica set started as `_rust` starts ours."""
    from tests.mongod_reference import MONGOD, _host_port, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    extra = ["--replSet", "sessions", "--setParameter", "enableTestCommands=1"]
    for p in parameters:
        extra += ["--setParameter", p]
    proc, uri, dbpath = _start_mongod(extra_args=tuple(extra))
    try:
        boot = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        host, port = _host_port(uri)
        boot.admin.command(
            {
                "replSetInitiate": {
                    "_id": "sessions",
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


@pytest.fixture(scope="module")
def rust_sessions() -> Iterator[Any]:
    with _rust(_PARAMETERS) as client:
        yield client


@pytest.fixture(scope="module")
def mongod_sessions() -> Iterator[Any]:
    with _mongod(_PARAMETERS) as client:
        yield client


def _op_msg(client: Any, command: dict) -> dict:
    """One command as a raw OP_MSG on a connection of its own."""
    host, port = client.address
    body = b"\x00" + bson.encode(command)
    msg = struct.pack("<iiii", 16 + 4 + len(body), 1, 0, 2013) + struct.pack("<I", 0) + body
    with socket.create_connection((host, port), timeout=120) as sock:
        sock.sendall(msg)
        head = b""
        while len(head) < 4:
            head += sock.recv(4 - len(head))
        (length,) = struct.unpack("<i", head)
        rest = b""
        while len(rest) < length - 4:
            rest += sock.recv(length - 4 - len(rest))
    reply = bson.decode(rest[12 + 4 + 1 :])
    for k in ("$clusterTime", "operationTime", "electionId", "opTime"):
        reply.pop(k, None)
    return reply


class _Sessions:
    """Sessions of a case's own, named, and everything read back with their
    ids, timestamps and dates replaced by what is the same on both servers."""

    def __init__(self, client: Any) -> None:
        self.client = client
        self.names: dict[bytes, str] = {}

    def new(self, name: str) -> dict:
        raw = uuid.uuid4().bytes
        self.names[raw] = name
        return {"id": Binary(raw, 4)}

    def run(self, command: dict, db: str = "admin", **fields: Any) -> dict:
        return self.norm(_op_msg(self.client, {**command, **fields, "$db": db}))

    def norm(self, value: Any) -> Any:
        if isinstance(value, dict):
            if set(value) == {"id", "uid"} and isinstance(value.get("id"), Binary):
                return {"id": self.names.get(bytes(value["id"]), "<other>"), "uid": value["uid"]}
            return {k: self.norm(v) for k, v in value.items()}
        if isinstance(value, list):
            return [self.norm(v) for v in value]
        if isinstance(value, Timestamp):
            return "<ts>" if value.time else value
        if hasattr(value, "year"):
            return "<date>"
        return value

    def ours(self, doc: dict) -> bool:
        """Whether `doc` (an oplog entry, a record) is about a session of
        this case: the servers' own and the driver's are left out."""
        key = doc.get("_id") or (doc.get("o2") or {}).get("_id") or (doc.get("o") or {}).get("_id")
        return isinstance(key, dict) and bytes(key.get("id", b"")) in self.names

    def last_ts(self) -> Timestamp:
        return self.client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]

    def config_entries(self, after: Timestamp) -> list:
        return [
            self.norm({k: e[k] for k in ("op", "ns", "o", "o2") if k in e})
            for e in self.client.local["oplog.rs"].find(
                {"ts": {"$gt": after}, "ns": {"$regex": "^config\\."}}
            )
            if self.ours(e)
        ]

    def rows(self, coll: str) -> list:
        rows = [self.norm(d) for d in self.client.config[coll].find() if self.ours(d)]
        return sorted(rows, key=lambda d: d["_id"]["id"])

    def listed(self, stage: str) -> list:
        """What `$listSessions` / `$listLocalSessions` list of this case's
        sessions."""
        on = ("config", "system.sessions") if stage == "$listSessions" else ("admin", 1)
        reply = _op_msg(
            self.client,
            {
                "aggregate": on[1],
                "pipeline": [{stage: {"allUsers": True}}],
                "cursor": {},
                "$db": on[0],
            },
        )
        rows = [self.norm(d) for d in reply["cursor"]["firstBatch"] if self.ours(d)]
        return sorted(rows, key=lambda d: d["_id"]["id"])


def _by_namespace(entries: list) -> list:
    """The namespaces in the order the entries came, each once, with the
    entries of each as a sorted set: the order inside one step is a hash
    set's on mongod."""
    out: list = []
    for e in entries:
        if not out or out[-1][0] != e["ns"]:
            out.append((e["ns"], []))
        out[-1][1].append(json_util.dumps(e, sort_keys=True))
    return [(ns, sorted(group)) for ns, group in out]


def _session_lifecycle(client: Any) -> list:
    s = _Sessions(client)
    client.drop_database("live_ls")
    client.live_ls.c.insert_many([{"_id": i, "x": i} for i in range(4)])
    insert, update, remove, reader, txn, bulk = (
        s.new(n) for n in ("insert", "update", "remove", "reader", "txn", "bulk")
    )
    out: list = []
    start = s.last_ts()
    out.append(
        s.run(
            {"insert": "c", "documents": [{"_id": 10}]},
            "live_ls",
            lsid=insert,
            txnNumber=Int64(1),
        )
    )
    fam = {"findAndModify": "c", "query": {"_id": 1}, "update": {"$inc": {"x": 1}}}
    out.append(s.run(fam, "live_ls", lsid=update, txnNumber=Int64(3)))
    out.append(
        s.run(
            {"findAndModify": "c", "query": {"_id": 2}, "remove": True},
            "live_ls",
            lsid=remove,
            txnNumber=Int64(1),
        )
    )
    out.append(s.run({"find": "c", "filter": {}}, "live_ls", lsid=reader))
    s.run(
        {"insert": "c", "documents": [{"_id": 20}]},
        "live_ls",
        lsid=txn,
        txnNumber=Int64(4),
        startTransaction=True,
        autocommit=False,
    )
    out.append(("open transaction's row", s.rows("transactions")))
    s.run({"commitTransaction": 1}, lsid=txn, txnNumber=Int64(4), autocommit=False)
    out.append(
        s.run(
            {
                "bulkWrite": 1,
                "ops": [
                    {"insert": 0, "document": {"_id": 30}},
                    {"insert": 0, "document": {"_id": 31}},
                    {"update": 0, "filter": {"_id": 3}, "updateMods": {"$set": {"y": 1}}},
                ],
                "nsInfo": [{"ns": "live_ls.c"}],
            },
            lsid=bulk,
            txnNumber=Int64(2),
        )
    )
    # The state is kept beside the oplog, not in it.
    out.append(("rows", s.rows("transactions"), s.rows("image_collection")))
    out.append(("logged before a refresh", s.config_entries(start)))

    out.append(("listed before a refresh", s.listed("$listSessions")))
    out.append(("local before a refresh", s.listed("$listLocalSessions")))
    start = s.last_ts()
    out.append(s.run({"refreshLogicalSessionCacheNow": 1}))
    out.append(("refresh", _by_namespace(s.config_entries(start)), s.rows("system.sessions")))
    out.append(("listed", s.listed("$listSessions"), s.listed("$listLocalSessions")))
    start = s.last_ts()
    out.append(s.run({"reapLogicalSessionCacheNow": 1}))
    out.append(("reap of live sessions", s.config_entries(start), len(s.rows("transactions"))))

    start = s.last_ts()
    out.append(s.run({"endSessions": [insert, update, remove, reader, txn, bulk]}))
    out.append(s.run({"refreshLogicalSessionCacheNow": 1}))
    out.append(("ended", _by_namespace(s.config_entries(start)), s.rows("system.sessions")))
    start = s.last_ts()
    out.append(s.run({"reapLogicalSessionCacheNow": 1}))
    out.append(
        (
            "reaped",
            _by_namespace(s.config_entries(start)),
            s.rows("transactions"),
            s.rows("image_collection"),
        )
    )
    # A retry of a reaped write runs it again.
    out.append(s.run(fam, "live_ls", lsid=update, txnNumber=Int64(3)))
    out.append(list(client.live_ls.c.find().sort("_id", 1)))
    client.drop_database("live_ls")
    return out


def test_the_reaper_deletes_an_expired_sessions_state(
    rust_sessions: Any, mongod_sessions: Any
) -> None:
    """mongod 8.2.11 keeps a session's `config.transactions` row (and a
    `findAndModify`'s image) without an oplog entry once a retryable write
    or a committed transaction of it logged one; a refresh upserts each
    session used since the last (`i` for a new record, `u` with the
    `lastUse` diff for one there) and deletes the records of ended
    sessions; the reaper deletes the images and then the rows of sessions
    without a record, and forgets their retryable writes."""
    same(_session_lifecycle(rust_sessions), _session_lifecycle(mongod_sessions))


def _cache_status(client: Any) -> list:
    status = _op_msg(client, {"serverStatus": 1, "$db": "admin"})["logicalSessionRecordCache"]
    return [(k, type(v).__name__) for k, v in status.items()]


def test_server_status_reports_the_cache_jobs(rust_sessions: Any, mongod_sessions: Any) -> None:
    """`serverStatus.logicalSessionRecordCache` reports both jobs, in
    mongod 8.2.11's fields, order and types."""
    for client in (rust_sessions, mongod_sessions):
        _op_msg(client, {"refreshLogicalSessionCacheNow": 1, "$db": "admin"})
        _op_msg(client, {"reapLogicalSessionCacheNow": 1, "$db": "admin"})
    same(_cache_status(rust_sessions), _cache_status(mongod_sessions))


def _session_parameters(client: Any) -> list:
    names = [
        "TransactionRecordMinimumLifetimeMinutes",
        "disableLogicalSessionCacheRefresh",
        "localLogicalSessionTimeoutMinutes",
        "logicalSessionRefreshMillis",
    ]
    got = _op_msg(client, {"getParameter": 1, **dict.fromkeys(names, 1), "$db": "admin"})
    out: list = [got]
    for name in names:
        refused = _op_msg(client, {"setParameter": 1, name: 5, "$db": "admin"})
        out.append((refused.get("code"), refused.get("errmsg")))
    hello = _op_msg(client, {"hello": 1, "$db": "admin"})
    out.append(hello.get("logicalSessionTimeoutMinutes"))
    # A set member builds `config.system.sessions` on its first refresh: run
    # one, so the index does not depend on which test ran before.
    _op_msg(client, {"refreshLogicalSessionCacheNow": 1, "$db": "admin"})
    out.append(
        [
            ix
            for ix in client.config["system.sessions"].list_indexes()
            if ix["name"] == "lsidTTLIndex"
        ]
    )
    return out


def test_the_session_parameters_are_startup_only(rust_sessions: Any, mongod_sessions: Any) -> None:
    """The four session parameters report what the server was started with
    and refuse a change at runtime (20 IllegalOperation)."""
    same(_session_parameters(rust_sessions), _session_parameters(mongod_sessions))


def _sessions_collection(client: Any) -> list:
    """Whether `config.system.sessions` is there, and its `lsidTTLIndex`,
    on a server nothing has used. (A set member's other `config`
    collections are RM-366.)"""
    there = "system.sessions" in client.config.list_collection_names()
    ttl = [ix for ix in client.config["system.sessions"].list_indexes()]
    return [there, [ix for ix in ttl if ix["name"] == "lsidTTLIndex"]]


def test_a_set_member_builds_the_sessions_collection_at_its_first_refresh() -> None:
    """mongod 8.2.11: a standalone creates `config.system.sessions` and its
    TTL index as it starts; a replica-set member leaves them to its logical
    session cache's first refresh, a `logicalSessionRefreshMillis` after it
    steps up (299 s under the default, measured 2026-10-28). This server
    created them at startup whatever its topology."""
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    with _rust(_PARAMETERS) as ours, _mongod(_PARAMETERS) as theirs:
        same(_sessions_collection(ours), _sessions_collection(theirs))
    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(extra_args=("--setParameter", "enableTestCommands=1"))
    try:
        theirs = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        # A standalone sets the collection up just after it starts listening.
        deadline = time.monotonic() + 10
        while "system.sessions" not in theirs.config.list_collection_names():
            assert time.monotonic() < deadline, "a standalone mongod never built the collection"
            time.sleep(0.1)
        with _rust(_PARAMETERS, ("--standalone",)) as ours:
            same(_sessions_collection(ours), _sessions_collection(theirs))
        theirs.close()
    finally:
        _stop_mongod(proc, dbpath)


def test_a_shorter_session_timeout_is_the_records_ttl() -> None:
    """Started with `localLogicalSessionTimeoutMinutes=1`, mongod 8.2.11
    says so in `hello` and expires session records after 60 seconds."""

    def answers(client: Any) -> list:
        _op_msg(client, {"refreshLogicalSessionCacheNow": 1, "$db": "admin"})
        return _session_parameters(client)

    shorter = ("localLogicalSessionTimeoutMinutes=1", *_PARAMETERS)
    with _rust(shorter) as ours, _mongod(shorter) as theirs:
        same(answers(ours), answers(theirs))


# ---------------------------------------------------- retryable bulkWrite --

_NS = [{"ns": "live_bw.c"}, {"ns": "live_bw.d"}]


def _ins(ns: int, i: int) -> dict:
    return {"insert": ns, "document": {"_id": i}}


_BULK_CASES: list[tuple[str, dict]] = [
    (
        "mixed namespaces and kinds",
        {
            "ops": [
                _ins(0, 10),
                _ins(1, 11),
                _ins(0, 12),
                {"update": 1, "filter": {"_id": 1}, "updateMods": {"$set": {"y": 1}}},
                {
                    "update": 0,
                    "filter": {"_id": 99},
                    "updateMods": {"$set": {"y": 1}},
                    "upsert": True,
                },
                {"update": 0, "filter": {"_id": 2}, "updateMods": {"z": 1}},
                {"delete": 1, "filter": {"_id": 2}},
                {"delete": 0, "filter": {"_id": 50}},
            ]
        },
    ),
    (
        "consecutive inserts",
        {"ops": [_ins(0, 10), _ins(0, 11), _ins(1, 12), _ins(1, 13), _ins(0, 14)]},
    ),
    ("a duplicate, ordered", {"ops": [_ins(0, 10), _ins(0, 1), _ins(0, 11)]}),
    (
        "a duplicate, unordered",
        {"ordered": False, "ops": [_ins(0, 10), _ins(0, 1), _ins(0, 11), _ins(0, 12)]},
    ),
    (
        "a multi op",
        {
            "ordered": False,
            "ops": [
                _ins(0, 10),
                {"update": 0, "filter": {}, "updateMods": {"$set": {"m": 1}}, "multi": True},
                _ins(1, 11),
            ],
        },
    ),
    ("statement ids given", {"ops": [_ins(0, 10), _ins(0, 11)], "stmtIds": [5, 9]}),
    ("a first statement id given", {"ops": [_ins(0, 10), _ins(1, 11)], "stmtId": 7}),
]


def _clean(entry: dict) -> dict:
    entry = {k: v for k, v in entry.items() if k not in ("ts", "wall", "ui", "lsid")}
    if "prevOpTime" in entry:
        prev = entry["prevOpTime"]
        entry["prevOpTime"] = {"ts": "<ts>" if prev["ts"].time else prev["ts"], "t": prev["t"]}
    if entry.get("op") == "c" and "applyOps" in entry["o"]:
        entry["o"] = {
            "applyOps": [
                {k: v for k, v in op.items() if k != "ui"} for op in entry["o"]["applyOps"]
            ]
        }
    return entry


def _bulk_writes(client: Any) -> list:
    out: list = []
    oplog = client.local["oplog.rs"]
    for label, body in _BULK_CASES:
        client.drop_database("live_bw")
        client.live_bw.c.insert_many([{"_id": i} for i in range(3)])
        client.live_bw.d.insert_many([{"_id": i} for i in range(3)])
        lsid = {"id": Binary(uuid.uuid4().bytes, 4)}
        command = {"bulkWrite": 1, "nsInfo": _NS, **body, "lsid": lsid, "txnNumber": Int64(1)}
        for attempt in ("first", "retry"):
            start = oplog.find_one(sort=[("$natural", -1)])["ts"]
            reply = _op_msg(client, {**command, "$db": "admin"})
            reply.get("cursor", {}).pop("id", None)
            entries = [
                _clean(e)
                for e in oplog.find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
                if e["ns"].startswith("live_bw.") or e["ns"] == "admin.$cmd"
            ]
            out.append((label, attempt, reply, entries))
        out.append((label, list(client.live_bw.c.find()), list(client.live_bw.d.find())))
    client.drop_database("live_bw")
    return out


def test_a_retryable_bulk_write_logs_its_session(rust_sessions: Any, mongod_sessions: Any) -> None:
    """mongod 8.2.11 logs each op of a retryable `bulkWrite` under its
    statement id with the session's `prevOpTime` chain, consecutive inserts
    into one namespace as one `applyOps` (`multiOpType: 1`, a `stmtId` per
    insert), a batch with a failed insert as single inserts; a retry answers
    `retriedStmtIds` and logs nothing."""
    same(_bulk_writes(rust_sessions), _bulk_writes(mongod_sessions))
