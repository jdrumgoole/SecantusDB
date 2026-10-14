"""Differential-probe five administrative commands against mongod.

`logRotate`, `connPoolStats`, `dbHash`, `getDefaultRWConcern` /
`setDefaultRWConcern` and `setFeatureCompatibilityVersion`: their replies,
their refusals, what they persist, and what a stored default write concern
does to the writes that follow. Both servers are started by the probe on
empty stores, so a scenario's state is its own:

    PROBE_RUST_BIN=crates/secantusdb/target/debug/secantusd-rs \\
        python tools/probes/admin_commands.py

`PROBE_MONGOD_BIN` names another mongod (default: the one on `PATH`). The
mongod is a one-member replica set logging to its stdout, which is what the
server under test says it is and has: `setDefaultRWConcern` is refused on a
standalone, and `logRotate` fails on a mongod with a `--logpath` whose
rotated name already exists.

Only what changes from run to run is normalised: the host name, elapsed
times, wall-clock times, op times and collection UUIDs are compared by
BSON type. A hash is compared exactly -- the point of `dbHash` is that two
servers holding the same documents answer the same digest.

The self-check: on mongod, `hello` says replica-set primary and a `dbHash`
of two known documents is the MD5 this probe computes for them. It changes
no state, so the first `getDefaultRWConcern` sees a server nothing was set
on.
"""

import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import bson  # noqa: E402
import pymongo  # noqa: E402
from _wire import Wire  # noqa: E402
from bson import Binary, Decimal128, Int64, Timestamp  # noqa: E402
from pymongo.errors import OperationFailure, PyMongoError  # noqa: E402

MONGOD_BIN = os.environ.get("PROBE_MONGOD_BIN", "mongod")
RUST_BIN = os.environ.get("PROBE_RUST_BIN")
PORTS = {"mongod": 27463, "rust": 27464}

NOISE = ("$clusterTime", "operationTime", "opTime", "electionId")
VOLATILE = {
    "host",
    "timeMillis",
    "updateOpTime",
    "updateWallClockTime",
    "localUpdateWallClockTime",
}

A = "admin"
S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, list(steps)))


# ------------------------------------------------------------------ logRotate

scenario(
    "logRotate: the value names the log",
    *[(A, {"logRotate": v}) for v in (1, 0, 1.5, Int64(1), Decimal128("1"), True, False)],
    *[(A, {"logRotate": v}) for v in ("server", "audit", "bogus", "", "SERVER", "server ")],
    *[(A, {"logRotate": v}) for v in (None, {"a": 1}, [1], Binary(b"x"), bson.Regex("x"))],
    (A, {"logRotate": 1, "bogus": 1}),
    (A, {"logRotate": {"a": 1}, "bogus": 1}),
    (A, {"logRotate": "server", "comment": 1}),
    (A, {"logRotate": "server", "maxTimeMS": 10}),
    (A, {"logRotate": "server", "writeConcern": {"w": 1}}),
    (A, {"logRotate": "server", "readConcern": {"level": "local"}}),
    {"logRotate": 1},
    (A, {"logrotate": 1}),
    (A, {"LogRotate": 1}),
)

# ------------------------------------------------------------------ connPoolStats

scenario(
    "connPoolStats: one fixed shape, on any database",
    (A, {"connPoolStats": 1}),
    {"connPoolStats": 1},
    (A, {"connPoolStats": "x"}),
    (A, {"connPoolStats": 1, "bogus": 1}),
    (A, {"connPoolStats": 1, "comment": "x"}),
    (A, {"connPoolStats": 1, "writeConcern": {"w": 1}}),
    ("local", {"connPoolStats": 1}),
)

# ------------------------------------------------------------------ dbHash

HASH_SEED = [
    {
        "insert": "a",
        "documents": [{"_id": 3, "x": "c"}, {"_id": 1, "x": "a"}, {"_id": 2, "x": "b"}],
    },
    {"insert": "b", "documents": [{"y": 1, "_id": "later"}]},
    {"insert": "b", "documents": [{"_id": {"k": 1}, "n": [1, 2.0, Int64(3)]}]},
    {"update": "b", "updates": [{"q": {"_id": "later"}, "u": {"$set": {"z": 2}}}]},
    {"create": "cap", "capped": True, "size": 4096},
    {"insert": "cap", "documents": [{"_id": 2}, {"_id": 1}]},
    {"create": "v", "viewOn": "a", "pipeline": []},
    {"create": "coll", "collation": {"locale": "en", "strength": 2}},
    {"insert": "coll", "documents": [{"_id": "b"}, {"_id": "C"}, {"_id": "a"}]},
    {
        "insert": "mixed",
        "documents": [
            {"_id": "s"},
            {"_id": 1},
            {"_id": None},
            {"_id": 1.5},
            {"_id": True},
            {"_id": bson.datetime.datetime(2020, 1, 1)},
            {"_id": bson.ObjectId("5f0000000000000000000000")},
            {"_id": {"a": 1}},
            {"_id": Binary(b"x")},
            {"_id": Timestamp(1, 1)},
            {"_id": Int64(2)},
            {"_id": Decimal128("1.50")},
            {"_id": -0.0},
        ],
    },
    {"insert": "empty", "documents": [{"_id": 1}]},
    {"delete": "empty", "deletes": [{"q": {}, "limit": 0}]},
    {"create": "emptyc"},
    {"profile": 2},
    {"find": "a", "filter": {"_id": 1}},
    {"profile": 0},
    {"insert": "big", "documents": [{"_id": i, "p": "x" * 100} for i in range(500)]},
]

scenario(
    "dbHash: the hashes of a seeded database",
    *HASH_SEED,
    {"dbHash": 1},
    {"dbHash": 1, "collections": ["a", "nosuch", "v", "system.views", "system.profile"]},
    {"dbHash": 1, "collections": ["b", "a"]},
    {"dbHash": 1, "collections": ["cap", "cap"]},
    {"dbHash": 1, "collections": "x"},
    {"dbHash": 1, "collections": [1]},
    {"dbHash": 1, "collections": ["a", 1]},
    {"dbHash": 1, "collections": []},
    {"dbHash": 1, "collections": {}},
    {"dbHash": 1, "collections": None},
    {"dbHash": 1, "bogus": 1},
    {"dbHash": "x"},
    {"dbhash": 1},
    {"dbHash": 1, "comment": "hi"},
    {"dbHash": 1, "writeConcern": {"w": 1}},
    {"dbHash": 1, "maxTimeMS": 10000},
    ("nosuchdb_zz", {"dbHash": 1}),
    ("local", {"dbHash": 1}),
    ("local", {"insert": "stuff", "documents": [{"_id": 1}]}),
    ("local", {"dbHash": 1}),
)

scenario(
    "dbHash: the system databases",
    (A, {"dbHash": 1}),
    ("config", {"dbHash": 1}),
)

# ------------------------------------------------------------------ getDefaultRWConcern

scenario(
    "getDefaultRWConcern: nothing set",
    (A, {"getDefaultRWConcern": 1}),
    (A, {"getDefaultRWConcern": 1, "inMemory": True}),
    (A, {"getDefaultRWConcern": 1, "inMemory": False}),
    (A, {"getDefaultRWConcern": 1, "inMemory": None}),
    (A, {"getDefaultRWConcern": 1, "inMemory": "x"}),
    (A, {"getDefaultRWConcern": 1, "inMemory": 1}),
    (A, {"getDefaultRWConcern": 1, "bogus": 1}),
    (A, {"getDefaultRWConcern": "x"}),
    (A, {"getDefaultRWConcern": 1, "comment": "x"}),
    (A, {"getDefaultRWConcern": 1, "writeConcern": {"w": 1}}),
    {"getDefaultRWConcern": 1},
)

# ------------------------------------------------------------------ setDefaultRWConcern

RC = "defaultReadConcern"
WC = "defaultWriteConcern"
SET = "setDefaultRWConcern"

scenario(
    "setDefaultRWConcern: refused, nothing stored",
    {SET: 1, RC: {"level": "local"}},
    (A, {SET: 1}),
    (A, {SET: 1, "bogus": 1}),
    (A, {SET: 1, RC: None}),
    (A, {SET: 1, WC: None}),
    (A, {SET: 1, RC: None, WC: None}),
    (A, {SET: 1, RC: {"level": "bogus"}}),
    (A, {SET: 1, RC: {"level": "LOCAL"}}),
    (A, {SET: 1, RC: {"level": "snapshot"}}),
    (A, {SET: 1, RC: {"level": "linearizable"}}),
    (A, {SET: 1, RC: {"level": "majority", "afterClusterTime": Timestamp(1, 1)}}),
    (A, {SET: 1, RC: {"level": "majority", "afterClusterTime": 1}}),
    (A, {SET: 1, RC: {"level": "local", "afterOpTime": {"ts": Timestamp(1, 1), "t": Int64(1)}}}),
    (A, {SET: 1, RC: {"level": "local", "atClusterTime": Timestamp(1, 1)}}),
    (A, {SET: 1, RC: {"level": "majority", "provenance": "x"}}),
    (A, {SET: 1, RC: {"level": "majority", "provenance": "clientSupplied"}}),
    (A, {SET: 1, RC: {"level": 1}}),
    (A, {SET: 1, RC: {"bogus": 1}}),
    (A, {SET: 1, RC: "x"}),
    (A, {SET: 1, WC: {"w": 0}}),
    (A, {SET: 1, WC: {"w": -1}}),
    (A, {SET: 1, WC: {"w": 51}}),
    (A, {SET: 1, WC: {"w": Int64(2**40)}}),
    (A, {SET: 1, WC: {"w": "bogus"}}),
    (A, {SET: 1, WC: {"w": "MAJORITY"}}),
    (A, {SET: 1, WC: {"w": ""}}),
    (A, {SET: 1, WC: {"w": None}}),
    (A, {SET: 1, WC: {"w": True}}),
    (A, {SET: 1, WC: {"w": {}}}),
    (A, {SET: 1, WC: {"w": 1, "bogus": 1}}),
    (A, {SET: 1, WC: {"wtimeout": -5}}),
    (A, {SET: 1, WC: {"j": True}}),
    (A, {SET: 1, WC: {"w": 1, "j": True, "fsync": True}}),
    (A, {SET: 1, WC: {"w": 1, "j": "x"}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": Int64(2**40)}}),
    (A, {SET: 1, WC: {"w": 1, "provenance": "x"}}),
    (A, {SET: 1, WC: {"w": 1, "provenance": "clientSupplied"}}),
    (A, {SET: 1, WC: "x"}),
    (A, {SET: 1, RC: {"level": "snapshot"}, WC: {"w": 0}}),
    (A, {SET: 1, RC: {"level": "snapshot"}, WC: {"w": True}}),
    (A, {SET: 1, RC: {"level": "snapshot"}, WC: {"w": -1}}),
    (A, {SET: 1, RC: {"level": "snapshot"}, WC: {"w": "bogus"}}),
    (A, {SET: 1, RC: {"level": "snapshot"}, WC: {"w": {}}}),
    (A, {SET: 1, RC: {"level": "snapshot"}, WC: {"j": True}}),
    (A, {SET: 1, RC: {"level": "snapshot"}, WC: {"w": 1, "j": True, "fsync": True}}),
    (A, {SET: 1, RC: {"level": "snapshot"}, WC: {"w": 1, "provenance": "clientSupplied"}}),
    (A, {SET: 1, RC: {"bogus": 1}, WC: {"w": 0}}),
    (A, {SET: 1, RC: {"level": "local"}, WC: {"w": 0, "bogus": 1}}),
    (A, {SET: 1, RC: "x", WC: {"w": 0}}),
    (A, {SET: 1, RC: {"level": 1}, WC: "x"}),
    (A, {SET: 1, RC: {"level": "local"}, "bogus": 1}),
    (A, {SET: 1, WC: {"w": 1, "j": True, "fsync": True}, "bogus": 1}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": -5}, "bogus": 1}),
    (A, {SET: 1, WC: {"w": 1}, RC: "x"}),
    (A, {SET: 1, WC: "y", RC: "x"}),
    (A, {SET: 1, WC: {"w": 0}, RC: {"level": "local"}}),
    (A, {SET: 1, WC: {"w": 0}, RC: {"level": "snapshot"}}),
    (A, {SET: 1, WC: {"w": 1, "bogus": 1}, RC: {"level": "snapshot"}}),
    (A, {SET: 1, WC: {"w": 1, "bogus": 1}, RC: {"bogus": 1}}),
    (A, {SET: 1, WC: {"w": 1, "bogus": 1}, RC: 5}),
    (A, {SET: 1, WC: {"w": 1}, RC: {"level": "local"}, "maxTimeMS": "x"}),
    (A, {SET: 1, RC: {"level": "local"}, "writeConcern": {"w": "bogus"}}),
    (A, {"getDefaultRWConcern": 1}),
    ("config", {"find": "settings"}),
)

scenario(
    "setDefaultRWConcern: stored and read back",
    (A, {SET: 1, WC: {}}),
    (A, {"getDefaultRWConcern": 1}),
    (A, {SET: 1, RC: {"level": "available"}}),
    (A, {"getDefaultRWConcern": 1}),
    (A, {"getDefaultRWConcern": 1, "inMemory": True}),
    (A, {SET: "x", RC: {"level": "majority"}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": "x"}}),
    (A, {SET: 1, WC: {"w": 1.5}}),
    (A, {SET: 1, WC: {"w": "majority", "wtimeout": 500, "j": False}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": 0}}),
    (A, {SET: 1, WC: {"w": 1, "j": True}}),
    (A, {SET: 1, WC: {"w": 1, "j": 1}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": Int64(7)}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": 7.0}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": 2.7}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": None}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": True}}),
    (A, {SET: 1, WC: {"w": 1, "wtimeout": -5}}),
    (A, {SET: 1, WC: {"w": 1.0}}),
    (A, {SET: 1, WC: {"w": Int64(1)}}),
    (A, {SET: 1, WC: {"w": Decimal128("1")}}),
    (A, {SET: 1, WC: {"w": 1, "fsync": True}}),
    (A, {SET: 1, WC: {"w": 1, "fsync": False}}),
    (A, {SET: 1, WC: {"w": 1, "getLastError": 1}}),
    (A, {SET: 1, WC: {"w": "majority"}}),
    (A, {SET: 1, WC: {"w": 1}, "writeConcern": {"w": 1}}),
    (A, {SET: 1, WC: {"w": 1}, "comment": "c"}),
    (A, {SET: 1, RC: {"level": "local"}, WC: {"w": 1}}),
    (A, {SET: 1, RC: {"level": "majority"}, WC: {"w": "majority"}}),
    (A, {SET: 1, RC: {"level": "majority"}, WC: {"w": 1}}),
    (A, {SET: 1, RC: {}}),
    (A, {"getDefaultRWConcern": 1}),
    (A, {SET: 1, RC: {"level": None}}),
    (A, {SET: 1, WC: {}}),
    (A, {SET: 1, RC: {}, WC: {}}),
    (A, {SET: 1, RC: {}, WC: {"w": 1}}),
    (A, {SET: 1, RC: {"level": "local"}, WC: {"w": 1, "wtimeout": 250}}),
    (A, {"getDefaultRWConcern": 1}),
    ("config", {"find": "settings"}),
)

scenario(
    "setDefaultRWConcern: a default no node can satisfy",
    (A, {SET: 1, WC: {"w": 50}}),
    (A, {SET: 1, WC: {"w": "majority", "j": True}, RC: {"level": "local"}}),
    (A, {SET: 1, WC: {"w": 3}}),
    (A, {"getDefaultRWConcern": 1}),
    {"insert": "c", "documents": [{"_id": 1}]},
    {"insert": "c", "documents": [{"_id": 2}], "writeConcern": {"w": 1}},
    {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 1}}}]},
    {"delete": "c", "deletes": [{"q": {"_id": 2}, "limit": 1}]},
    {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"b": 1}}},
    {"find": "c"},
    {"count": "c"},
    {"create": "d"},
    {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
    {"dropIndexes": "c", "index": "a_1"},
    {"drop": "d"},
    {"aggregate": "c", "pipeline": [], "cursor": {}},
    {"aggregate": "c", "pipeline": [{"$out": "e"}], "cursor": {}},
    {"collMod": "c", "validationLevel": "off"},
    (A, {"renameCollection": "@db.e", "to": "@db.f"}),
    (A, {"createUser": "u1", "pwd": "x", "roles": []}),
    (A, {"dropUser": "u1"}),
    (A, {"setFeatureCompatibilityVersion": "8.2", "confirm": True}),
    (A, {"setParameter": 1, "logLevel": 0}),
    (A, {"logRotate": 1}),
    (A, {"ping": 1}),
    (A, {SET: 1, WC: {"w": 1}}),
    {"insert": "c", "documents": [{"_id": 3}]},
    (A, {"getDefaultRWConcern": 1}),
    (A, {SET: 1, WC: {"w": {"dc": 1}}}),
    {"insert": "c", "documents": [{"_id": 4}]},
    (A, {SET: 1, WC: {"w": 1}}),
    {"insert": "c", "documents": [{"_id": 5}]},
    (A, {SET: 1, WC: {"w": 3}}),
    {"dropDatabase": 1},
    (A, {SET: 1, WC: {"w": 1}}),
)

# ------------------------------------------------------------------ setFeatureCompatibilityVersion

FCV = "setFeatureCompatibilityVersion"
GET_FCV = {"getParameter": 1, "featureCompatibilityVersion": 1}

scenario(
    "setFeatureCompatibilityVersion: the arguments",
    {FCV: "8.2"},
    (A, GET_FCV),
    (A, {FCV: "8.2"}),
    (A, {FCV: "8.2", "confirm": True}),
    (A, {FCV: "8.2", "confirm": False}),
    (A, {FCV: "8.2", "confirm": 1}),
    (A, {FCV: "8.2", "confirm": 0}),
    (A, {FCV: "8.2", "confirm": 2.5}),
    (A, {FCV: "8.2", "confirm": Decimal128("1")}),
    (A, {FCV: "8.2", "confirm": None}),
    (A, {FCV: "8.2", "confirm": "yes"}),
    (A, {FCV: "8.0"}),
    (A, {FCV: "7.0"}),
    (A, {FCV: "6.0", "confirm": True}),
    (A, {FCV: "8.3", "confirm": True}),
    (A, {FCV: "9.0", "confirm": True}),
    (A, {FCV: "abc", "confirm": True}),
    (A, {FCV: "", "confirm": True}),
    (A, {FCV: " 8.2", "confirm": True}),
    (A, {FCV: "8.2.0", "confirm": True}),
    (A, {FCV: 1, "confirm": True}),
    (A, {FCV: None, "confirm": True}),
    (A, {FCV: "8.2", "confirm": True, "bogus": 1}),
    (A, {FCV: 1, "bogus": 1}),
    (A, {FCV: "7.0", "bogus": 1}),
    (A, {FCV: "7.0", "confirm": "x"}),
    (A, {FCV: "8.2", "confirm": True, "fromConfigServer": True}),
    (A, {FCV: "8.2", "confirm": True, "fromConfigServer": None}),
    (A, {FCV: "8.2", "confirm": True, "fromConfigServer": "x"}),
    (A, {FCV: "8.2", "confirm": True, "dryRun": True}),
    (A, {FCV: "8.2", "confirm": True, "dryRun": False}),
    (A, {FCV: "8.2", "confirm": True, "dryRun": 0}),
    (A, {FCV: "8.2", "confirm": True, "dryRun": None}),
    (A, {FCV: "8.2", "confirm": True, "dryRun": "x"}),
    (A, {FCV: "8.2", "dryRun": True}),
    (A, {FCV: "8.0", "confirm": True, "dryRun": True}),
    (A, {FCV: "7.0", "confirm": True, "dryRun": True}),
    (A, {FCV: "8.2", "confirm": True, "skipDryRun": True}),
    (A, {FCV: "8.2", "confirm": True, "comment": {"a": 1}}),
    (A, {FCV: "8.2", "confirm": True, "writeConcern": {"w": 1}}),
    (A, {FCV: "8.2", "confirm": True, "maxTimeMS": 1000}),
    (A, GET_FCV),
    (A, {"getParameter": 1, "featureCompatibilityVersion": 1, "showDetails": True}),
)

scenario(
    "setFeatureCompatibilityVersion: the transitions a binary of 8.2 allows",
    (A, {FCV: "8.1"}),
    (A, {FCV: "8.1", "confirm": True}),
    (A, {FCV: "8.0", "confirm": True}),
    (A, GET_FCV),
    (A, {"find": "system.version", "filter": {"_id": "featureCompatibilityVersion"}}),
    (A, {FCV: "8.1"}),
    (A, {FCV: "8.1", "confirm": True}),
    (A, {FCV: "8.0"}),
    (A, {FCV: "8.0", "confirm": True}),
    (A, {FCV: "8.2"}),
    (A, {FCV: "8.2", "confirm": True}),
    (A, GET_FCV),
    (A, {FCV: "8.0", "confirm": True, "comment": "x"}),
    (A, GET_FCV),
    (A, {FCV: "8.2", "confirm": True}),
    (A, {"find": "system.version", "filter": {"_id": "featureCompatibilityVersion"}}),
)


# ------------------------------------------------------------------ running


def start(kind: str, home: str):
    port = PORTS[kind]
    if kind == "mongod":
        argv = [
            MONGOD_BIN,
            "--replSet",
            "probe",
            "--port",
            str(port),
            "--dbpath",
            home,
            "--bind_ip",
            "127.0.0.1",
        ]
    else:
        argv = [RUST_BIN, "--port", str(port), "--storage-path", home]
    with open(os.path.join(home, "probe-server.log"), "w") as log:
        proc = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT)
    uri = f"mongodb://127.0.0.1:{port}/?directConnection=true"
    for _ in range(300):
        try:
            client = pymongo.MongoClient(uri, serverSelectionTimeoutMS=200)
            client.admin.command("ping")
            break
        except PyMongoError:
            if proc.poll() is not None:
                break
            time.sleep(0.1)
    else:
        proc.kill()
        raise SystemExit(f"{kind} did not start; see {home}/probe-server.log")
    if kind == "mongod":
        client.admin.command(
            "replSetInitiate",
            {"_id": "probe", "members": [{"_id": 0, "host": f"127.0.0.1:{port}"}]},
        )
        for _ in range(300):
            if client.admin.command("hello").get("isWritablePrimary"):
                break
            time.sleep(0.1)
        else:
            proc.kill()
            raise SystemExit("mongod did not become primary")
    client.close()
    return proc


def kind(value):
    return f"<{type(value).__name__}>"


def clean(value, key=None):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k in NOISE:
                continue
            if k in VOLATILE:
                out[k] = kind(v)
            elif k == "uuids" and isinstance(v, dict):
                out[k] = {name: kind(u) for name, u in v.items()}
            elif k == "id" and key == "cursor":
                out[k] = "CURSOR" if v else 0
            else:
                out[k] = clean(v, k)
        return out
    if isinstance(value, list):
        return [clean(v, key) for v in value]
    if isinstance(value, float) and value != value:
        return "NaN"
    if isinstance(value, str):
        # The order of an expected-type list changes between mongod patch
        # releases; an opTime in a message is the moment's.
        text = re.sub(
            r"expected types '\[([^\]]*)\]'",
            lambda m: "expected types '[" + ", ".join(sorted(m.group(1).split(", "))) + "]'",
            value,
        )
        return re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(T)", text)
    return value


def resolve(value, db):
    if isinstance(value, dict):
        return {k: resolve(v, db) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve(v, db) for v in value]
    if isinstance(value, str) and "@db" in value:
        return value.replace("@db", db)
    return value


def run_scenario(uri, index, steps):
    db = f"adminp_{index}"
    wire = Wire(uri)
    replies = []
    for step in steps:
        where, command = step if isinstance(step, tuple) else (None, step)
        where = db if where is None else where
        try:
            reply = wire.command(where, resolve(dict(command), db))
        except (TimeoutError, ConnectionError, OSError) as exc:
            wire.close()
            wire = Wire(uri)
            replies.append({"probe": f"CONNECTION {type(exc).__name__}"})
            continue
        replies.append(clean(reply))
    wire.close()
    return replies


def expected_hash(docs):
    """The MD5 mongod answers for `docs`, in `_id` order."""
    h = hashlib.md5()
    for d in sorted(docs, key=lambda d: d["_id"]):
        h.update(bson.encode(d))
    return h.hexdigest()


def self_check(uri):
    docs = [{"_id": 2, "v": "two"}, {"_id": 1, "v": "one"}]
    got = run_scenario(
        uri,
        0,
        [
            (A, {"hello": 1}),
            {"insert": "c", "documents": docs},
            {"dbHash": 1},
            {"dropDatabase": 1},
        ],
    )
    problems = []
    if not got[0].get("isWritablePrimary") or not got[0].get("setName"):
        problems.append(f"not a replica-set primary: {got[0]}")
    want = expected_hash(docs)
    if got[2].get("collections", {}).get("c") != want:
        problems.append(f"dbHash of two documents is not {want}: {got[2]}")
    return problems


def show(step):
    where, command = step if isinstance(step, tuple) else (None, step)
    return (("" if where is None else f"[{where}] ") + repr(command))[:200]


def main():
    if not RUST_BIN:
        print("PROBE_RUST_BIN is required (see the module docstring)", file=sys.stderr)
        return 2
    width = int(os.environ.get("PROBE_WIDTH", "1500"))
    only = os.environ.get("PROBE_ONLY")
    homes = {k: tempfile.mkdtemp(prefix=f"admin-probe-{k}-") for k in PORTS}
    procs = {}
    try:
        for k in PORTS:
            procs[k] = start(k, homes[k])
        uris = {k: f"mongodb://127.0.0.1:{p}/?directConnection=true" for k, p in PORTS.items()}
        problems = self_check(uris["mongod"])
        if problems:
            print("SELF-CHECK FAILED on mongod:", file=sys.stderr)
            for p in problems:
                print(f"  {p}", file=sys.stderr)
            return 2
        divergent = replies = replies_bad = total = 0
        for index, (name, steps) in enumerate(S, start=1):
            if only and only not in name:
                continue
            total += 1
            want = run_scenario(uris["mongod"], index, steps)
            got = run_scenario(uris["rust"], index, steps)
            replies += len(steps)
            bad = [i for i, (w, g) in enumerate(zip(want, got, strict=True)) if repr(w) != repr(g)]
            if not bad:
                continue
            divergent += 1
            replies_bad += len(bad)
            print(f"\n--- {name}")
            for i in bad:
                print(f"  step {i}: {show(steps[i])}")
                print(f"    mongod: {str(want[i])[:width]}")
                print(f"    rust  : {str(got[i])[:width]}")
        print(
            f"\n=== admin commands: {total} scenarios, {replies} replies -- "
            f"{divergent} scenarios and {replies_bad} replies differ ==="
        )
        return 1 if divergent else 0
    finally:
        for proc in procs.values():
            proc.terminate()
        for proc in procs.values():
            try:
                proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                proc.kill()
        for home in homes.values():
            shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OperationFailure as exc:
        print(f"probe failed: {exc}", file=sys.stderr)
        sys.exit(2)
