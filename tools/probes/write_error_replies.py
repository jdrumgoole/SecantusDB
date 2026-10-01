"""Error replies from REAL failures, against mongod: the whole reply.

`error_labels.py` covers errors injected with `failCommand`. This covers the ones
a server raises itself, the way an application meets them -- duplicate keys
(with `keyPattern` / `keyValue`), update-operator mistakes, `findAndModify`
misuse, index and collection DDL errors, bad `find` / `aggregate` arguments --
and compares the ENTIRE reply: `ok`, `code`, `codeName`, `errmsg`, every
`writeErrors` entry, `n` / `nModified` / `upserted`. Each case runs in a fresh
collection.

Compares the Rust server only:

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27058/?directConnection=true" \\
        python tools/probes/write_error_replies.py [--show]

Replica-set bookkeeping (`$clusterTime`, `operationTime`, `opTime`,
`electionId`) is dropped before comparing; ObjectIds and the collection UUID in
messages are normalised.
"""

from __future__ import annotations

import os
import re
import sys
from collections.abc import Callable
from typing import Any

import pymongo
from bson import ObjectId

MONGOD = os.environ.get("PROBE_MONGOD")
SERVER = os.environ.get("PROBE_SERVER")
DB = "err_replies_probe"
DROP = {"$clusterTime", "operationTime", "opTime", "electionId"}


def normalise(v: Any) -> Any:
    if isinstance(v, dict):
        return {k: normalise(x) for k, x in v.items() if k not in DROP}
    if isinstance(v, list):
        return [normalise(x) for x in v]
    if isinstance(v, ObjectId):
        return "<oid>"
    if isinstance(v, str):
        v = re.sub(r"ObjectId\('[0-9a-f]{24}'\)", "ObjectId(<oid>)", v)
        v = re.sub(r"UUID\(\"[0-9a-f-]{36}\"\)", "UUID(<uuid>)", v)
        v = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "<uuid>", v)
        return v
    return v


Setup = Callable[[Any], None]


def docs(*ds: dict[str, Any]) -> Setup:
    return lambda c: c.insert_many(list(ds)) if ds else None


def index(keys: list[tuple[str, int]], **opts: Any) -> Setup:
    return lambda c: c.create_index(keys, **opts)


def both(*steps: Setup) -> Setup:
    def run(c: Any) -> None:
        for s in steps:
            s(c)

    return run


#: (label, setup, command). `{coll}` in a string value is the case's collection.
CASES: list[tuple[str, Setup, dict[str, Any]]] = [
    # --- duplicate keys
    ("insert dup _id", docs({"_id": 1}), {"insert": "{coll}", "documents": [{"_id": 1}]}),
    (
        "insert dup unique field",
        both(index([("a", 1)], unique=True), docs({"_id": 1, "a": 5})),
        {"insert": "{coll}", "documents": [{"_id": 2, "a": 5}]},
    ),
    (
        "insert dup compound unique",
        both(index([("a", 1), ("b", -1)], unique=True), docs({"_id": 1, "a": 1, "b": "x"})),
        {"insert": "{coll}", "documents": [{"_id": 2, "a": 1, "b": "x"}]},
    ),
    (
        "insert many ordered stops",
        docs({"_id": 2}),
        {"insert": "{coll}", "documents": [{"_id": 1}, {"_id": 2}, {"_id": 3}]},
    ),
    (
        "insert many unordered continues",
        docs({"_id": 2}),
        {
            "insert": "{coll}",
            "documents": [{"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 2}],
            "ordered": False,
        },
    ),
    (
        "update makes dup",
        both(index([("a", 1)], unique=True), docs({"_id": 1, "a": 1}, {"_id": 2, "a": 2})),
        {"update": "{coll}", "updates": [{"q": {"_id": 2}, "u": {"$set": {"a": 1}}}]},
    ),
    (
        "upsert dup",
        both(index([("a", 1)], unique=True), docs({"_id": 1, "a": 1})),
        {
            "update": "{coll}",
            "updates": [{"q": {"_id": 9}, "u": {"$set": {"a": 1}}, "upsert": True}],
        },
    ),
    (
        "dup key with null",
        both(index([("a", 1)], unique=True), docs({"_id": 1})),
        {"insert": "{coll}", "documents": [{"_id": 2}]},
    ),
    # --- update operator mistakes
    (
        "inc on string",
        docs({"_id": 1, "a": "x"}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$inc": {"a": 1}}}]},
    ),
    (
        "inc non-numeric arg",
        docs({"_id": 1, "a": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$inc": {"a": "x"}}}]},
    ),
    (
        "push on non-array",
        docs({"_id": 1, "a": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$push": {"a": 1}}}]},
    ),
    (
        "conflicting paths",
        docs({"_id": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 1, "a.b": 2}}}]},
    ),
    (
        "conflicting ops",
        docs({"_id": 1}),
        {
            "update": "{coll}",
            "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 1}, "$inc": {"a": 2}}}],
        },
    ),
    (
        "empty modifier",
        docs({"_id": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$set": {}}}]},
    ),
    (
        "unknown modifier",
        docs({"_id": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$foo": {"a": 1}}}]},
    ),
    (
        "mixed op and field",
        docs({"_id": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 1}, "b": 2}}]},
    ),
    (
        "modify _id",
        docs({"_id": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$set": {"_id": 2}}}]},
    ),
    (
        "replace changes _id",
        docs({"_id": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"_id": 2, "a": 1}}]},
    ),
    (
        "rename onto self",
        docs({"_id": 1, "a": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$rename": {"a": "a"}}}]},
    ),
    (
        "rename to _id",
        docs({"_id": 1, "a": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$rename": {"a": "_id"}}}]},
    ),
    (
        "path not viable",
        docs({"_id": 1, "a": 5}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a.b": 1}}}]},
    ),
    (
        "positional no match",
        docs({"_id": 1, "a": [1]}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a.$": 1}}}]},
    ),
    (
        "dollar field in replace",
        docs({"_id": 1}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"a": {"$x": 1}}}]},
    ),
    (
        "multi with replacement",
        docs({"_id": 1}),
        {"update": "{coll}", "updates": [{"q": {}, "u": {"a": 1}, "multi": True}]},
    ),
    (
        "array filter unused",
        docs({"_id": 1, "a": [1]}),
        {
            "update": "{coll}",
            "updates": [{"q": {"_id": 1}, "u": {"$set": {"a.0": 2}}, "arrayFilters": [{"x": 1}]}],
        },
    ),
    (
        "array filter missing",
        docs({"_id": 1, "a": [1]}),
        {"update": "{coll}", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a.$[x]": 2}}}]},
    ),
    # --- findAndModify
    (
        "fam remove and update",
        docs({"_id": 1}),
        {"findAndModify": "{coll}", "query": {}, "remove": True, "update": {"$set": {"a": 1}}},
    ),
    (
        "fam new with remove",
        docs({"_id": 1}),
        {"findAndModify": "{coll}", "query": {}, "remove": True, "new": True},
    ),
    ("fam neither", docs({"_id": 1}), {"findAndModify": "{coll}", "query": {}}),
    (
        "fam dup",
        both(index([("a", 1)], unique=True), docs({"_id": 1, "a": 1}, {"_id": 2})),
        {"findAndModify": "{coll}", "query": {"_id": 2}, "update": {"$set": {"a": 1}}},
    ),
    # --- indexes
    (
        "index same name other keys",
        index([("a", 1)], name="ix"),
        {"createIndexes": "{coll}", "indexes": [{"key": {"b": 1}, "name": "ix"}]},
    ),
    (
        "index same keys other name",
        index([("a", 1)], name="ix"),
        {"createIndexes": "{coll}", "indexes": [{"key": {"a": 1}, "name": "other"}]},
    ),
    (
        "index same keys other options",
        index([("a", 1)]),
        {"createIndexes": "{coll}", "indexes": [{"key": {"a": 1}, "name": "a_1", "unique": True}]},
    ),
    (
        "unique index over dups",
        docs({"_id": 1, "a": 1}, {"_id": 2, "a": 1}),
        {"createIndexes": "{coll}", "indexes": [{"key": {"a": 1}, "name": "a_1", "unique": True}]},
    ),
    (
        "index bad key value",
        docs(),
        {"createIndexes": "{coll}", "indexes": [{"key": {"a": "x"}, "name": "ax"}]},
    ),
    ("index empty key", docs(), {"createIndexes": "{coll}", "indexes": [{"key": {}, "name": "e"}]}),
    ("index no name", docs(), {"createIndexes": "{coll}", "indexes": [{"key": {"a": 1}}]}),
    ("drop missing index", docs({"_id": 1}), {"dropIndexes": "{coll}", "index": "nope"}),
    ("drop _id index", docs({"_id": 1}), {"dropIndexes": "{coll}", "index": "_id_"}),
    # --- collections
    ("create existing", docs({"_id": 1}), {"create": "{coll}"}),
    ("drop missing", docs(), {"drop": "{coll}_absent"}),
    (
        "rename onto existing",
        both(
            docs({"_id": 1}),
            lambda c: c.database["{coll}_t".replace("{coll}", c.name)].insert_one({"_id": 1}),
        ),
        {"renameCollection": "{db}.{coll}", "to": "{db}.{coll}_t"},
    ),
    (
        "rename missing source",
        docs(),
        {"renameCollection": "{db}.{coll}_absent", "to": "{db}.{coll}_x"},
    ),
    ("invalid collection name", docs(), {"create": "a$b"}),
    ("empty collection name", docs(), {"create": ""}),
    ("collMod unknown option", docs({"_id": 1}), {"collMod": "{coll}", "nope": 1}),
    # --- find / aggregate arguments
    ("find bad sort value", docs({"_id": 1}), {"find": "{coll}", "sort": {"a": 2}}),
    ("find mixed projection", docs({"_id": 1}), {"find": "{coll}", "projection": {"a": 1, "b": 0}}),
    ("find negative limit and skip", docs({"_id": 1}), {"find": "{coll}", "skip": -1}),
    ("find unknown operator", docs({"_id": 1}), {"find": "{coll}", "filter": {"a": {"$foo": 1}}}),
    ("find bad regex", docs({"_id": 1}), {"find": "{coll}", "filter": {"a": {"$regex": "("}}}),
    (
        "aggregate unknown stage",
        docs({"_id": 1}),
        {"aggregate": "{coll}", "pipeline": [{"$foo": {}}], "cursor": {}},
    ),
    (
        "aggregate group no _id",
        docs({"_id": 1}),
        {"aggregate": "{coll}", "pipeline": [{"$group": {"n": {"$sum": 1}}}], "cursor": {}},
    ),
    (
        "aggregate out not last",
        docs({"_id": 1}),
        {"aggregate": "{coll}", "pipeline": [{"$out": "x"}, {"$match": {}}], "cursor": {}},
    ),
    ("aggregate no cursor", docs({"_id": 1}), {"aggregate": "{coll}", "pipeline": []}),
    ("count bad query", docs({"_id": 1}), {"count": "{coll}", "query": {"$foo": 1}}),
    ("distinct no key", docs({"_id": 1}), {"distinct": "{coll}"}),
    (
        "delete bad limit",
        docs({"_id": 1}),
        {"delete": "{coll}", "deletes": [{"q": {}, "limit": 5}]},
    ),
]


def fill(v: Any, coll: str, db: str) -> Any:
    if isinstance(v, str):
        return v.replace("{coll}", coll).replace("{db}", db)
    if isinstance(v, dict):
        return {fill(k, coll, db): fill(x, coll, db) for k, x in v.items()}
    if isinstance(v, list):
        return [fill(x, coll, db) for x in v]
    return v


def measure(uri: str) -> list[Any]:
    client = pymongo.MongoClient(uri)
    client.drop_database(DB)
    db = client[DB]
    out = []
    for i, (_label, setup, cmd) in enumerate(CASES):
        name = f"c{i}"
        coll = db[name]
        setup(coll)
        try:
            reply = db.command(fill(cmd, name, DB), check=False)
        except pymongo.errors.PyMongoError as e:
            reply = {"client-error": type(e).__name__}
        out.append(normalise(reply))
    client.drop_database(DB)
    client.close()
    return out


def main() -> int:
    if not (MONGOD and SERVER):
        print("PROBE_MONGOD and PROBE_SERVER are required (see the module docstring)")
        return 2
    show = "--show" in sys.argv
    want, got = measure(MONGOD), measure(SERVER)
    if want[0].get("writeErrors", [{}])[0].get("code") != 11000:
        print("SELF-CHECK FAILED: mongod did not report a duplicate key for case 0")
        return 2
    bad = 0
    for (label, _setup, cmd), w, g in zip(CASES, want, got, strict=True):
        if show:
            print(f"{label}: {cmd}\n  mongod: {w}")
        if w != g:
            bad += 1
            print(f"DIFF {label}: {cmd}\n  mongod: {w}\n  ours:   {g}")
    print(f"=== error replies: {bad} of {len(CASES)} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
