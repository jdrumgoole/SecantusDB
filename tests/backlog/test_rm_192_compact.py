"""RM-192 (`compact` gives disk space back; `--repair`) and RM-219's
`autoCompact`, against mongod 8.2.11 as a one-member replica set.

`compact` used to answer `bytesFreed: 0` without doing anything, and
`autoCompact` was 59 CommandNotFound. mongod's replies, measured
2026-10-26: options parsed in the order sent (`force` / `dryRun` booleans,
`freeSpaceTargetMB` a whole number of at least 1), `force` needed on a
primary, a view refused, a time-series collection compacted through either
name, `estimatedBytesFreed` for a dry run (0 on every shape measured), and
`bytesFreed` the files' shrinkage -- non-zero once at least
`freeSpaceTargetMB` (default 20 MB) of a file is free. The collections here
share their files with others (`secantus_storage::compact`), so the bytes
freed are compared as mongod's are only in being given back, not in amount.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time
from typing import Any

import pymongo
import pytest
from bson import Decimal128, Int64
from bson.codec_options import CodecOptions
from bson.raw_bson import RawBSONDocument
from pymongo.errors import PyMongoError

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

_RAW = CodecOptions(document_class=RawBSONDocument)
_FREED = ("bytesFreed", "estimatedBytesFreed")


def _bson_type(raw: bytes, name: str) -> int:
    """The BSON type byte of top-level field `name` in a raw document."""
    i = 4
    while i < len(raw) - 1:
        t = raw[i]
        end = raw.index(0, i + 1)
        key = raw[i + 1 : end].decode()
        if key == name:
            return t
        j = end + 1
        if t == 0x10:
            j += 4
        elif t in (0x01, 0x09, 0x11, 0x12):
            j += 8
        elif t == 0x08:
            j += 1
        elif t in (0x02,):
            j += 4 + int.from_bytes(raw[j : j + 4], "little")
        elif t in (0x03, 0x04):
            j += int.from_bytes(raw[j : j + 4], "little")
        else:
            raise AssertionError(f"type {t:#x} not walked")
        i = j
    raise AssertionError(f"{name} not in the reply")


def reply(db: Any, cmd: dict, **kw: Any) -> tuple:
    """A command's answer: its fields in order (a freed-bytes count as its
    BSON type), or `("err", code, errmsg)`."""
    try:
        raw = db.command(cmd, codec_options=_RAW, **kw).raw
    except PyMongoError as e:
        details = getattr(e, "details", None) or {}
        return ("err", getattr(e, "code", None), details.get("errmsg", str(e)))
    out = []
    for k, v in RawBSONDocument(raw).items():
        if k in ("$clusterTime", "operationTime"):
            continue
        if k in _FREED:
            # The amount is the files' business: the checkpoint compaction
            # starts with can trim a few KB off a small collection's file on
            # mongod as well, run to run. Compared: the BSON type.
            v = hex(_bson_type(raw, k))
        out.append((k, v))
    return ("ok", out)


def _seed(client: Any) -> Any:
    client.drop_database("rm192")
    db = client.rm192
    db.c.insert_many([{"x": i} for i in range(10)])
    db.command({"create": "v", "viewOn": "c", "pipeline": []})
    db.command({"create": "ts", "timeseries": {"timeField": "t"}})
    db.command({"create": "cap", "capped": True, "size": 4096})
    return db


COMPACT_CASES: list[tuple[str, dict]] = [
    ("no force on a primary", {"compact": "c"}),
    ("force", {"compact": "c", "force": True}),
    ("force false", {"compact": "c", "force": False}),
    ("force an int", {"compact": "c", "force": 1}),
    ("force a string", {"compact": "c", "force": "yes"}),
    ("dry run", {"compact": "c", "force": True, "dryRun": True}),
    ("dry run without force", {"compact": "c", "dryRun": True}),
    ("dry run false", {"compact": "c", "force": True, "dryRun": False}),
    ("dry run an int", {"compact": "c", "force": True, "dryRun": 1}),
    ("target 1", {"compact": "c", "force": True, "freeSpaceTargetMB": 1}),
    ("target 0", {"compact": "c", "force": True, "freeSpaceTargetMB": 0}),
    ("target -1", {"compact": "c", "force": True, "freeSpaceTargetMB": -1}),
    ("target 0.5", {"compact": "c", "force": True, "freeSpaceTargetMB": 0.5}),
    ("target NaN", {"compact": "c", "force": True, "freeSpaceTargetMB": float("nan")}),
    ("target 1.5", {"compact": "c", "force": True, "freeSpaceTargetMB": 1.5}),
    ("target long", {"compact": "c", "force": True, "freeSpaceTargetMB": Int64(3)}),
    ("target decimal", {"compact": "c", "force": True, "freeSpaceTargetMB": Decimal128("2.5")}),
    ("target a string", {"compact": "c", "force": True, "freeSpaceTargetMB": "1"}),
    ("target a bool", {"compact": "c", "force": True, "freeSpaceTargetMB": True}),
    ("target null", {"compact": "c", "force": True, "freeSpaceTargetMB": None}),
    ("comment", {"compact": "c", "force": True, "comment": "hi"}),
    ("unknown field", {"compact": "c", "force": True, "bogus": 1}),
    ("paddingFactor", {"compact": "c", "force": True, "paddingFactor": 1.1}),
    ("force type before an unknown field", {"compact": "c", "force": 1, "bogus": 1}),
    ("unknown field before force type", {"compact": "c", "bogus": 1, "force": 1}),
    ("target before force", {"compact": "c", "freeSpaceTargetMB": 0, "force": 1}),
    ("missing", {"compact": "nope", "force": True}),
    ("missing, bad target", {"compact": "nope", "freeSpaceTargetMB": 0, "force": True}),
    ("missing, no force", {"compact": "nope"}),
    ("view", {"compact": "v", "force": True}),
    ("time-series", {"compact": "ts", "force": True}),
    ("time-series buckets", {"compact": "system.buckets.ts", "force": True}),
    ("capped", {"compact": "cap", "force": True}),
    ("system.views", {"compact": "system.views", "force": True}),
    ("name an int", {"compact": 5, "force": True}),
    ("name empty", {"compact": "", "force": True}),
    ("name empty, unknown field", {"compact": "", "bogus": 1}),
    ("writeConcern", {"compact": "c", "force": True, "writeConcern": {"w": 1}}),
    ("readConcern", {"compact": "c", "force": True, "readConcern": {"level": "local"}}),
    ("maxTimeMS", {"compact": "c", "force": True, "maxTimeMS": 1000}),
]


def _compact_replies(client: Any) -> dict:
    db = _seed(client)
    out = {name: reply(db, cmd) for name, cmd in COMPACT_CASES}
    out["admin.system.version"] = reply(client.admin, {"compact": "system.version", "force": True})
    out["local.oplog.rs"] = reply(client.local, {"compact": "oplog.rs", "force": True})
    out["config, missing"] = reply(client.config, {"compact": "x", "force": True})
    out["database missing"] = reply(client.rm192_none, {"compact": "c", "force": True})
    with client.start_session() as s:
        s.start_transaction()
        out["in a transaction"] = reply(db, {"compact": "c", "force": True}, session=s)
        out["autoCompact in a transaction"] = reply(client.admin, {"autoCompact": False}, session=s)
        s.abort_transaction()
    return out


@pytest.mark.backlog_fixed("RM-192")
def test_compact_answers_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """Every option, error and namespace kind, mongod 8.2.11's way: the
    reply's fields in order (a freed-bytes count as an int32), and each
    error's code and message."""
    ours, theirs = _compact_replies(rust_mongo), _compact_replies(mongod_rs)
    differ = {k: (ours[k], theirs[k]) for k in theirs if ours[k] != theirs[k]}
    assert not differ, differ


def _bg_stats(client: Any) -> dict:
    return client.admin.command("serverStatus")["wiredTiger"]["background-compact"]


def _wait_idle(client: Any) -> None:
    deadline = time.monotonic() + 60
    while _bg_stats(client)["background compact running"] != 0:
        assert time.monotonic() < deadline, "background compaction never went idle"
        time.sleep(0.1)


AUTO_STEPS: list[dict | str] = [
    {"autoCompact": False},
    {"autoCompact": True},
    {"autoCompact": True},
    {"autoCompact": True, "freeSpaceTargetMB": 5},
    {"autoCompact": True, "runOnce": True},
    {"autoCompact": False},
    {"autoCompact": False},
    {"autoCompact": False, "freeSpaceTargetMB": 5},
    {"autoCompact": False, "runOnce": True},
    {"autoCompact": True, "freeSpaceTargetMB": 5},
    {"autoCompact": True, "freeSpaceTargetMB": 5},
    {"autoCompact": True, "freeSpaceTargetMB": 6},
    {"autoCompact": True},
    {"autoCompact": False},
    {"autoCompact": True, "runOnce": True},
    "idle",
    {"autoCompact": True, "runOnce": True},
    "idle",
    {"autoCompact": True},
    {"autoCompact": False},
    {"autoCompact": True, "freeSpaceTargetMB": 0},
    {"autoCompact": True, "freeSpaceTargetMB": -3},
    {"autoCompact": True, "freeSpaceTargetMB": 1.5},
    {"autoCompact": True, "freeSpaceTargetMB": Int64(2)},
    {"autoCompact": False},
    {"autoCompact": True, "freeSpaceTargetMB": "1"},
    {"autoCompact": True, "runOnce": 1},
    {"autoCompact": True, "runOnce": 1, "freeSpaceTargetMB": 0},
    {"autoCompact": True, "freeSpaceTargetMB": 0, "runOnce": 1},
    {"autoCompact": True, "runOnce": None},
    {"autoCompact": False},
    {"autoCompact": 1},
    {"autoCompact": "yes"},
    {"autoCompact": None},
    {"autoCompact": True, "bogus": 1},
    {"autoCompact": False, "bogus": 1},
    {"autoCompact": True, "writeConcern": {"w": 1}},
    {"autoCompact": True, "comment": "x"},
    {"autoCompact": False},
]


def _auto_replies(client: Any) -> list:
    out: list = []
    for step in AUTO_STEPS:
        if step == "idle":
            _wait_idle(client)
            continue
        assert isinstance(step, dict)
        out.append((step, reply(client.admin, step)))
        # A change sent before the server has taken up the last one is 314
        # ObjectIsBusy on both, which is a timing: each step is given time.
        # (These run while the files are small, before the large shape
        # below, so no pass is long.)
        time.sleep(0.3)
    out.append(("from another database", reply(client.rm192, {"autoCompact": True})))
    out.append(("bad, from another database", reply(client.rm192, {"autoCompact": 1})))
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_auto_compact_answers_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """`autoCompact` on, off, again and reconfigured, mongod 8.2.11's way:
    the same configuration again is no change, another while it runs is 23
    AlreadyInitialized, `runOnce` stops by itself, and the options are parsed
    in the order sent before the `admin`-only refusal."""
    ours, theirs = _auto_replies(rust_mongo), _auto_replies(mongod_rs)
    differ = [(a[0], a[1], b[1]) for a, b in zip(ours, theirs, strict=True) if a != b]
    assert not differ, differ


def _status(client: Any) -> dict:
    client.admin.command({"autoCompact": False})
    before = _bg_stats(client)
    client.admin.command({"autoCompact": True, "freeSpaceTargetMB": 1})
    deadline = time.monotonic() + 30
    while _bg_stats(client)["background compact running"] != 1:
        assert time.monotonic() < deadline
        time.sleep(0.1)
    client.admin.command({"autoCompact": False})
    deadline = time.monotonic() + 30
    while _bg_stats(client)["background compact running"] != 0:
        assert time.monotonic() < deadline
        time.sleep(0.1)
    after = _bg_stats(client)
    return {
        "counters": list(before),
        "types": [type(v).__name__ for v in before.values()],
        "turned off": after["background compact running"],
        "files tracked when off": after["number of files tracked by background compaction"],
    }


@pytest.mark.backlog_fixed("RM-219")
def test_auto_compact_reports_itself_in_server_status(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """`serverStatus.wiredTiger["background-compact"]`: WiredTiger's twelve
    counters in its order, `running` 1 while on and 0 once off."""
    assert _status(rust_mongo) == _status(mongod_rs)


def _noise_collection(db: Any, n: int) -> None:
    """`n` documents of 2,000 incompressible bytes, all but every tenth then
    deleted and a checkpoint taken: a file mostly free."""
    db.drop_collection("big")
    for start in range(0, n, 1000):
        db.big.insert_many(
            [{"_id": i, "p": os.urandom(2000)} for i in range(start, min(n, start + 1000))]
        )
    db.client.admin.command("fsync")
    db.big.delete_many({"$expr": {"$ne": [{"$mod": ["$_id", 10]}, 0]}})
    db.client.admin.command("fsync")


def _freed(db: Any, **options: Any) -> int:
    got = db.command({"compact": "big", "force": True, **options})
    return got.get("bytesFreed", got.get("estimatedBytesFreed"))


def _gives_back(client: Any) -> dict:
    db = client.rm192big
    _noise_collection(db, 20_000)
    out = {
        "dry run": _freed(db, dryRun=True),
        "target above the free space": _freed(db, freeSpaceTargetMB=100_000),
    }
    first = _freed(db)
    out["compact gave back over 20 MB"] = first > 20_000_000
    out["again, under 1 MB"] = _freed(db) < 1 << 20
    out["documents"] = db.big.count_documents({})
    client.drop_database("rm192big")
    return out


@pytest.mark.backlog_fixed("RM-192")
def test_compact_gives_a_mostly_deleted_collections_space_back(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """20,000 documents of 2 KB, 90% deleted: mongod 8.2.11 gives back about
    36 MB (the default target is 20 MB), estimates 0 on a dry run, and frees
    nothing above the target; a second compact has little left."""
    assert _gives_back(rust_mongo) == _gives_back(mongod_rs)


# ------------------------------------------------------------- --repair --


def _repair(argv: list[str]) -> int:
    return subprocess.run(argv, capture_output=True, timeout=300).returncode


def _start(store: str) -> tuple[subprocess.Popen, pymongo.MongoClient]:
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", store],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    assert proc.stdout is not None
    line, m = "", None
    for _ in range(20):
        line = proc.stdout.readline()
        m = _RS_BANNER.search(line)
        if m or not line:
            break
    assert m, f"secantusd-rs did not start: {line!r}"
    drain_output(proc)
    return proc, pymongo.MongoClient(host=m.group(1), port=int(m.group(2)))


def _stop(proc: subprocess.Popen, client: pymongo.MongoClient) -> None:
    client.close()
    proc.terminate()
    assert proc.wait(timeout=60) == 0


@pytest.mark.backlog_fixed("RM-192")
def test_repair_exits_as_mongod_does() -> None:
    """`--repair` on a healthy store repairs nothing and exits 0, and on a
    data directory that does not exist exits 100 (mongod 8.2.11's
    NonExistentPath), with mongod's `--repair` as the reference."""
    from tests.mongod_reference import MONGOD

    assert MONGOD, "no mongod on PATH: the reference server is required"
    work = tempfile.mkdtemp(prefix="rm192-repair-")
    try:
        ours, theirs = os.path.join(work, "rs"), os.path.join(work, "mongod")
        os.makedirs(theirs)
        proc, client = _start(ours)
        client.rm192.c.insert_many([{"_id": i} for i in range(100)])
        _stop(proc, client)
        got = {
            "healthy": _repair([str(RS_BINARY), "--storage-path", ours, "--repair"]),
            "missing": _repair(
                [str(RS_BINARY), "--storage-path", os.path.join(work, "x"), "--repair"]
            ),
        }
        want = {
            "healthy": _repair([MONGOD, "--dbpath", theirs, "--repair", "--quiet"]),
            "missing": _repair(
                [MONGOD, "--dbpath", os.path.join(work, "y"), "--repair", "--quiet"]
            ),
        }
        assert got == want
        proc, client = _start(ours)
        try:
            assert client.rm192.c.count_documents({}) == 100
        finally:
            _stop(proc, client)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def _damage(path: str) -> None:
    """Overwrite 16 KB in the middle of a file."""
    size = os.path.getsize(path)
    with open(path, "r+b") as f:
        f.seek(size // 2)
        f.write(b"\xde" * 16 * 1024)
        f.flush()
        os.fsync(f.fileno())


@pytest.mark.backlog_fixed("RM-192")
def test_repair_salvages_a_damaged_collection() -> None:
    """A store whose collection's file was damaged: `--repair` salvages what
    still reads back, rebuilds the collection's indexes from it and exits 0;
    the server then serves every surviving document, by `_id` and through
    its index, and `validate` finds them consistent. (mongod 8.2.11 salvages
    the same way and then aborts rebuilding the index -- fassert 7033800,
    measured 2026-10-26 -- so this is compared with what mongod sets out to
    do rather than with its exit.)"""
    work = tempfile.mkdtemp(prefix="rm192-salvage-")
    try:
        proc, client = _start(work)
        db = client.rm192
        db.c.create_index("k")
        for start in range(0, 5000, 500):
            db.c.insert_many(
                [{"_id": i, "k": i % 97, "p": os.urandom(1000)} for i in range(start, start + 500)]
            )
        ident = next(
            e["ident"]
            for e in client.admin.aggregate([{"$listCatalog": {}}])
            if e.get("ns") == "rm192.c"
        )
        _stop(proc, client)
        _damage(os.path.join(work, ident.removeprefix("table:") + ".wt"))

        done = subprocess.run(
            [str(RS_BINARY), "--storage-path", work, "--repair"],
            capture_output=True,
            text=True,
            timeout=300,
        )
        assert done.returncode == 0, done.stderr
        assert "Verify failed" in done.stderr and "Running a salvage operation" in done.stderr
        assert "Rebuilt the indexes of rm192.c" in done.stderr, done.stderr

        proc, client = _start(work)
        try:
            db = client.rm192
            kept = db.c.count_documents({})
            assert 4000 < kept < 5000, kept
            by_id = sum(1 for i in range(5000) if db.c.find_one({"_id": i}, {"_id": 1}))
            by_k = sum(db.c.count_documents({"k": k}, hint="k_1") for k in range(97))
            assert by_id == by_k == kept
            assert db.command("validate", "c")["valid"] is True
        finally:
            _stop(proc, client)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def _startup_log(client: Any) -> dict:
    local = client.local
    exists = "startup_log" in local.list_collection_names()
    last = local.startup_log.find_one(sort=[("$natural", -1)]) if exists else None
    return {
        "exists": exists,
        "fields": sorted(last or {}),
        "capped": exists and local.startup_log.options().get("capped", False),
    }


@pytest.mark.backlog("RM-319")
def test_local_startup_log_records_each_start(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 keeps a capped `local.startup_log`, one document per
    start: `_id`, `hostname`, `startTime`, `startTimeLocal`, `cmdLine`,
    `pid`, `buildinfo`."""
    assert _startup_log(rust_mongo) == _startup_log(mongod_rs)
