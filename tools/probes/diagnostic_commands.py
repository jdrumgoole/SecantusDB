"""Differential-probe the handshake, diagnostic and session commands against mongod.

The commands a driver or a tool sends that are about the server and the
connection, not about a collection's documents: `hello` and its older
names, `ping`, `buildInfo`, `connectionStatus`, `whatsmyuri`,
`getParameter`, `serverStatus`, `hostInfo`, `getCmdLineOpts`, `getLog`,
`currentOp`, the statistics commands (`dbStats`, `collStats`, `dataSize`,
`validate`, `listDatabases`), the session commands, the fields every
command takes (`$db`, `lsid`, `readConcern`, `writeConcern`, `maxTimeMS`,
`comment`, `apiVersion`), the replica-set status commands, and the commands
a server of this version no longer has.

A reply of this kind is mostly values that differ from one machine to the
next, so a step is compared one of two ways:

- **exact**, the default: the whole reply, with the keys in `VOLATILE`
  replaced by the BSON type of their value;
- **shape** (`shape(command, depth)`): the keys and the BSON type of each
  value, to `depth` levels. An error reply is always compared exactly.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/diagnostic_commands.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it. The
reference must be a replica-set member, as the server under test says it is.
"""

import os
import re
import sys
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Binary, Decimal128, Int64, ObjectId, Timestamp  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
NOISE = ("$clusterTime", "operationTime", "opTime")

#: Keys whose value is the machine's, the process's or the moment's. The
#: BSON type is still compared.
VOLATILE = {
    "connectionId",
    "localTime",
    "you",
    "pid",
    "host",
    "hosts",
    "me",
    "primary",
    "setName",
    "processId",
    "counter",
    "electionId",
    "lastWrite",
    "sizeOnDisk",
    "totalSize",
    "totalSizeMb",
    "storageSize",
    "totalIndexSize",
    "indexSize",
    "indexSizes",
    "freeStorageSize",
    "totalFreeStorageSize",
    "indexFreeStorageSize",
    "fsUsedSize",
    "fsTotalSize",
    "millis",
    "numYields",
    "uptime",
    "uptimeMillis",
    "uptimeEstimate",
    "id",
    "uuid",
    "atClusterTime",
    "uuids",
}

A = "admin"
#: Database names a server may or may not take; dropped after every scenario.
ODD_NAMES = ["a.b", "a b", "a$b", "A" * 64]


def shape(step, depth=1):
    """Compare this step by the keys and types of its reply, `depth` deep."""
    if isinstance(step, tuple):
        return (step[0], {**step[1], "shape": depth})
    return {**step, "shape": depth}


S: list[tuple[str, list]] = []

SEED = [
    {
        "insert": "c",
        "documents": [{"_id": 1, "v": 5}, {"_id": 2, "v": "x"}, {"_id": 3, "v": [1, 2]}],
    },
    {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v_1"}]},
]


def scenario(name, *steps, seed=False):
    S.append((name, (SEED if seed else []) + list(steps)))


VALUES = [1, 0, -1, 2.5, "x", "", None, True, False, {}, [], Int64(1), Decimal128("1")]
BAD_BOOL = ["x", 2, 0, None, {}, [], 1.5]
BAD_INT = ["x", None, True, {}, [], 2.5, -1, Int64(2**40)]
BAD_DOC = [1, "x", None, True, []]

# ------------------------------------------------------------------ hello

CLIENT = {
    "driver": {"name": "probe", "version": "1.0"},
    "os": {"type": "Darwin", "name": "macOS", "architecture": "arm64", "version": "15"},
    "platform": "probe",
    "application": {"name": "diag"},
}

scenario(
    "hello: the three names and their value",
    *[{"hello": v} for v in VALUES],
    {"isMaster": 1},
    {"ismaster": 1},
    {"ISMASTER": 1},
    {"Hello": 1},
    (A, {"hello": 1}),
    ("local", {"hello": 1}),
    ("nosuchdb", {"isMaster": 1}),
)
scenario(
    "hello: helloOk, and what the older names answer after it",
    {"isMaster": 1, "helloOk": True},
    {"isMaster": 1},
    {"hello": 1, "helloOk": True},
    *[{"hello": 1, "helloOk": v} for v in BAD_BOOL],
    *[{"isMaster": 1, "helloOk": v} for v in ["x", 1, None]],
)
scenario(
    "hello: client metadata on the first hello",
    {"hello": 1, "client": CLIENT},
    {"hello": 1, "client": CLIENT},
    {"isMaster": 1, "client": CLIENT},
    {"hello": 1},
)
for label, client in [
    ("not a document", 1),
    ("a string", "x"),
    ("null", None),
    ("empty", {}),
    ("no driver", {"os": {"type": "x"}}),
    ("no os", {"driver": {"name": "p", "version": "1"}}),
    ("driver not a document", {"driver": 1, "os": {"type": "x"}}),
    ("driver without a name", {"driver": {"version": "1"}, "os": {"type": "x"}}),
    ("driver without a version", {"driver": {"name": "p"}, "os": {"type": "x"}}),
    ("driver name not a string", {"driver": {"name": 1, "version": "1"}, "os": {"type": "x"}}),
    ("os not a document", {"driver": {"name": "p", "version": "1"}, "os": 1}),
    ("os without a type", {"driver": {"name": "p", "version": "1"}, "os": {"name": "x"}}),
    ("os type not a string", {"driver": {"name": "p", "version": "1"}, "os": {"type": 1}}),
    (
        "application not a document",
        {"driver": {"name": "p", "version": "1"}, "os": {"type": "x"}, "application": 1},
    ),
    (
        "application name not a string",
        {"driver": {"name": "p", "version": "1"}, "os": {"type": "x"}, "application": {"name": 1}},
    ),
    (
        "application name of 129 bytes",
        {
            "driver": {"name": "p", "version": "1"},
            "os": {"type": "x"},
            "application": {"name": "a" * 129},
        },
    ),
    (
        "application name of 128 bytes",
        {
            "driver": {"name": "p", "version": "1"},
            "os": {"type": "x"},
            "application": {"name": "a" * 128},
        },
    ),
    (
        "an unknown field",
        {"driver": {"name": "p", "version": "1"}, "os": {"type": "x"}, "bogus": 1},
    ),
    (
        "over 512 bytes",
        {"driver": {"name": "p", "version": "1"}, "os": {"type": "x"}, "platform": "p" * 600},
    ),
    (
        "a duplicate-free minimum",
        {"driver": {"name": "p", "version": "1"}, "os": {"type": "x"}},
    ),
]:
    scenario(f"hello: client metadata, {label}", {"hello": 1, "client": client}, {"ping": 1})

scenario(
    "hello: the other fields a driver sends",
    {"hello": 1, "compression": []},
    {"hello": 1, "compression": ["zlib"]},
    {"hello": 1, "compression": ["snappy", "zstd", "zlib"]},
    {"hello": 1, "compression": ["noop"]},
    {"hello": 1, "compression": ["bogus"]},
    {"hello": 1, "compression": "zlib"},
    {"hello": 1, "compression": [1]},
    {"hello": 1, "saslSupportedMechs": "admin.nobody"},
    {"hello": 1, "saslSupportedMechs": "nodot"},
    {"hello": 1, "saslSupportedMechs": 1},
    {"hello": 1, "saslSupportedMechs": ""},
    {"hello": 1, "loadBalanced": True},
    {"hello": 1, "loadBalanced": False},
    {"hello": 1, "loadBalanced": "x"},
    {"hello": 1, "comment": "x"},
    {"hello": 1, "comment": {"a": 1}},
    {"hello": 1, "bogus": 1},
    {"hello": 1, "hostInfo": "x:1"},
    {"hello": 1, "forShell": 1},
    {"hello": 1, "internalClient": 1},
    {"hello": 1, "hangUpOnStepDown": False},
    {"hello": 1, "maxAwaitTimeMS": 5},
    {"hello": 1, "topologyVersion": 1},
    {"hello": 1, "topologyVersion": {}},
    {"hello": 1, "topologyVersion": {"processId": ObjectId(), "counter": Int64(0)}},
    {"hello": 1, "speculativeAuthenticate": 1},
    {"hello": 1, "speculativeAuthenticate": {}},
    {
        "hello": 1,
        "speculativeAuthenticate": {
            "saslStart": 1,
            "mechanism": "SCRAM-SHA-256",
            "payload": Binary(b"n,,n=u,r=abc"),
            "db": "admin",
        },
    },
    {"hello": 1, "apiVersion": "1"},
    {"hello": 1, "apiVersion": "1", "apiStrict": True},
    {"isMaster": 1, "apiVersion": "1", "apiStrict": True},
)

# ------------------------------------------------------------------- ping

scenario(
    "ping: its value, extra fields and the database",
    *[{"ping": v} for v in VALUES],
    {"ping": 1, "bogus": 1},
    {"ping": 1, "comment": "x"},
    {"ping": 1, "comment": None},
    (A, {"ping": 1}),
    ("local", {"ping": 1}),
    ("config", {"ping": 1}),
    {"Ping": 1},
    {"PING": 1},
)
scenario(
    "apiVersion and its companions",
    *[{"ping": 1, "apiVersion": v} for v in ["1", "2", "", 1, None, True, {}, "1.0", "v1"]],
    *[{"ping": 1, "apiVersion": "1", "apiStrict": v} for v in [True, False, *BAD_BOOL]],
    *[{"ping": 1, "apiVersion": "1", "apiDeprecationErrors": v} for v in [True, False, "x", 1]],
    {"ping": 1, "apiStrict": True},
    {"ping": 1, "apiDeprecationErrors": True},
    {"ping": 1, "apiStrict": False},
    {"find": "c", "apiVersion": "1", "apiStrict": True},
    {"find": "c", "apiVersion": "1", "apiStrict": True, "tailable": True},
    {"count": "c", "apiVersion": "1", "apiStrict": True},
    {"count": "c", "apiVersion": "1", "apiStrict": False},
    {"distinct": "c", "key": "v", "apiVersion": "1", "apiStrict": True},
    {"buildInfo": 1, "apiVersion": "1", "apiStrict": True},
    (
        A,
        {
            "listDatabases": 1,
            "nameOnly": True,
            "apiVersion": "1",
            "apiStrict": True,
            "pick": ["ok"],
        },
    ),
    {"serverStatus": 1, "apiVersion": "1", "apiStrict": True},
    {"collStats": "c", "apiVersion": "1", "apiStrict": True},
    {"dbStats": 1, "apiVersion": "1", "apiStrict": True},
    {"getParameter": 1, "quiet": 1, "apiVersion": "1", "apiStrict": True},
    {"nosuch": 1, "apiVersion": "1", "apiStrict": True},
    {
        "aggregate": "c",
        "pipeline": [{"$collStats": {"count": {}}}],
        "cursor": {},
        "apiVersion": "1",
        "apiStrict": True,
    },
    {
        "aggregate": "c",
        "pipeline": [{"$match": {}}],
        "cursor": {},
        "apiVersion": "1",
        "apiStrict": True,
    },
    {
        "createIndexes": "c",
        "indexes": [{"key": {"t": "text"}, "name": "t"}],
        "apiVersion": "1",
        "apiStrict": True,
    },
    {"insert": "c", "documents": [{"_id": 1}], "apiVersion": "1", "apiStrict": True},
    {"find": "c", "apiVersion": "1", "apiDeprecationErrors": True},
    {"mapReduce": "c", "map": "function(){}", "reduce": "function(){}", "out": {"inline": 1}},
)

# ------------------------------------------- the fields every command takes

scenario(
    "$db: its type and the names it refuses",
    *[("@raw", {"ping": 1, "$db": v}) for v in [1, None, True, {}, [], 2.5]],
    *[
        (name, {"ping": 1})
        for name in ["", "a.b", "a b", "a/b", "a\\b", 'a"b', "a$b", "a\x00b", "A" * 63, "A" * 64]
    ],
    *[(name, {"find": "c"}) for name in ["", "a.b", "a b", "a$b", "A" * 64]],
    *[(name, {"insert": "c", "documents": [{"_id": 1}]}) for name in ODD_NAMES],
    ("@missing", {"ping": 1}),
)
scenario(
    "comment and maxTimeMS on a command that does not scan",
    *[{"ping": 1, "maxTimeMS": v} for v in [0, 1, 100, *BAD_INT, 2**31 - 1, 2**31, 1.0, 1.5]],
    *[{"buildInfo": 1, "maxTimeMS": v} for v in ["x", -1]],
    *[{"find": "c", "maxTimeMS": v} for v in ["x", -1, None, 2**31, Int64(5), 5.0, 5.5]],
    seed=True,
)
scenario(
    "readConcern on a read",
    *[
        {"find": "c", "sort": {"_id": 1}, "readConcern": v}
        for v in [
            {},
            {"level": "local"},
            {"level": "available"},
            {"level": "majority"},
            {"level": "linearizable"},
            {"level": "snapshot"},
            {"level": "bogus"},
            {"level": 1},
            {"level": None},
            {"bogus": 1},
            {"level": "local", "bogus": 1},
            {"afterClusterTime": 1},
            {"afterClusterTime": Timestamp(0, 0)},
            {"afterClusterTime": Timestamp(1, 1)},
            {"afterClusterTime": Timestamp(2**31 - 1, 1)},
            {"level": "snapshot", "atClusterTime": Timestamp(1, 1)},
            {"level": "local", "atClusterTime": Timestamp(1, 1)},
            {"atClusterTime": 1},
            {"afterOpTime": {"ts": Timestamp(1, 1), "t": Int64(1)}},
            {"level": "majority", "afterClusterTime": Timestamp(1, 1), "atClusterTime": 1},
            {"provenance": "clientSupplied"},
            {"provenance": "bogus"},
            1,
            "x",
            None,
            [],
        ]
    ],
    seed=True,
)
scenario(
    "readConcern on commands that are not reads",
    *[
        {**command, "readConcern": {"level": level}}
        for command in [
            {"ping": 1},
            {"insert": "c", "documents": [{"_id": 9}]},
            {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"v": 1}}}]},
            {"delete": "c", "deletes": [{"q": {"_id": 9}, "limit": 1}]},
            {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"v": 2}}},
            {"count": "c"},
            {"distinct": "c", "key": "v"},
            {"aggregate": "c", "pipeline": [], "cursor": {}},
            {"listCollections": 1},
            {"listIndexes": "c"},
            {"create": "d"},
            {"createIndexes": "c", "indexes": [{"key": {"w": 1}, "name": "w_1"}]},
            {"drop": "d"},
            {"buildInfo": 1, "pick": ["version", "ok"]},
            {"dbStats": 1, "pick": ["db", "ok"]},
            {"collStats": "c", "pick": ["ns", "count", "ok"]},
        ]
        for level in ["local", "majority", "snapshot", "linearizable"]
    ],
    seed=True,
)
scenario(
    "writeConcern on commands that do not write",
    *[
        {**command, "writeConcern": {"w": 1}}
        for command in [
            {"ping": 1},
            {"hello": 1},
            {"find": "c"},
            {"count": "c"},
            {"distinct": "c", "key": "v"},
            {"aggregate": "c", "pipeline": [], "cursor": {}},
            {"listCollections": 1},
            {"listIndexes": "c"},
            {"buildInfo": 1, "pick": ["version", "ok"]},
            {"dbStats": 1, "pick": ["db", "ok"]},
            {"collStats": "c", "pick": ["ns", "count", "ok"]},
            {"getMore": Int64(5), "collection": "c"},
            {"killCursors": "c", "cursors": [Int64(5)]},
            {"explain": {"find": "c"}},
            {"validate": "c"},
        ]
    ],
    *[
        {"insert": "c", "documents": [{"x": 1}], "writeConcern": v}
        for v in [
            {},
            {"w": 0},
            {"w": 1},
            {"w": 2},
            {"w": "majority"},
            {"w": "bogus"},
            {"w": -1},
            {"w": 1.5},
            {"w": None},
            {"w": {}},
            {"w": {"a": 1}},
            {"w": 51},
            {"w": 1, "j": True},
            {"w": 1, "j": "x"},
            {"w": 1, "fsync": True},
            {"w": 1, "j": True, "fsync": True},
            {"w": 1, "wtimeout": 5},
            {"w": 1, "wtimeout": "x"},
            {"w": 1, "wtimeout": -1},
            {"w": 1, "wtimeoutMS": 5},
            {"w": 1, "bogus": 1},
            {"w": 1, "provenance": "clientSupplied"},
            {"j": True},
            {"wtimeout": 5},
            1,
            "x",
            None,
            [],
        ]
    ],
    seed=True,
)
scenario(
    "$readPreference and other dollar fields",
    *[
        {"find": "c", "sort": {"_id": 1}, "$readPreference": v}
        for v in [
            {"mode": "primary"},
            {"mode": "secondary"},
            {"mode": "primaryPreferred"},
            {"mode": "secondaryPreferred"},
            {"mode": "nearest"},
            {"mode": "bogus"},
            {"mode": 1},
            {},
            {"mode": "secondary", "tags": [{"a": "b"}]},
            {"mode": "primary", "tags": [{"a": "b"}]},
            {"mode": "secondary", "maxStalenessSeconds": 90},
            {"mode": "secondary", "maxStalenessSeconds": 1},
            {"mode": "primary", "maxStalenessSeconds": 90},
            {"mode": "secondary", "hedge": {"enabled": True}},
            {"mode": "secondary", "bogus": 1},
            1,
            "x",
            None,
        ]
    ],
    {"ping": 1, "$bogus": 1},
    {"ping": 1, "$clusterTime": 1},
    {"ping": 1, "$clusterTime": {}},
    {"ping": 1, "$clusterTime": {"clusterTime": Timestamp(1, 1)}},
    {
        "ping": 1,
        "$clusterTime": {
            "clusterTime": Timestamp(1, 1),
            "signature": {"hash": Binary(b"\x00" * 20), "keyId": Int64(0)},
        },
    },
    {"ping": 1, "$audit": 1},
    {"ping": 1, "$client": {}},
    {"ping": 1, "$configServerState": 1},
    {"ping": 1, "$queryOptions": {}},
    {"ping": 1, "$maxTimeMS": 5},
    {"find": "c", "$readPreference": {"mode": "secondary"}, "sort": {"_id": 1}},
    seed=True,
)

# ------------------------------------------------------ what the server is

scenario(
    "buildInfo, by both names",
    shape({"buildInfo": 1}, 2),
    shape({"buildinfo": 1}, 2),
    shape((A, {"buildInfo": 1}), 2),
    *[shape({"buildInfo": v}, 1) for v in ["x", None, 0, {}]],
    {"buildInfo": 1, "bogus": 1},
    {"BuildInfo": 1},
)
scenario(
    "buildInfo: the values a driver reads",
    {"buildInfo": 1, "pick": ["maxBsonObjectSize", "bits", "debug", "storageEngines", "ok"]},
    {"hello": 1, "pick": ["maxBsonObjectSize", "maxMessageSizeBytes", "maxWriteBatchSize"]},
    {"hello": 1, "pick": ["minWireVersion", "maxWireVersion", "logicalSessionTimeoutMinutes"]},
    {"hello": 1, "pick": ["readOnly", "isWritablePrimary", "secondary", "ok"]},
    {"isMaster": 1, "pick": ["ismaster", "secondary", "helloOk", "ok"]},
)
scenario(
    "connectionStatus and whatsmyuri",
    {"connectionStatus": 1},
    *[{"connectionStatus": 1, "showPrivileges": v} for v in [True, False, *BAD_BOOL]],
    {"connectionStatus": 1, "bogus": 1},
    (A, {"connectionStatus": 1}),
    {"connectionStatus": "x"},
    {"whatsmyuri": 1},
    (A, {"whatsmyuri": 1}),
    {"whatsmyuri": 1, "bogus": 1},
)
scenario(
    "hostInfo, getCmdLineOpts, features and the like",
    shape({"hostInfo": 1}, 2),
    shape((A, {"hostInfo": 1}), 2),
    shape((A, {"getCmdLineOpts": 1}), 1),
    {"getCmdLineOpts": 1},
    shape({"features": 1}, 1),
    shape({"isSelf": 1}, 1),
    shape((A, {"connPoolStats": 1}), 1),
    shape((A, {"lockInfo": 1}), 1),
    shape((A, {"top": 1}), 1),
    {"top": 1},
    shape((A, {"listCommands": 1}), 1),
    shape({"listCommands": 1}, 1),
    {"listCommands": 1, "bogus": 1, "pick": ["ok"]},
    shape((A, {"getDefaultRWConcern": 1}), 2),
    {"getDefaultRWConcern": 1},
    (A, {"getDefaultRWConcern": 1, "inMemory": "x"}),
    shape((A, {"getClusterParameter": "*"}), 1),
    (A, {"getClusterParameter": "bogus"}),
    (A, {"getClusterParameter": 1}),
    {"getClusterParameter": "*"},
    shape({"getShardMap": 1}, 1),
    {"listShards": 1},
    (A, {"listShards": 1}),
    {"isdbgrid": 1},
    (A, {"isdbgrid": 1}),
    (A, {"shardingState": 1}),
    (A, {"balancerStatus": 1}),
    (A, {"getShardVersion": "a.b"}),
    (A, {"logRotate": "bogus"}),
    {"logRotate": 1},
)
scenario(
    "serverStatus: its sections",
    shape({"serverStatus": 1}, 1),
    shape((A, {"serverStatus": 1}), 1),
    *[
        shape({"serverStatus": 1, "pick": [section, "ok"]}, 3)
        for section in [
            "connections",
            "opcounters",
            "opcountersRepl",
            "asserts",
            "network",
            "mem",
            "extra_info",
            "globalLock",
            "storageEngine",
            "logicalSessionRecordCache",
            "transactions",
            "repl",
            "metrics",
            "electionMetrics",
            "tcmalloc",
            "locks",
            "opLatencies",
            "security",
            "trafficRecording",
            "catalogStats",
            "indexStats",
            "flowControl",
        ]
    ],
    {
        "serverStatus": 1,
        "pick": ["version", "process", "uptime", "uptimeMillis", "uptimeEstimate", "pid", "ok"],
    },
    *[shape({"serverStatus": 1, "repl": v}, 1) for v in [0, 1, "x"]],
    shape({"serverStatus": 1, "metrics": 0, "locks": 0, "wiredTiger": 0}, 1),
    shape({"serverStatus": 1, "bogus": 1}, 1),
    shape({"serverStatus": "x"}, 1),
    shape({"serverStatus": 1, "latchAnalysis": 1}, 1),
    shape({"serverStatus": 1, "mirroredReads": 1}, 1),
)
scenario(
    "serverStatus: counters move with the commands run",
    {"serverStatus": 1, "pick": ["ok"], "save": "before"},
    {"insert": "c", "documents": [{"_id": 10}, {"_id": 11}]},
    {"find": "c"},
    {"update": "c", "updates": [{"q": {"_id": 10}, "u": {"$set": {"v": 1}}}]},
    {"delete": "c", "deletes": [{"q": {"_id": 11}, "limit": 1}]},
    {"aggregate": "c", "pipeline": [], "cursor": {}},
    {"count": "c"},
    {"serverStatus": 1, "delta": "opcounters"},
    seed=True,
)
scenario(
    "getParameter",
    (A, {"setParameter": 1, "quiet": False, "pick": ["ok"]}),
    (A, {"setParameter": 1, "logLevel": 0, "pick": ["ok"]}),
    *[
        (A, {"getParameter": 1, name: 1})
        for name in [
            "featureCompatibilityVersion",
            "authenticationMechanisms",
            "logLevel",
            "quiet",
            "notablescan",
            "ttlMonitorEnabled",
            "ttlMonitorSleepSecs",
            "cursorTimeoutMillis",
            "transactionLifetimeLimitSeconds",
            "maxTransactionLockRequestTimeoutMillis",
            "localLogicalSessionTimeoutMinutes",
            "maxSessions",
            "internalQueryMaxBlockingSortMemoryUsageBytes",
            "internalQueryFacetBufferSizeBytes",
            "internalDocumentSourceGroupMaxMemoryBytes",
            "internalQueryExecMaxBlockingSortBytes",
            "maxIndexBuildMemoryUsageMegabytes",
            "enableTestCommands",
            "tlsMode",
            "sslMode",
            "bogus",
            "storageEngineConcurrentReadTransactions",
            "wiredTigerConcurrentReadTransactions",
            "internalQueryFrameworkControl",
            "maxBSONDepth",
            "maxNumberOfTransactionOperationsInSingleOplogEntry",
            "minSnapshotHistoryWindowInSeconds",
            "diagnosticDataCollectionEnabled",
            "replWriterThreadCount",
            "periodicNoopIntervalSecs",
            "enableLocalhostAuthBypass",
            "scramIterationCount",
            "scramSHA256IterationCount",
            "allowDiskUseByDefault",
            "changeStreamPreAndPostImagesExpiredDocumentsRemoverSleepSecs",
            "timeseriesBucketMaxCount",
            "tcpFastOpenServer",
        ]
    ],
    (A, {"getParameter": 1}),
    (A, {"getParameter": 1, "quiet": 1, "logLevel": 1}),
    (A, {"getParameter": 1, "quiet": 1, "bogus": 1}),
    (A, {"getParameter": 1, "quiet": 0}),
    (A, {"getParameter": 1, "quiet": "x"}),
    shape((A, {"getParameter": "*"}), 1),
    (A, {"getParameter": "x", "quiet": 1}),
    (A, {"getParameter": None, "quiet": 1}),
    (A, {"getParameter": {"showDetails": True}, "quiet": 1}),
    (A, {"getParameter": {"showDetails": False}, "quiet": 1}),
    (A, {"getParameter": {"showDetails": True}, "featureCompatibilityVersion": 1}),
    (A, {"getParameter": {"allParameters": True}, "pick": ["quiet", "logLevel", "ok"]}),
    (A, {"getParameter": {"showDetails": "x"}, "quiet": 1}),
    (A, {"getParameter": {"bogus": 1}, "quiet": 1}),
    (A, {"getParameter": {"setAt": "startup"}, "quiet": 1}),
    (A, {"getParameter": {"setAt": "bogus"}, "quiet": 1}),
    {"getParameter": 1, "quiet": 1},
    {"getparameter": 1, "quiet": 1},
)
scenario(
    "setParameter, and reading back what it set",
    (A, {"setParameter": 1, "quiet": False, "pick": ["ok"]}),
    (A, {"setParameter": 1, "logLevel": 0, "pick": ["ok"]}),
    (A, {"setParameter": 1, "quiet": True}),
    (A, {"getParameter": 1, "quiet": 1}),
    (A, {"setParameter": 1, "quiet": False}),
    (A, {"getParameter": 1, "quiet": 1}),
    (A, {"setParameter": 1, "quiet": "x"}),
    (A, {"setParameter": 1, "quiet": 5}),
    (A, {"setParameter": 1, "logLevel": 1}),
    (A, {"getParameter": 1, "logLevel": 1}),
    (A, {"setParameter": 1, "logLevel": 0}),
    (A, {"setParameter": 1, "logLevel": -1}),
    (A, {"setParameter": 1, "logLevel": 6}),
    (A, {"setParameter": 1, "logLevel": "x"}),
    (A, {"setParameter": 1, "notablescan": True}),
    ("@db", {"find": "c", "filter": {"v": 5}}),
    ("@db", {"find": "c", "filter": {"zz": 5}}),
    ("@db", {"find": "c", "filter": {"_id": 1}}),
    ("@db", {"count": "c", "query": {"zz": 5}}),
    ("@db", {"aggregate": "c", "pipeline": [{"$match": {"zz": 1}}], "cursor": {}}),
    ("@db", {"update": "c", "updates": [{"q": {"zz": 1}, "u": {"$set": {"a": 1}}}]}),
    ("@db", {"delete": "c", "deletes": [{"q": {"zz": 1}, "limit": 0}]}),
    ("@db", {"find": "c", "hint": {"$natural": 1}}),
    ("@db", {"find": "c"}),
    (A, {"setParameter": 1, "notablescan": False}),
    ("@db", {"find": "c", "filter": {"zz": 5}}),
    (A, {"setParameter": 1, "cursorTimeoutMillis": 600000}),
    (A, {"setParameter": 1, "cursorTimeoutMillis": "x"}),
    (A, {"setParameter": 1, "transactionLifetimeLimitSeconds": 60}),
    (A, {"setParameter": 1, "transactionLifetimeLimitSeconds": 0}),
    (A, {"setParameter": 1, "ttlMonitorEnabled": True}),
    (A, {"setParameter": 1, "ttlMonitorSleepSecs": 60}),
    (A, {"setParameter": 1, "bogus": 1}),
    (A, {"setParameter": 1}),
    (A, {"setParameter": 1, "quiet": False, "bogus": 1}),
    (A, {"setParameter": 1, "featureCompatibilityVersion": "8.0"}),
    (A, {"setParameter": 1, "authenticationMechanisms": "x"}),
    (A, {"setParameter": 1, "maxBSONDepth": 5}),
    (A, {"setParameter": 1, "enableTestCommands": 1}),
    {"setParameter": 1, "quiet": False},
    (A, {"setParameter": 1, "internalQueryMaxBlockingSortMemoryUsageBytes": 104857600}),
    seed=True,
)
scenario(
    "getLog",
    shape((A, {"getLog": "*"}), 1),
    (A, {"getLog": "*"}),
    shape((A, {"getLog": "global"}), 1),
    shape((A, {"getLog": "startupWarnings"}), 1),
    (A, {"getLog": "bogus"}),
    (A, {"getLog": 1}),
    (A, {"getLog": None}),
    (A, {"getLog": ""}),
    (A, {"getLog": {}}),
    {"getLog": "global"},
    (A, {"getLog": "global", "bogus": 1}),
)
scenario(
    "currentOp, killOp and the profiler",
    shape((A, {"currentOp": 1}), 1),
    {"currentOp": 1},
    shape((A, {"currentOp": 1, "$all": True}), 1),
    shape((A, {"currentOp": 1, "$ownOps": True}), 1),
    shape((A, {"currentOp": 1, "active": True}), 1),
    shape((A, {"currentOp": "x"}), 1),
    (A, {"currentOp": 1, "op": "nosuch", "pick": ["inprog", "ok"]}),
    (A, {"killOp": 1, "op": 123456}),
    (A, {"killOp": 1}),
    (A, {"killOp": 1, "op": "x"}),
    (A, {"killOp": 1, "op": None}),
    (A, {"killOp": 1, "op": 2.5}),
    (A, {"killOp": 1, "op": Int64(2**40)}),
    (A, {"killOp": 1, "op": -1}),
    {"killOp": 1, "op": 1},
    {"profile": -1},
    {"profile": 0},
    {"profile": 1},
    {"profile": -1},
    {"profile": 2},
    {"profile": -1},
    {"profile": 0, "slowms": 50},
    {"profile": -1},
    {"profile": 0, "slowms": 100, "sampleRate": 0.5},
    {"profile": -1},
    {"profile": 0, "sampleRate": 1.0},
    {"profile": 1, "filter": {"millis": {"$gt": 5}}},
    {"profile": -1},
    {"profile": 0, "filter": "unset"},
    {"profile": -1},
    *[{"profile": v} for v in [3, -2, "x", None, 1.5, True, {}]],
    {"profile": 0, "slowms": "x"},
    {"profile": 0, "sampleRate": 2},
    {"profile": 0, "sampleRate": "x"},
    {"profile": 0, "filter": 1},
    {"profile": 0, "filter": "x"},
    {"profile": 0, "filter": {"$bogus": 1}},
    {"profile": 0, "bogus": 1},
    {"profile": 0, "slowms": 100},
    (A, {"profile": -1}),
    ("local", {"profile": -1}),
    {"listCollections": 1, "filter": {"name": "system.profile"}, "nameOnly": True},
    {"find": "system.profile", "limit": 1, "pick": ["ok"]},
    {"drop": "system.profile"},
)

# -------------------------------------------------------------- statistics

scenario(
    "dbStats",
    {"dbStats": 1},
    *[{"dbStats": 1, "scale": v} for v in [1, 1024, 0, -1, "x", None, 2.5, 1.0, True, {}, 2**31]],
    *[{"dbStats": 1, "freeStorage": v} for v in [1, 0, True, "x"]],
    {"dbstats": 1},
    {"dbStats": "x"},
    {"dbStats": 1, "bogus": 1},
    ("@other", {"dbStats": 1}),
    (A, {"dbStats": 1, "pick": ["db", "ok"]}),
    {"dbStats": 1, "pick": ["collections", "views", "objects", "indexes", "avgObjSize"]},
    {"create": "vw", "viewOn": "c", "pipeline": []},
    {"create": "empty"},
    {"dbStats": 1, "pick": ["collections", "views", "objects", "indexes", "avgObjSize"]},
    seed=True,
)
scenario(
    "collStats",
    shape({"collStats": "c"}, 1),
    {"collStats": "c", "pick": ["ns", "count", "size", "avgObjSize", "nindexes", "capped", "ok"]},
    *[
        {"collStats": "c", "scale": v, "pick": ["size", "avgObjSize", "scaleFactor", "ok"]}
        for v in [1, 2, 1024, 0, -1, "x", None, 2.5, 1.0, True, {}]
    ],
    {"collStats": "nosuch"},
    shape({"collStats": "nosuch"}, 1),
    *[{"collStats": v} for v in [1, "", None, {}, []]],
    {"collstats": "c", "pick": ["ns", "ok"]},
    {"collStats": "c", "bogus": 1},
    {"collStats": "c", "verbose": True, "pick": ["count", "ok"]},
    {"create": "vw", "viewOn": "c", "pipeline": []},
    {"collStats": "vw"},
    {"create": "cap", "capped": True, "size": 4096, "max": 5},
    {"collStats": "cap", "pick": ["capped", "max", "maxSize", "count", "size", "ok"]},
    {"create": "empty"},
    {"collStats": "empty", "pick": ["count", "size", "avgObjSize", "nindexes", "ok"]},
    {"collStats": "c", "pick": ["indexDetails", "ok"], "shape": 1},
    {"collStats": "c", "pick": ["indexSizes", "indexBuilds", "ok"], "shape": 2},
    (A, {"collStats": "system.version", "pick": ["ns", "ok"]}),
    seed=True,
)
scenario(
    "$collStats and $indexStats stages",
    *[
        shape({"aggregate": "c", "pipeline": [{"$collStats": spec}], "cursor": {}}, 4)
        for spec in [
            {},
            {"count": {}},
            {"storageStats": {}},
            {"storageStats": {"scale": 1024}},
            {"latencyStats": {}},
            {"latencyStats": {"histograms": True}},
            {"queryExecStats": {}},
            {"count": {}, "storageStats": {}, "latencyStats": {}},
            {"storageStats": {"bogus": 1}},
        ]
    ],
    {"aggregate": "c", "pipeline": [{"$collStats": {"count": {}}}], "cursor": {}, "pick": ["ok"]},
    *[
        {"aggregate": "c", "pipeline": [{"$collStats": spec}], "cursor": {}}
        for spec in [
            1,
            "x",
            None,
            {"bogus": 1},
            {"count": 1},
            {"storageStats": 1},
            {"storageStats": {"scale": 0}},
            {"storageStats": {"scale": "x"}},
            {"latencyStats": 1},
            {"latencyStats": {"histograms": "x"}},
        ]
    ],
    {"aggregate": "c", "pipeline": [{"$match": {}}, {"$collStats": {}}], "cursor": {}},
    {"aggregate": "nosuch", "pipeline": [{"$collStats": {"count": {}}}], "cursor": {}},
    {
        "aggregate": "c",
        "pipeline": [{"$collStats": {"count": {}}}, {"$project": {"count": 1, "ns": 1}}],
        "cursor": {},
    },
    {
        "aggregate": "c",
        "pipeline": [
            {"$collStats": {"storageStats": {}}},
            {
                "$project": {
                    "c": "$storageStats.count",
                    "s": "$storageStats.size",
                    "a": "$storageStats.avgObjSize",
                    "n": "$storageStats.nindexes",
                    "cap": "$storageStats.capped",
                }
            },
        ],
        "cursor": {},
    },
    shape({"aggregate": "c", "pipeline": [{"$indexStats": {}}], "cursor": {}}, 5),
    {
        "aggregate": "c",
        "pipeline": [
            {"$indexStats": {}},
            {"$project": {"name": 1, "key": 1, "ops": "$accesses.ops", "spec": 1}},
            {"$sort": {"name": 1}},
        ],
        "cursor": {},
    },
    *[
        {"aggregate": "c", "pipeline": [{"$indexStats": spec}], "cursor": {}}
        for spec in [1, "x", {"bogus": 1}]
    ],
    {"aggregate": "c", "pipeline": [{"$match": {}}, {"$indexStats": {}}], "cursor": {}},
    {"aggregate": "nosuch", "pipeline": [{"$indexStats": {}}], "cursor": {}},
    {"find": "c", "filter": {"v": 5}},
    {"find": "c", "filter": {"v": 5}},
    {"find": "c", "filter": {"_id": 1}},
    {
        "aggregate": "c",
        "pipeline": [
            {"$indexStats": {}},
            {"$project": {"name": 1, "ops": "$accesses.ops"}},
            {"$sort": {"name": 1}},
        ],
        "cursor": {},
    },
    seed=True,
)
scenario(
    "dataSize",
    {"dataSize": "@db.c"},
    {"dataSize": "@db.c", "pick": ["size", "numObjects", "estimate", "ok"]},
    {"dataSize": "@db.c", "estimate": True, "pick": ["size", "numObjects", "estimate", "ok"]},
    {"dataSize": "@db.c", "keyPattern": {"_id": 1}, "pick": ["size", "numObjects", "ok"]},
    {
        "dataSize": "@db.c",
        "keyPattern": {"_id": 1},
        "min": {"_id": 2},
        "max": {"_id": 3},
        "pick": ["size", "numObjects", "ok"],
    },
    {"dataSize": "@db.c", "keyPattern": {"zz": 1}},
    {"dataSize": "@db.c", "min": {"_id": 2}},
    {"dataSize": "@db.c", "min": {"_id": 2}, "max": {"_id": 3}},
    {"dataSize": "@db.c", "min": {"zz": 2}, "max": {"zz": 3}},
    {"dataSize": "@db.nosuch"},
    {"dataSize": "c"},
    {"dataSize": ""},
    {"dataSize": 1},
    {"dataSize": None},
    {"dataSize": "other.c"},
    {"dataSize": "@db.c", "bogus": 1},
    {"datasize": "@db.c", "pick": ["numObjects", "ok"]},
    seed=True,
)
scenario(
    "validate",
    shape({"validate": "c"}, 1),
    {
        "validate": "c",
        "pick": ["ns", "nrecords", "nIndexes", "keysPerIndex", "valid", "warnings", "errors"],
    },
    {"validate": "c", "pick": ["nInvalidDocuments", "nNonCompliantDocuments", "repaired", "ok"]},
    *[
        {"validate": "c", option: v, "pick": ["valid", "ok"]}
        for option in ["full", "repair", "metadata", "checkBSONConformance", "background"]
        for v in [True, False, "x"]
    ],
    {"validate": "c", "full": True, "repair": True},
    {"validate": "c", "metadata": True, "full": True},
    {"validate": "c", "background": True, "full": True},
    {"validate": "c", "bogus": 1, "pick": ["valid", "ok"]},
    {"validate": "nosuch"},
    *[{"validate": v} for v in [1, "", None, {}]],
    {"create": "vw", "viewOn": "c", "pipeline": []},
    {"validate": "vw"},
    {"create": "empty"},
    {"validate": "empty", "pick": ["nrecords", "nIndexes", "keysPerIndex", "valid", "ok"]},
    {"validate": "system.views", "pick": ["nrecords", "valid", "ok"]},
    seed=True,
)
scenario(
    "validate: the keys each index holds",
    {
        "insert": "k",
        "documents": [
            {"_id": 1, "v": 5, "w": "a"},
            {"_id": 2, "v": "x", "w": "A"},
            {"_id": 3, "v": [1, 2, 2], "p": [{"q": 1}, {}]},
            {"_id": 4, "v": [], "p": [{"q": [7, 8]}, 3]},
            {"_id": 5, "p": {"q": None}},
            {"_id": 6, "v": [[1, 2], 3], "w": None},
            {"_id": 7, "v": 5, "w": "a"},
        ],
    },
    {
        "createIndexes": "k",
        "indexes": [
            {"key": {"v": 1}, "name": "v_1"},
            {"key": {"v": 1, "w": -1}, "name": "v_w"},
            {"key": {"w": 1}, "name": "w_sparse", "sparse": True},
            {"key": {"v": -1}, "name": "v_partial", "partialFilterExpression": {"w": "a"}},
            {"key": {"p.q": 1}, "name": "pq"},
            {"key": {"w": 1, "p.q": 1}, "name": "w_pq"},
            {
                "key": {"w": 1, "_id": 1},
                "name": "w_ci",
                "collation": {"locale": "en", "strength": 2},
            },
        ],
    },
    {"validate": "k", "pick": ["nrecords", "nIndexes", "keysPerIndex", "valid", "errors"]},
    {"update": "k", "updates": [{"q": {"_id": 3}, "u": {"$set": {"v": [9, 10, 11]}}}]},
    {"delete": "k", "deletes": [{"q": {"_id": 1}, "limit": 1}]},
    {"validate": "k", "pick": ["nrecords", "nIndexes", "keysPerIndex", "valid", "errors"]},
    {"validate": "k", "full": True, "pick": ["nrecords", "keysPerIndex", "valid"]},
)
scenario(
    "listDatabases",
    (A, {"listDatabases": 1, "filter": {"name": "@db"}}),
    (A, {"listDatabases": 1, "filter": {"name": "@db"}, "nameOnly": True}),
    (A, {"listDatabases": 1, "filter": {"name": {"$regex": "^@db"}}, "nameOnly": True}),
    (A, {"listDatabases": 1, "filter": {"name": "nosuchdb"}}),
    (A, {"listDatabases": 1, "filter": {"name": "@db", "empty": False}, "nameOnly": True}),
    (A, {"listDatabases": 1, "filter": {"name": "@db", "sizeOnDisk": {"$gt": 0}}}),
    (A, {"listDatabases": 1, "filter": {"name": "@db", "bogus": 1}}),
    (A, {"listDatabases": 1, "filter": {"$expr": {"$eq": ["$name", "@db"]}}, "nameOnly": True}),
    (A, {"listDatabases": 1, "filter": {"name": {"$in": ["admin", "local", "config"]}}}),
    (
        A,
        {
            "listDatabases": 1,
            "filter": {"name": {"$in": ["admin", "local", "config"]}},
            "nameOnly": True,
        },
    ),
    *[
        (A, {"listDatabases": 1, "filter": {"name": "@db"}, "nameOnly": v})
        for v in ["x", 1, 0, None, {}]
    ],
    *[(A, {"listDatabases": 1, "filter": v}) for v in [1, "x", [], {"$bogus": 1}]],
    *[
        (A, {"listDatabases": 1, "filter": {"name": "@db"}, "authorizedDatabases": v})
        for v in [True, False, "x"]
    ],
    (A, {"listDatabases": 1, "filter": {"name": "@db"}, "bogus": 1}),
    (A, {"listDatabases": "x", "filter": {"name": "@db"}}),
    (A, {"listdatabases": 1, "filter": {"name": "@db"}}),
    {"listDatabases": 1},
    shape((A, {"listDatabases": 1}), 1),
    ("@db", {"create": "empty2"}),
    (A, {"listDatabases": 1, "filter": {"name": "@db_o"}}),
    ("@other", {"create": "onlyempty"}),
    (A, {"listDatabases": 1, "filter": {"name": "@db_o"}}),
    seed=True,
)
scenario(
    "dbHash",
    shape({"dbHash": 1}, 2),
    {"dbHash": 1, "pick": ["collections", "md5", "ok"]},
    {"dbHash": 1, "collections": ["c"], "pick": ["collections", "ok"]},
    {"dbHash": 1, "collections": ["nosuch"], "pick": ["collections", "ok"]},
    {"dbHash": 1, "collections": "c"},
    {"dbHash": 1, "bogus": 1, "pick": ["ok"]},
    {"dbhash": 1, "pick": ["ok"]},
    seed=True,
)

# ---------------------------------------------------------------- sessions

#: New on every run: a session a server has seen keeps its transaction number.
LSID = {"id": Binary(uuid.uuid4().bytes, 4)}
LSID2 = {"id": Binary(uuid.uuid4().bytes, 4)}

scenario(
    "startSession and the commands that end one",
    {"startSession": 1},
    (A, {"startSession": 1}),
    {"startSession": "x"},
    {"startSession": 1, "bogus": 1},
    {"startSession": 1, "lsid": LSID},
    {"endSessions": [LSID]},
    {"endSessions": [LSID, LSID2]},
    {"endSessions": []},
    *[{"endSessions": v} for v in [1, "x", None, {}, [1], [{}], [{"id": 1}], [{"id": "x"}]]],
    {"endSessions": [{"id": Binary(b"short", 4)}]},
    {"endSessions": [{"id": Binary(LSID["id"], 0)}]},
    {"endSessions": [{**LSID, "bogus": 1}]},
    {"endSessions": [{**LSID, "uid": Binary(b"\x00" * 32)}]},
    {"endSessions": [LSID], "bogus": 1},
    {"endSessions": [LSID] * 10001},
    {"refreshSessions": [LSID]},
    {"refreshSessions": []},
    *[{"refreshSessions": v} for v in [1, "x", None, [1], [{}], [{"id": 1}]]],
    {"refreshSessions": [LSID], "bogus": 1},
    {"killSessions": [LSID]},
    {"killSessions": []},
    *[{"killSessions": v} for v in [1, "x", None, [1], [{}], [{"id": 1}]]],
    {"killSessions": [LSID], "bogus": 1},
    {"killAllSessions": []},
    {"killAllSessions": [{"user": "u", "db": "d"}]},
    *[{"killAllSessions": v} for v in [1, "x", None, [1], [{}], [{"user": 1}]]],
    {"killAllSessionsByPattern": []},
    {"killAllSessionsByPattern": [{"lsid": LSID}]},
    {"killAllSessionsByPattern": [{"users": [{"user": "u", "db": "d"}]}]},
    {"killAllSessionsByPattern": [{"bogus": 1}]},
    *[{"killAllSessionsByPattern": v} for v in [1, "x", None, [1]]],
    (A, {"refreshLogicalSessionCacheNow": 1}),
    {"refreshLogicalSessionCacheNow": 1},
)
scenario(
    "lsid on a command",
    {"ping": 1, "lsid": LSID},
    {"find": "c", "sort": {"_id": 1}, "lsid": LSID},
    *[
        {"find": "c", "sort": {"_id": 1}, "lsid": v}
        for v in [
            1,
            "x",
            None,
            [],
            {},
            {"id": 1},
            {"id": "x"},
            {"id": None},
            {"id": Binary(b"short", 4)},
            {"id": Binary(LSID["id"], 0)},
            {"id": Binary(LSID["id"], 3)},
            {**LSID, "bogus": 1},
            {**LSID, "uid": Binary(b"\x00" * 32)},
            {**LSID, "uid": 1},
            {**LSID, "txnNumber": Int64(1)},
            {**LSID, "txnUUID": Binary(LSID["id"], 4)},
        ]
    ],
    *[
        {"find": "c", "sort": {"_id": 1}, "lsid": LSID, "txnNumber": v}
        for v in [Int64(1), 1, "x", None, -1, Int64(-1), 2.5]
    ],
    {"find": "c", "txnNumber": Int64(1)},
    {"insert": "c", "documents": [{"_id": 20}], "txnNumber": Int64(1)},
    {"insert": "c", "documents": [{"_id": 21}], "lsid": LSID2, "txnNumber": Int64(1)},
    {"insert": "c", "documents": [{"_id": 21}], "lsid": LSID2, "txnNumber": Int64(1)},
    {"insert": "c", "documents": [{"_id": 22}], "lsid": LSID2, "txnNumber": Int64(0)},
    {"insert": "c", "documents": [{"_id": 23}], "lsid": LSID2, "txnNumber": Int64(2)},
    {"find": "c", "sort": {"_id": 1}, "projection": {"_id": 1}},
    {"hello": 1, "lsid": LSID, "pick": ["ok"]},
    {"hello": 1, "lsid": 1, "pick": ["ok"]},
    {"buildInfo": 1, "lsid": LSID, "pick": ["ok"]},
    {"killCursors": "c", "cursors": [Int64(5)], "lsid": LSID},
    {"endSessions": [LSID, LSID2]},
    seed=True,
)
scenario(
    "config.system.sessions and $listSessions",
    ("config", {"find": "system.sessions", "limit": 1, "pick": ["ok"]}),
    ("config", {"listCollections": 1, "filter": {"name": "system.sessions"}, "nameOnly": True}),
    (
        "config",
        {
            "aggregate": "system.sessions",
            "pipeline": [{"$listSessions": {}}, {"$limit": 0}],
            "cursor": {},
        },
    ),
    (
        "config",
        {
            "aggregate": "system.sessions",
            "pipeline": [{"$listSessions": {"allUsers": True}}, {"$limit": 0}],
            "cursor": {},
        },
    ),
    ("config", {"aggregate": "system.sessions", "pipeline": [{"$listSessions": 1}], "cursor": {}}),
    {"aggregate": "c", "pipeline": [{"$listSessions": {}}], "cursor": {}},
    (A, {"aggregate": 1, "pipeline": [{"$listLocalSessions": {}}, {"$limit": 0}], "cursor": {}}),
    {"aggregate": 1, "pipeline": [{"$listLocalSessions": {}}, {"$limit": 0}], "cursor": {}},
    (A, {"aggregate": 1, "pipeline": [{"$listLocalSessions": 1}], "cursor": {}}),
    (
        A,
        {
            "aggregate": 1,
            "pipeline": [{"$currentOp": {}}, {"$limit": 0}],
            "cursor": {},
        },
    ),
    {"aggregate": 1, "pipeline": [{"$currentOp": {}}, {"$limit": 0}], "cursor": {}},
    (A, {"aggregate": 1, "pipeline": [{"$currentOp": 1}], "cursor": {}}),
    (A, {"aggregate": 1, "pipeline": [{"$currentOp": {"bogus": 1}}], "cursor": {}}),
    (A, {"aggregate": "c", "pipeline": [{"$currentOp": {}}], "cursor": {}}),
    (A, {"aggregate": 1, "pipeline": [{"$match": {}}, {"$currentOp": {}}], "cursor": {}}),
    (A, {"aggregate": 1, "pipeline": [], "cursor": {}}),
    (A, {"aggregate": 1, "pipeline": [{"$match": {}}], "cursor": {}}),
    (A, {"aggregate": 1, "pipeline": [{"$documents": [{"a": 1}]}], "cursor": {}}),
    {"aggregate": 1, "pipeline": [{"$documents": [{"a": 1}]}], "cursor": {}},
    {"aggregate": 2, "pipeline": [{"$documents": [{"a": 1}]}], "cursor": {}},
    (A, {"aggregate": 1, "pipeline": [{"$listCatalog": {}}, {"$limit": 0}], "cursor": {}}),
    {"aggregate": 1, "pipeline": [{"$listCatalog": {}}], "cursor": {}},
)

# ----------------------------------------------------------- replica set

scenario(
    "replica-set status commands",
    shape((A, {"replSetGetStatus": 1}), 1),
    (A, {"replSetGetStatus": 1, "pick": ["myState", "ok"]}),
    {"replSetGetStatus": 1},
    shape((A, {"replSetGetStatus": 1, "initialSync": 1}), 1),
    shape((A, {"replSetGetConfig": 1}), 2),
    {"replSetGetConfig": 1},
    (A, {"replSetGetConfig": 1, "commitmentStatus": "x"}),
    (A, {"replSetGetConfig": 1, "bogus": 1}),
    (A, {"replSetInitiate": {}}),
    (A, {"replSetReconfig": {}}),
    (A, {"replSetSyncFrom": 1}),
    (A, {"replSetHeartbeat": 1}),
    (A, {"replSetUpdatePosition": 1}),
    (A, {"replSetResizeOplog": 1}),
    (A, {"replSetResizeOplog": 1, "size": "x"}),
    (A, {"replSetResizeOplog": 1, "size": 1}),
    (A, {"replSetAbortPrimaryCatchUp": 1}),
    (A, {"appendOplogNote": 1}),
    (A, {"appendOplogNote": 1, "data": 1}),
    (A, {"appendOplogNote": 1, "data": {"msg": "probe"}}),
    {"appendOplogNote": 1, "data": {"msg": "probe"}},
    (A, {"fsync": 1}),
    (A, {"fsync": 1, "bogus": 1}),
    {"fsync": 1},
    (A, {"fsyncUnlock": 1}),
    {"fsyncUnlock": 1},
    ("local", {"listCollections": 1, "filter": {"name": "oplog.rs"}, "nameOnly": True}),
    ("local", {"collStats": "oplog.rs", "pick": ["ns", "capped", "ok"]}),
    ("local", {"find": "oplog.rs", "limit": 1, "pick": ["ok"]}),
    ("local", {"find": "replset.election", "projection": {"_id": 0, "term": 1}}),
    ("local", {"count": "system.replset"}),
    ("local", {"listCollections": 1, "nameOnly": True, "pick": ["ok"]}),
    (A, {"listCollections": 1, "nameOnly": True, "filter": {"name": "system.version"}}),
    (A, {"find": "system.version", "filter": {"_id": "featureCompatibilityVersion"}}),
    (A, {"setFeatureCompatibilityVersion": "bogus"}),
    (A, {"setFeatureCompatibilityVersion": 1}),
    (A, {"setFeatureCompatibilityVersion": "8.2"}),
    (A, {"setFeatureCompatibilityVersion": "9.0", "confirm": True}),
    {"setFeatureCompatibilityVersion": "8.2", "confirm": True},
)

# ------------------------------------------------- authentication commands

scenario(
    "authentication commands on a server with none configured",
    {"logout": 1},
    (A, {"logout": 1}),
    {"getnonce": 1},
    {"authenticate": 1},
    {"authenticate": 1, "mechanism": "MONGODB-X509"},
    {"authenticate": 1, "mechanism": "bogus"},
    (A, {"saslStart": 1}),
    (A, {"saslStart": 1, "mechanism": "bogus", "payload": Binary(b"")}),
    (A, {"saslStart": 1, "mechanism": "PLAIN", "payload": Binary(b"\x00u\x00p")}),
    (A, {"saslStart": 1, "mechanism": "SCRAM-SHA-256", "payload": Binary(b"junk")}),
    (A, {"saslStart": 1, "mechanism": "SCRAM-SHA-256", "payload": "x"}),
    (A, {"saslStart": 1, "mechanism": 1, "payload": Binary(b"")}),
    (A, {"saslContinue": 1}),
    (A, {"saslContinue": 1, "conversationId": 1, "payload": Binary(b"")}),
    (A, {"saslContinue": 1, "conversationId": "x", "payload": Binary(b"")}),
    (A, {"usersInfo": 1}),
    {"usersInfo": 1},
    (A, {"usersInfo": "nobody"}),
    (A, {"usersInfo": {"user": "nobody", "db": "admin"}}),
    (A, {"usersInfo": [{"user": "nobody", "db": "admin"}, "other"]}),
    (A, {"usersInfo": {"forAllDBs": True}}),
    (A, {"usersInfo": 1, "showCredentials": True, "showPrivileges": True}),
    (A, {"usersInfo": 1, "filter": {"roles.role": "root"}}),
    *[(A, {"usersInfo": v}) for v in [None, 2.5, {}, {"user": 1}, {"bogus": 1}, [1]]],
    (A, {"usersInfo": 1, "bogus": 1}),
    (A, {"rolesInfo": 1}),
    {"rolesInfo": 1},
    (A, {"rolesInfo": "read"}),
    (A, {"rolesInfo": "nosuchrole"}),
    (A, {"rolesInfo": {"role": "read", "db": "admin"}}),
    (A, {"rolesInfo": "read", "showPrivileges": True, "pick": ["ok"]}),
    shape((A, {"rolesInfo": "read", "showPrivileges": True}), 3),
    (A, {"rolesInfo": 1, "showBuiltinRoles": True, "pick": ["ok"]}),
    {"rolesInfo": 1, "showBuiltinRoles": True},
    *[(A, {"rolesInfo": v}) for v in [None, 2.5, {}, {"role": 1}, [1]]],
    (A, {"rolesInfo": "read", "showPrivileges": "x"}),
    (A, {"rolesInfo": "read", "showAuthenticationRestrictions": True}),
)

# ------------------------------------------------- commands that are gone

scenario(
    "commands this version does not have, and shapes that are not commands",
    *[
        {name: 1}
        for name in [
            "nosuch",
            "getLastError",
            "getlasterror",
            "getPrevError",
            "resetError",
            "forceerror",
            "eval",
            "geoNear",
            "geoSearch",
            "group",
            "parallelCollectionScan",
            "repairDatabase",
            "copydb",
            "copydbgetnonce",
            "clone",
            "cloneCollection",
            "reIndex",
            "touch",
            "handshake",
            "availableQueryOptions",
            "diagLogging",
            "journalLatencyTest",
            "cleanupOrphaned",
            "driverOIDTest",
            "cloneCollectionAsCapped",
            "convertToCapped",
            "compact",
            "planCacheClear",
            "planCacheListFilters",
            "planCacheSetFilter",
            "planCacheClearFilters",
            "setQuerySettings",
            "removeQuerySettings",
            "getAuditConfig",
            "setIndexCommitQuorum",
            "dropConnections",
            "hello2",
            "$ping",
            "",
        ]
    ],
    {},
    {"ping.x": 1},
    {"a" * 200: 1},
    {"find": "c", "find2": 1},
    {"comment": "x", "ping": 1},
    {"maxTimeMS": 5, "ping": 1},
    {"lsid": LSID, "ping": 1},
    {"readConcern": {}, "find": "c"},
    {"1": 1},
    {"é": 1},
    (A, {"nosuch": 1}),
)
scenario(
    "commands that only run against admin, run elsewhere",
    *[
        shape({name: 1}, 1)
        for name in [
            "listDatabases",
            "getParameter",
            "setParameter",
            "getCmdLineOpts",
            "getLog",
            "currentOp",
            "killOp",
            "top",
            "replSetGetStatus",
            "replSetGetConfig",
            "renameCollection",
            "fsync",
            "fsyncUnlock",
            "logRotate",
            "setFeatureCompatibilityVersion",
            "connPoolStats",
            "lockInfo",
            "appendOplogNote",
            "getDefaultRWConcern",
            "setDefaultRWConcern",
            "killAllSessions",
            "refreshLogicalSessionCacheNow",
            "hostInfo",
            "serverStatus",
            "buildInfo",
            "listCommands",
            "connectionStatus",
            "whatsmyuri",
            "features",
            "startSession",
            "endSessions",
            "usersInfo",
            "rolesInfo",
            "dbStats",
            "profile",
            "dbHash",
        ]
    ],
)

# ---------------------------------------------------------------- running


def resolve(value, db):
    if isinstance(value, dict):
        return {resolve(k, db): resolve(v, db) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve(v, db) for v in value]
    if isinstance(value, str) and not isinstance(value, Binary) and "@db" in value:
        return value.replace("@db", db)
    return value


UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def kind(value):
    """The BSON type of a decoded value, as far as the decoder keeps it."""
    return f"<{type(value).__name__}>"


def clean(value, key=None):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k in NOISE:
                continue
            if k in VOLATILE and not (k == "id" and key == "cursor"):
                out[k] = kind(v)
                continue
            if k == "id" and key == "cursor":
                out[k] = "CURSOR" if v else 0
                continue
            out[k] = clean(v, k)
        return out
    if isinstance(value, list):
        return [clean(v, key) for v in value]
    if isinstance(value, str):
        text = UUID_TEXT.sub("UUID", value)
        text = re.sub(
            r"expected types '\[([^\]]*)\]'",
            lambda m: "expected types '[" + ", ".join(sorted(m.group(1).split(", "))) + "]'",
            text,
        )
        text = re.sub(r"127\.0\.0\.1:\d+", "HOST", text)
        text = re.sub(r"session [0-9a-f-]{36} - [A-Za-z0-9+/=]{44}", "session S", text)
        return re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(T)", text)
    if isinstance(value, float) and value != value:
        return "NaN"
    return value


def shaped(value, depth):
    if isinstance(value, dict):
        if depth <= 0:
            return "<dict>"
        return {k: shaped(v, depth - 1) for k, v in value.items() if k not in NOISE}
    return kind(value)


def reduce(reply, depth, pick):
    """What of a reply is compared: all of it, the picked keys, or its shape."""
    if reply.get("ok") != 1:
        return clean(reply)
    if pick is not None:
        reply = {k: reply[k] for k in pick if k in reply} | {
            f"({k} absent)": True for k in pick if k not in reply
        }
    if depth is not None:
        return shaped(reply, depth)
    return clean(reply)


def run_scenario(uri, index, steps):
    db = f"diagp_{index}"
    wire = Wire(uri)
    saved: dict[str, dict] = {}
    replies = []
    for step in steps:
        where, command = step if isinstance(step, tuple) else (None, step)
        command = dict(command)
        depth = command.pop("shape", None)
        pick = command.pop("pick", None)
        save = command.pop("save", None)
        delta = command.pop("delta", None)
        raw = where in ("@raw", "@missing")
        if where is None or where == "@db":
            where = db
        elif where == "@other":
            where = db + "_o"
        try:
            body = resolve(command, db)
            reply = wire.raw(body) if raw else wire.command(where, body)
        except TimeoutError:
            wire.close()
            wire = Wire(uri)
            replies.append({"probe": "TIMEOUT"})
            continue
        except (ConnectionError, OSError) as exc:
            wire.close()
            wire = Wire(uri)
            replies.append({"probe": f"CONNECTION {type(exc).__name__}"})
            continue
        if save:
            saved[save] = reply
        if delta and reply.get("ok") == 1 and "before" in saved:
            now, then = reply.get(delta, {}), saved["before"].get(delta, {})
            replies.append(
                {k: now[k] - then.get(k, 0) for k in now}
                if isinstance(now, dict) and isinstance(then, dict)
                else {"probe": f"no {delta}"}
            )
            continue
        replies.append(reduce(reply, depth, pick))
    try:
        wire.command("admin", {"setParameter": 1, "notablescan": False})
        for name in (db, db + "_o", *ODD_NAMES):
            wire.command(name, {"profile": 0, "slowms": 100, "sampleRate": 1.0})
            wire.command(name, {"dropDatabase": 1})
    except (TimeoutError, ConnectionError, OSError):
        print(f"  NOTE: cleanup of {db} did not finish on {uri}", file=sys.stderr)
    finally:
        wire.close()
    return replies


def differences(want, got, path=""):
    """The paths at which two reduced replies differ."""
    if isinstance(want, dict) and isinstance(got, dict):
        out = []
        for k in want:
            where = f"{path}.{k}" if path else str(k)
            if k not in got:
                out.append(f"-{where}")
            else:
                out.extend(differences(want[k], got[k], where))
        out.extend(f"+{path}.{k}" if path else f"+{k}" for k in got if k not in want)
        if not out and list(want) != list(got):
            out.append(f"~{path or 'reply'}: key order")
        return out
    if repr(want) != repr(got):
        return [f"~{path}: {str(want)[:300]} | {str(got)[:300]}"]
    return []


def show(step):
    where, command = step if isinstance(step, tuple) else (None, step)
    body = {k: v for k, v in command.items() if k not in ("save", "shape", "pick", "delta")}
    return (("" if where is None else f"[{where}] ") + repr(body))[:200]


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "1500"))

    # Self-check: the reference must be a replica-set primary that answers
    # the three kinds of step the way the probe assumes.
    check = run_scenario(
        mongod_uri,
        0,
        [
            *SEED,
            {"hello": 1, "pick": ["isWritablePrimary", "setName", "bogus"]},
            shape({"collStats": "c", "pick": ["count"]}, 1),
            {"nosuch": 1},
        ],
    )
    expected = [
        {"isWritablePrimary": True, "setName": "<str>", "(bogus absent)": True},
        {"count": "<int>"},
    ]
    if check[2:4] != expected or check[4].get("code") != 59:
        print("self-check failed on mongod:", check[2:], file=sys.stderr)
        return 2

    divergent = replies = replies_bad = total = 0
    for index, (name, steps) in enumerate(S, start=1):
        if only and only not in name:
            continue
        total += 1
        want = run_scenario(mongod_uri, index, steps)
        got = run_scenario(server_uri, index, steps)
        replies += len(steps)
        bad = [i for i, (w, g) in enumerate(zip(want, got, strict=True)) if repr(w) != repr(g)]
        if not bad:
            continue
        divergent += 1
        replies_bad += len(bad)
        print(f"\n--- {name}")
        for i in bad:
            print(f"  step {i}: {show(steps[i])}")
            found = differences(want[i], got[i])
            one_failed = want[i].get("ok") != got[i].get("ok")
            if one_failed or len(found) > 12 or not found:
                print(f"    mongod: {str(want[i])[:width]}")
                print(f"    server: {str(got[i])[:width]}")
            else:
                for line in found:
                    print(f"    {line[:width]}")
    print(
        f"\n=== diagnostic commands: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
