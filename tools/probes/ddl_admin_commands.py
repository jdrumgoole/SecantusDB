"""Differential-probe the DDL and admin commands against a real mongod.

`create` and every option it takes, `listCollections`, `collMod`,
`renameCollection`, `drop`, `dropDatabase`, `dbStats`, `collStats` and
`listDatabases`: what each accepts, what it refuses and in which words, what
its reply holds, and what the catalog shows afterwards. Commands go over a
raw connection, each scenario on a database of its own.

Byte counts are compared by TYPE only (`storageSize`, `dataSize` and the
like): the two servers lay documents out differently on disk, and a count of
bytes says nothing about the command. Document and index COUNTS are compared.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/ddl_admin_commands.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Decimal128, Int64  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
RUN = f"{os.getpid() % 100000}"

DROP = {"$clusterTime", "operationTime", "opTime", "electionId", "localTime"}

#: Reported in bytes, or derived from bytes: compared by type only.
SIZES = {
    "size",
    "storageSize",
    "totalSize",
    "totalIndexSize",
    "indexSize",
    "dataSize",
    "avgObjSize",
    "freeStorageSize",
    "indexFreeStorageSize",
    "totalFreeStorageSize",
    "fsUsedSize",
    "fsTotalSize",
    "sizeOnDisk",
    "numOrphanDocs",
}
#: What each storage engine says about itself.
ENGINE = {"wiredTiger", "indexDetails", "indexBuilds"}
#: The commands whose replies hold byte counts.
STATS = ("collStats", "dbStats", "listDatabases", "dataSize")

BAD = ["x", 2.5, -1, None, True, {}, [1], Int64(2), 0, Decimal128("2"), 1]
BOOLS = [True, False, 1, 0, "x", None, {}, 2.5]
DOCS = [{"_id": i, "a": i, "s": f"s{i}"} for i in range(5)]

S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, list(steps)))


def listed(**more):
    return {"listCollections": 1, **more}


def create(name="c", **options):
    return {"create": name, **options}


def admin(command):
    return ("admin", command)


def rename(src, dst, **more):
    return admin({"renameCollection": f"@db.{src}", "to": f"@db.{dst}", **more})


SEED = [
    {"insert": "c", "documents": DOCS},
    {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
]

# ------------------------------------------------------------------ create

scenario("create: a plain collection", create(), listed(), create(), create(capped=False))
scenario(
    "create: the name",
    *[
        {"create": v}
        for v in [
            "",
            1,
            None,
            {},
            [],
            "a.b",
            "a b",
            "$a",
            "a$b",
            "system.x",
            "system.views",
            "system.profile",
            "system.js",
            ".a",
            "a.",
            "a..b",
            "a\x00b",
            "x" * 200,
            "x" * 255,
            "x" * 256,
            "ünï",
            "oplog.rs",
        ]
    ],
    listed(nameOnly=True),
)
scenario(
    "create: in the internal databases",
    admin(create("ddlp_admin_c")),
    admin({"drop": "ddlp_admin_c"}),
    ("local", create("ddlp_local_c")),
    ("local", {"drop": "ddlp_local_c"}),
    ("config", create("ddlp_config_c")),
    ("config", {"drop": "ddlp_config_c"}),
    ("", create("c")),
    ("a.b", create("c")),
    ("a b", create("c")),
    ("$x", create("c")),
    ("x" * 64, create("c")),
)
scenario(
    "create: capped, size and max",
    *[create(f"k{i}", capped=v, size=4096) for i, v in enumerate(BOOLS)],
    *[create(f"z{i}", capped=True, size=v) for i, v in enumerate(BAD)],
    *[create(f"m{i}", capped=True, size=4096, max=v) for i, v in enumerate(BAD)],
    create("n1", size=4096),
    create("n2", max=5),
    create("n3", capped=True),
    create("n4", capped=False, size=4096, max=5),
    listed(),
)
scenario(
    "create: validator, level and action",
    *[create(f"v{i}", validator=v) for i, v in enumerate(BAD + [{"a": {"$gt": 1}}])],
    *[
        create(f"l{i}", validationLevel=v)
        for i, v in enumerate(["strict", "moderate", "off", "nope", "", 1, None, {}])
    ],
    *[
        create(f"a{i}", validationAction=v)
        for i, v in enumerate(["error", "warn", "errorAndLog", "nope", "", 1, None, {}])
    ],
    create("w1", validator={"a": {"$nope": 1}}),
    create("w2", validator={"$jsonSchema": {"bsonType": "object"}}),
    create("w3", validator={"$jsonSchema": {"bsonType": "nope"}}),
    create("w4", validator={"$expr": {"$gt": ["$a", 1]}}),
    create("w5", validator={"$where": "true"}),
    listed(),
)
scenario(
    "create: collation",
    *[create(f"c{i}", collation=v) for i, v in enumerate(BAD)],
    create("d1", collation={"locale": "en"}),
    create("d2", collation={"locale": "simple"}),
    create("d3", collation={"locale": "zz"}),
    create("d4", collation={"locale": "en", "strength": 9}),
    create("d5", collation={"locale": "en", "nope": 1}),
    create("d6", collation={"strength": 2}),
    create("d7", collation={"locale": "en", "strength": 2, "caseLevel": True}),
    create("d1", collation={"locale": "en"}),
    create("d1", collation={"locale": "fr"}),
    create("d1"),
    listed(),
)
scenario(
    "create: pre-images, expiry and the clustered index",
    *[create(f"p{i}", changeStreamPreAndPostImages=v) for i, v in enumerate(BAD)],
    create("q1", changeStreamPreAndPostImages={"enabled": True}),
    create("q2", changeStreamPreAndPostImages={"enabled": False}),
    create("q3", changeStreamPreAndPostImages={"enabled": 1}),
    create("q4", changeStreamPreAndPostImages={}),
    create("q5", changeStreamPreAndPostImages={"enabled": True, "nope": 1}),
    *[create(f"e{i}", expireAfterSeconds=v) for i, v in enumerate(BAD)],
    create("r1", clusteredIndex={"key": {"_id": 1}, "unique": True}),
    create("r2", clusteredIndex={"key": {"_id": 1}, "unique": True}, expireAfterSeconds=10),
    create("r3", clusteredIndex={"key": {"a": 1}, "unique": True}),
    create("r4", clusteredIndex={"key": {"_id": 1}}),
    create("r5", clusteredIndex={"key": {"_id": 1}, "unique": False}),
    create("r6", clusteredIndex=True),
    create("r7", clusteredIndex={"key": {"_id": 1}, "unique": True, "name": "mine"}),
    create("r8", clusteredIndex={"key": {"_id": 1}, "unique": True}, capped=True, size=4096),
    listed(),
)
scenario(
    "create: the remaining options",
    create("o1", nope=1),
    create("o2", comment="x"),
    create("o3", comment={"a": 1}),
    create("o4", writeConcern={"w": 1}),
    create("o5", writeConcern={"w": "nope"}),
    create("o6", writeConcern=1),
    create("o7", maxTimeMS=1000),
    create("o8", maxTimeMS="x"),
    create("o9", storageEngine={"wiredTiger": {"configString": "block_compressor=zlib"}}),
    create("o10", storageEngine={"nope": {}}),
    create("o11", storageEngine=1),
    create("o12", indexOptionDefaults={"storageEngine": {"wiredTiger": {}}}),
    create("o13", indexOptionDefaults=1),
    create("o14", autoIndexId=True),
    create("o15", autoIndexId=False),
    create("o16", idIndex={"key": {"_id": 1}, "name": "_id_"}),
    create("o17", idIndex={"key": {"_id": 1}, "name": "mine"}),
    create("o18", idIndex={"key": {"a": 1}, "name": "_id_"}),
    create("o19", idIndex=1),
    create("o20", flags=1),
    create("o21", temp=True),
    create("o22", encryptedFields={}),
    create("o23", recordIdsReplicated=True),
    create("o24", apiVersion="1"),
    create("o25", readConcern={"level": "local"}),
    create("o26", lsid={"id": 1}),
    listed(),
)
scenario(
    "create: one that exists",
    create("c", capped=True, size=4096),
    create("c", capped=True, size=4096),
    create("c", capped=True, size=8192),
    create("c", capped=True, size=4096, max=3),
    create("c"),
    create("v", viewOn="c", pipeline=[]),
    create("v", viewOn="c", pipeline=[]),
    create("v", viewOn="c", pipeline=[{"$match": {}}]),
    create("v"),
    create("c", viewOn="c", pipeline=[]),
    create("u", validator={"a": 1}),
    create("u", validator={"a": 1}),
    create("u", validator={"a": 2}),
    create("u", validator={"a": 1}, validationLevel="strict"),
    create("u", validator={"a": 1}, validationAction="error"),
    create("u", validator={"a": 1}, validationLevel="moderate"),
    {"insert": "i", "documents": [{"_id": 1}]},
    create("i"),
    create("i", capped=True, size=4096),
    listed(),
)
scenario(
    "create: a view",
    *SEED,
    *[create(f"a{i}", viewOn=v) for i, v in enumerate(BAD + ["", "c", "nope", "system.views"])],
    *[
        create(f"p{i}", viewOn="c", pipeline=v)
        for i, v in enumerate(["x", 1, None, {}, [1], [{}], [{"$nope": 1}], [{"$out": "o"}]])
    ],
    create("w1", pipeline=[]),
    create("w2", viewOn="c"),
    create("w3", viewOn="c", pipeline=[], capped=True, size=4096),
    create("w4", viewOn="c", pipeline=[], validator={"a": 1}),
    create("w5", viewOn="c", pipeline=[], collation={"locale": "en"}),
    create("w6", viewOn="w2", pipeline=[{"$match": {"a": {"$gt": 1}}}]),
    create("w7", viewOn="w7", pipeline=[]),
    create("w8", viewOn="@db.c", pipeline=[]),
    listed(filter={"type": "view"}),
    {"find": "system.views", "filter": {}, "sort": {"_id": 1}},
)

# --------------------------------------------------------- listCollections

scenario(
    "listCollections: arguments",
    *SEED,
    create("k", capped=True, size=4096),
    create("v", viewOn="c", pipeline=[]),
    listed(),
    *[{"listCollections": v} for v in ["x", 2.5, None, True, {}, 0, -1]],
    *[listed(nameOnly=v) for v in BOOLS],
    *[listed(authorizedCollections=v) for v in BOOLS],
    *[listed(filter=v) for v in ["x", 1, None, [], True]],
    *[listed(cursor=v) for v in ["x", 1, None, [], {}, {"batchSize": "x"}, {"nope": 1}]],
    listed(nope=1),
    listed(comment="x"),
    listed(maxTimeMS=1000),
    listed(includePendingDrops=True),
)
scenario(
    "listCollections: filters",
    *SEED,
    create("k", capped=True, size=4096),
    create("v", viewOn="c", pipeline=[]),
    create("w", validator={"a": 1}),
    listed(filter={"name": "c"}),
    listed(filter={"name": "nope"}),
    listed(filter={"name": {"$in": ["c", "k"]}}),
    listed(filter={"name": {"$regex": "^[ck]"}}),
    listed(filter={"type": "collection"}),
    listed(filter={"type": "view"}),
    listed(filter={"type": "timeseries"}),
    listed(filter={"options.capped": True}),
    listed(filter={"options.validator": {"$exists": True}}),
    listed(filter={"info.readOnly": True}),
    listed(filter={"idIndex.name": "_id_"}),
    listed(filter={"name": {"$nope": 1}}),
    listed(filter={"$nope": 1}),
    listed(filter={"name": "c"}, nameOnly=True),
    listed(filter={"options.capped": True}, nameOnly=True),
    listed(filter={"$or": [{"name": "c"}, {"type": "view"}]}),
    listed(filter={"name": 1}),
    ("@missing", listed()),
    ("@missing", listed(nameOnly=True)),
)
scenario(
    "listCollections: the cursor",
    *[create(f"c{i}") for i in range(5)],
    listed(cursor={"batchSize": 2}, nameOnly=True),
    listed(cursor={"batchSize": 0}, nameOnly=True),
    listed(cursor={"batchSize": 5}, nameOnly=True),
    listed(cursor={"batchSize": 100}, nameOnly=True),
    listed(cursor={"batchSize": -1}, nameOnly=True),
)

# ------------------------------------------------------------------ collMod

scenario(
    "collMod: arguments",
    *SEED,
    *[{"collMod": v} for v in ["", 1, None, {}, "nope", "system.views"]],
    {"collMod": "c"},
    {"collMod": "c", "nope": 1},
    {"collMod": "c", "comment": "x"},
    {"collMod": "c", "writeConcern": {"w": 1}},
    {"collMod": "c", "dryRun": True},
    {"collMod": "c", "dryRun": True, "validator": {"a": 1}},
    {"collMod": "c", "maxTimeMS": 1000},
    ("@missing", {"collMod": "c", "validator": {}}),
    listed(),
)
scenario(
    "collMod: validator, level and action",
    *SEED,
    *[{"collMod": "c", "validator": v} for v in BAD + [{"a": {"$gte": 0}}, {}]],
    *[
        {"collMod": "c", "validationLevel": v}
        for v in ["strict", "moderate", "off", "nope", "", 1, None]
    ],
    *[
        {"collMod": "c", "validationAction": v}
        for v in ["error", "warn", "errorAndLog", "nope", "", 1, None]
    ],
    {"collMod": "c", "validator": {"a": {"$nope": 1}}},
    {"collMod": "c", "validator": {"$jsonSchema": {"bsonType": "nope"}}},
    listed(),
    {"collMod": "c", "validator": {}, "validationLevel": "off"},
    listed(),
)
scenario(
    "collMod: pre-images and expiry",
    *SEED,
    *[{"collMod": "c", "changeStreamPreAndPostImages": v} for v in BAD],
    {"collMod": "c", "changeStreamPreAndPostImages": {"enabled": True}},
    listed(),
    {"collMod": "c", "changeStreamPreAndPostImages": {"enabled": False}},
    listed(),
    {"collMod": "c", "changeStreamPreAndPostImages": {}},
    {"collMod": "c", "changeStreamPreAndPostImages": {"enabled": 1}},
    *[{"collMod": "c", "expireAfterSeconds": v} for v in BAD + ["off"]],
    create("r", clusteredIndex={"key": {"_id": 1}, "unique": True}),
    {"collMod": "r", "expireAfterSeconds": 10},
    {"collMod": "r", "expireAfterSeconds": "off"},
    {"collMod": "r", "expireAfterSeconds": -1},
    listed(),
)
scenario(
    "collMod: index",
    *SEED,
    *[{"collMod": "c", "index": v} for v in BAD],
    {"collMod": "c", "index": {}},
    {"collMod": "c", "index": {"name": "a_1"}},
    {"collMod": "c", "index": {"name": "nope", "hidden": True}},
    {"collMod": "c", "index": {"keyPattern": {"a": 1}, "hidden": True}},
    {"collMod": "c", "index": {"keyPattern": {"a": 1}, "hidden": True}},
    {"collMod": "c", "index": {"name": "a_1", "hidden": False}},
    {"collMod": "c", "index": {"name": "a_1", "keyPattern": {"a": 1}, "hidden": True}},
    {"collMod": "c", "index": {"name": "_id_", "hidden": True}},
    {"collMod": "c", "index": {"name": "a_1", "expireAfterSeconds": 100}},
    {"collMod": "c", "index": {"name": "a_1", "expireAfterSeconds": 50}},
    {"collMod": "c", "index": {"name": "a_1", "unique": True}},
    {"collMod": "c", "index": {"name": "a_1", "prepareUnique": True}},
    {"collMod": "c", "index": {"name": "a_1", "unique": True}},
    {"collMod": "c", "index": {"name": "a_1", "nope": 1}},
    {"listIndexes": "c"},
)
scenario(
    "collMod: a view",
    *SEED,
    create("v", viewOn="c", pipeline=[]),
    {"collMod": "v", "viewOn": "c", "pipeline": [{"$match": {"a": {"$gt": 1}}}]},
    {"collMod": "v", "viewOn": "c"},
    {"collMod": "v", "pipeline": []},
    {"collMod": "v", "viewOn": "nope", "pipeline": []},
    {"collMod": "v", "viewOn": "v", "pipeline": []},
    {"collMod": "v", "viewOn": "c", "pipeline": [{"$nope": 1}]},
    {"collMod": "v", "validator": {"a": 1}},
    {"collMod": "v", "index": {"name": "a_1", "hidden": True}},
    {"collMod": "c", "viewOn": "c", "pipeline": []},
    listed(filter={"name": "v"}),
    {"find": "v", "filter": {}, "sort": {"_id": 1}},
)

# --------------------------------------------------------- renameCollection

scenario(
    "renameCollection: arguments",
    *SEED,
    {"renameCollection": "@db.c", "to": "@db.d"},
    *[admin({"renameCollection": v, "to": "@db.d"}) for v in ["", 1, None, {}, "c", "@db.", ".c"]],
    *[admin({"renameCollection": "@db.c", "to": v}) for v in ["", 1, None, {}, "d", "@db.", ".d"]],
    admin({"renameCollection": "@db.c"}),
    *[rename("c", "d", dropTarget=v) for v in ["x", None, {}, 2.5]],
    rename("c", "c2", nope=1),
    listed(nameOnly=True),
)
scenario(
    "renameCollection: what moves",
    *SEED,
    create("k", capped=True, size=4096, validator={"a": 1}),
    {"listIndexes": "c"},
    listed(),
    rename("c", "d"),
    rename("k", "l"),
    listed(),
    {"listIndexes": "d"},
    {"find": "d", "filter": {}, "sort": {"_id": 1}},
    {"count": "c"},
    {"listIndexes": "c"},
    rename("d", "d"),
    rename("nope", "e"),
    rename("d", "l"),
    rename("d", "l", dropTarget=False),
    rename("d", "l", dropTarget=True),
    listed(),
    {"count": "l"},
)
scenario(
    "renameCollection: names and namespaces",
    *SEED,
    rename("c", "system.x"),
    rename("c", "a$b"),
    rename("c", "x" * 256),
    rename("c", "a.b.c"),
    rename("a.b.c", "c"),
    rename("c", "oplog.rs"),
    admin({"renameCollection": "@db.c", "to": "@other.c"}),
    ("@other", listed(nameOnly=True)),
    ("@other", {"listIndexes": "c"}),
    ("@other", {"count": "c"}),
    admin({"renameCollection": "@other.c", "to": "@db.c"}),
    admin({"renameCollection": "@db.c", "to": "admin.ddlp_moved"}),
    admin({"renameCollection": "@db.c", "to": "local.ddlp_moved"}),
    admin({"renameCollection": "local.oplog.rs", "to": "@db.o"}),
    admin({"renameCollection": "@db.system.views", "to": "@db.sv"}),
    ("@other", {"dropDatabase": 1}),
    listed(nameOnly=True),
)
scenario(
    "renameCollection: views and cursors",
    *SEED,
    create("v", viewOn="c", pipeline=[]),
    rename("v", "w"),
    rename("c", "v"),
    rename("c", "v", dropTarget=True),
    {"find": "c", "filter": {}, "batchSize": 2, "sort": {"_id": 1}, "save": "k"},
    rename("c", "d"),
    {"getMore": "@cursor:k", "collection": "c"},
    {"find": "v", "filter": {}},
    listed(nameOnly=True),
)

# ----------------------------------------------------- drop and dropDatabase

scenario(
    "drop",
    *SEED,
    create("v", viewOn="c", pipeline=[]),
    *[{"drop": v} for v in ["", 1, None, {}, "nope", "system.views", "system.nope"]],
    {"drop": "c", "nope": 1},
    {"drop": "c", "comment": "x", "writeConcern": {"w": 1}},
    {"drop": "c"},
    {"drop": "v"},
    {"drop": "v"},
    {"insert": "e", "documents": [{"_id": 1}]},
    {
        "createIndexes": "e",
        "indexes": [{"key": {"a": 1}, "name": "a_1"}, {"key": {"b": 1}, "name": "b_1"}],
    },
    {"drop": "e"},
    {"drop": "e", "writeConcern": {"w": "nope"}},
    ("@missing", {"drop": "c"}),
    listed(),
)
scenario(
    "dropDatabase",
    *SEED,
    *[{"dropDatabase": v} for v in ["x", None, {}, 0, 2.5, True, -1]],
    listed(nameOnly=True),
    *SEED,
    {"dropDatabase": 1, "nope": 1},
    {"dropDatabase": 1, "comment": "x"},
    {"dropDatabase": 1, "writeConcern": {"w": "nope"}},
    {"dropDatabase": 1, "writeConcern": {"w": 1}},
    {"dropDatabase": 1},
    listed(),
    ("@missing", {"dropDatabase": 1}),
    admin({"listDatabases": 1, "filter": {"name": "@db"}, "nameOnly": True}),
)

# ----------------------------------------------------------------- the stats

scenario(
    "dbStats",
    {"dbStats": 1},
    *SEED,
    create("v", viewOn="c", pipeline=[]),
    {"dbStats": 1},
    *[{"dbStats": v} for v in ["x", None, {}, 0, 2.5]],
    *[{"dbStats": 1, "scale": v} for v in BAD + [1024, 0.5]],
    *[{"dbStats": 1, "freeStorage": v} for v in [1, 0, True, "x", None]],
    {"dbStats": 1, "nope": 1},
    ("@missing", {"dbStats": 1}),
)
scenario(
    "collStats",
    *SEED,
    create("k", capped=True, size=4096, max=3),
    create("v", viewOn="c", pipeline=[]),
    {"collStats": "c"},
    {"collStats": "k"},
    {"collStats": "v"},
    {"collStats": "nope"},
    *[{"collStats": v} for v in ["", 1, None, {}]],
    *[{"collStats": "c", "scale": v} for v in BAD + [1024, 0.5]],
    {"collStats": "c", "verbose": True},
    {"collStats": "c", "nope": 1},
    ("@missing", {"collStats": "c"}),
    {"dataSize": "@db.c"},
    {"dataSize": "@db.nope"},
    {"dataSize": "c"},
    {"dataSize": 1},
)
scenario(
    "listDatabases",
    *SEED,
    admin({"listDatabases": 1, "filter": {"name": "@db"}}),
    admin({"listDatabases": 1, "filter": {"name": "@db"}, "nameOnly": True}),
    *[admin({"listDatabases": v, "filter": {"name": "@db"}}) for v in ["x", None, {}, 0, 2.5]],
    *[admin({"listDatabases": 1, "filter": {"name": "@db"}, "nameOnly": v}) for v in BOOLS],
    *[admin({"listDatabases": 1, "filter": v}) for v in ["x", 1, [], True]],
    *[
        admin({"listDatabases": 1, "filter": {"name": "@db"}, "authorizedDatabases": v})
        for v in BOOLS
    ],
    admin({"listDatabases": 1, "filter": {"name": "@db"}, "nope": 1}),
    admin({"listDatabases": 1, "filter": {"name": {"$regex": "^@db$"}}}),
    admin({"listDatabases": 1, "filter": {"name": "@db", "sizeOnDisk": {"$gt": 0}}}),
    admin({"listDatabases": 1, "filter": {"name": "@db", "empty": False}}),
    admin({"listDatabases": 1, "filter": {"name": {"$nope": 1}}}),
    admin({"listDatabases": 1, "filter": {"name": "@missing"}}),
    admin({"listDatabases": 1, "filter": {"name": {"$in": ["admin", "local", "config"]}}}),
    {"listDatabases": 1, "filter": {"name": "@db"}},
    {"create": "empty"},
    {"drop": "c"},
    admin({"listDatabases": 1, "filter": {"name": "@db"}}),
)


# ------------------------------------------------------------------ running

UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def resolve(value, db, cursors):
    if isinstance(value, dict):
        return {k: resolve(v, db, cursors) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve(v, db, cursors) for v in value]
    if isinstance(value, str):
        if value.startswith("@cursor:"):
            return Int64(cursors.get(value[8:], 0))
        return value.replace("@db", db).replace("@other", db + "_o").replace("@missing", db + "_m")
    return value


def kind(value):
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, Int64):
        return "long"
    if isinstance(value, int):
        return "int"
    if isinstance(value, float):
        return "double"
    return type(value).__name__


def clean(value, db, stats=False, key=None):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k in DROP or k in ENGINE:
                continue
            if stats and k in SIZES:
                out[k] = f"<{kind(v)}>"
            elif stats and k == "indexSizes" and isinstance(v, dict):
                out[k] = {name: f"<{kind(n)}>" for name, n in v.items()}
            else:
                out[k] = clean(v, db, stats, k)
        return out
    if isinstance(value, list):
        return [clean(v, db, stats, key) for v in value]
    if isinstance(value, bytes) and len(value) == 16:
        return "ID"
    if isinstance(value, Int64):
        if key == "id" and value > 10**6:
            return "CURSOR"
        return f"Int64({int(value)})"
    if isinstance(value, float):
        return "NaN" if value != value else f"double({value})"
    if isinstance(value, str):
        text = value.replace(db, "@db")
        text = UUID_TEXT.sub("UUID", text)
        text = re.sub(
            r"expected types '\[([^\]]*)\]'",
            lambda m: "expected types '[" + ", ".join(sorted(m.group(1).split(", "))) + "]'",
            text,
        )
        return re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(T)", text)
    return value


def run_scenario(uri, index, steps):
    db = f"ddlp{RUN}_{index}"
    wire = Wire(uri)
    cursors: dict[str, int] = {}
    replies = []
    for step in steps:
        where, command = step if isinstance(step, tuple) else ("@db", step)
        command = dict(command)
        save = command.pop("save", None)
        where = resolve(where, db, cursors)
        name = next(iter(command), "")
        try:
            reply = wire.command(where, resolve(command, db, cursors))
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
        if save and isinstance(reply.get("cursor"), dict):
            cursors[save] = reply["cursor"].get("id", 0)
        cleaned = clean(reply, db, stats=name in STATS)
        batch = cleaned.get("cursor", {}).get("firstBatch") if name == "listCollections" else None
        if isinstance(batch, list):
            # mongod lists collections in no promised order.
            if reply["cursor"].get("id"):
                cleaned["cursor"]["firstBatch"] = f"{len(batch)} of them"
            else:
                batch.sort(key=lambda c: str(c.get("name")))
        replies.append(cleaned)
    try:
        for name in (db, db + "_o", db + "_m"):
            wire.command(name, {"dropDatabase": 1})
        for where, coll in (("admin", "ddlp_moved"), ("local", "ddlp_moved")):
            wire.command(where, {"drop": coll})
    except (TimeoutError, ConnectionError, OSError):
        print(f"  NOTE: cleanup of {db} did not finish on {uri}", file=sys.stderr)
    finally:
        wire.close()
    return replies


def show(step):
    where, command = step if isinstance(step, tuple) else (None, step)
    body = {k: v for k, v in command.items() if k != "save"}
    return (("" if where is None else f"[{where}] ") + repr(body))[:220]


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "900"))

    # Self-check: the reference must create, list and count as the probe assumes.
    check = run_scenario(mongod_uri, 0, [*SEED, listed(nameOnly=True), {"collStats": "c"}])
    names = [c.get("name") for c in check[2].get("cursor", {}).get("firstBatch", [])]
    if not (names == ["c"] and check[3].get("count") == 5 and check[3].get("nindexes") == 2):
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
        print(f"\n--- {name} ({len(bad)} of {len(steps)})")
        for i in bad:
            print(f"  step {i}: {show(steps[i])}")
            print(f"    mongod: {str(want[i])[:width]}")
            print(f"    server: {str(got[i])[:width]}")
    print(
        f"\n=== DDL and admin: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
