"""Differential-probe views against a real mongod.

One stateful sequence of commands over two collections and the views built on
them: what `create` and `collMod` accept as a view definition, what reads
through a view answer (`find`, `count`, `distinct`, `$lookup`, `$graphLookup`,
`$unionWith` from one), and what every write, index and collection command
answers when aimed at a view.

Run against mongod 8.2.11 on 2026-10-09, the Rust server accepted an `insert`
into a view, answered `[]` for `distinct` on one, matched nothing in a
`$lookup` from one, and ignored a `collMod` of its pipeline.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/views.py

`distinct` values are compared as a set: mongod returns a view's in hash order.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _servers import probe_targets, report  # noqa: E402
from bson import Int64  # noqa: E402
from pymongo.errors import PyMongoError  # noqa: E402

NOISE = ("$clusterTime", "operationTime", "opTime", "electionId", "uuid", "idIndex", "localTime")
DB = "views_probe"
MATCH_G1 = [{"$match": {"g": 1}}]
SET_A = {"$set": {"a": 1}}
LOOKUP = {"from": "v1", "localField": "g", "foreignField": "g", "as": "m"}
GRAPH = {
    "from": "v1",
    "startWith": "$g",
    "connectFromField": "g",
    "connectToField": "g",
    "as": "m",
    "maxDepth": 0,
}
SIZE_OF_M = [{"$project": {"n": {"$size": "$m"}}}, {"$sort": {"_id": 1}}]

#: (label, command). `admin:` in the label runs it on the admin database.
SEQUENCE = [
    ("create", {"create": "v1", "viewOn": "src", "pipeline": MATCH_G1}),
    ("create on a view", {"create": "v4", "viewOn": "v1", "pipeline": [{"$project": {"g": 1}}]}),
    ("create on a missing collection", {"create": "v3", "viewOn": "nosuch", "pipeline": []}),
    ("create without a pipeline", {"create": "v2", "viewOn": "src"}),
    ("create pipeline without viewOn", {"create": "bad", "pipeline": []}),
    ("create unknown stage", {"create": "bad", "viewOn": "src", "pipeline": [{"$nope": 1}]}),
    ("create $out", {"create": "bad", "viewOn": "src", "pipeline": [{"$out": "x"}]}),
    ("create $merge", {"create": "bad", "viewOn": "src", "pipeline": [{"$merge": {"into": "x"}}]}),
    (
        "create $changeStream",
        {"create": "bad", "viewOn": "src", "pipeline": [{"$changeStream": {}}]},
    ),
    ("create pipeline not an array", {"create": "bad", "viewOn": "src", "pipeline": {"a": 1}}),
    ("create viewOn empty", {"create": "bad", "viewOn": "", "pipeline": []}),
    ("create viewOn a number", {"create": "bad", "viewOn": 5, "pipeline": []}),
    ("create on itself", {"create": "bad", "viewOn": "bad", "pipeline": []}),
    ("listCollections bad", {"listCollections": 1, "filter": {"name": "bad"}}),
    ("listCollections v1", {"listCollections": 1, "filter": {"name": "v1"}}),
    ("find", {"find": "v1"}),
    (
        "find filter sort",
        {"find": "v1", "filter": {"v": {"$gt": 10}}, "sort": {"v": -1}, "limit": 2},
    ),
    ("find through two views", {"find": "v4"}),
    ("find missing source", {"find": "v3"}),
    ("find $natural -1", {"find": "v1", "sort": {"$natural": -1}}),
    ("find $natural 1", {"find": "v1", "sort": {"$natural": 1}}),
    ("find hint $natural", {"find": "v1", "hint": {"$natural": 1}}),
    ("find tailable", {"find": "v1", "tailable": True}),
    ("find other collation", {"find": "v1", "collation": {"locale": "fr"}}),
    ("count", {"count": "v1"}),
    ("count query", {"count": "v4", "query": {"g": 1}}),
    ("distinct", {"distinct": "v1", "key": "v"}),
    ("distinct query", {"distinct": "v1", "key": "v", "query": {"v": {"$gt": 10}}}),
    ("distinct through two views", {"distinct": "v4", "key": "g"}),
    ("aggregate", {"aggregate": "v1", "pipeline": [{"$count": "n"}], "cursor": {}}),
    (
        "$lookup from",
        {"aggregate": "other", "pipeline": [{"$lookup": LOOKUP}, *SIZE_OF_M], "cursor": {}},
    ),
    (
        "$graphLookup from",
        {"aggregate": "other", "pipeline": [{"$graphLookup": GRAPH}, *SIZE_OF_M], "cursor": {}},
    ),
    (
        "$unionWith",
        {"aggregate": "other", "pipeline": [{"$unionWith": "v1"}, {"$count": "n"}], "cursor": {}},
    ),
    ("$out into", {"aggregate": "other", "pipeline": [{"$out": "v1"}], "cursor": {}}),
    ("$merge into", {"aggregate": "other", "pipeline": [{"$merge": {"into": "v1"}}], "cursor": {}}),
    ("$collStats", {"aggregate": "v1", "pipeline": [{"$collStats": {"count": {}}}], "cursor": {}}),
    ("$changeStream", {"aggregate": "v1", "pipeline": [{"$changeStream": {}}], "cursor": {}}),
    ("insert ordered", {"insert": "v1", "documents": [{"_id": 100}, {"_id": 101}]}),
    (
        "insert unordered",
        {"insert": "v1", "documents": [{"_id": 100}, {"_id": 101}], "ordered": False},
    ),
    ("update", {"update": "v1", "updates": [{"q": {}, "u": SET_A}, {"q": {}, "u": SET_A}]}),
    (
        "delete unordered",
        {
            "delete": "v1",
            "ordered": False,
            "deletes": [{"q": {}, "limit": 1}, {"q": {}, "limit": 0}],
        },
    ),
    ("findAndModify", {"findAndModify": "v1", "query": {}, "remove": True}),
    ("createIndexes", {"createIndexes": "v1", "indexes": [{"key": {"v": 1}, "name": "v_1"}]}),
    ("listIndexes", {"listIndexes": "v1"}),
    ("dropIndexes", {"dropIndexes": "v1", "index": "*"}),
    ("collStats", {"collStats": "v1"}),
    ("validate", {"validate": "v1"}),
    ("admin: rename a view", {"renameCollection": f"{DB}.v2", "to": f"{DB}.v2b"}),
    ("admin: rename onto a view", {"renameCollection": f"{DB}.other", "to": f"{DB}.v1"}),
    ("find source after the writes", {"find": "src", "projection": {"_id": 1}}),
    ("collMod pipeline", {"collMod": "v1", "pipeline": [{"$match": {"g": 2}}]}),
    ("listCollections after pipeline", {"listCollections": 1, "filter": {"name": "v1"}}),
    ("collMod viewOn", {"collMod": "v1", "viewOn": "other"}),
    ("listCollections after viewOn", {"listCollections": 1, "filter": {"name": "v1"}}),
    ("collMod both", {"collMod": "v1", "viewOn": "src", "pipeline": MATCH_G1}),
    ("find after collMod", {"find": "v1", "projection": {"_id": 1}}),
    ("collMod viewOn on a collection", {"collMod": "src", "viewOn": "other"}),
    ("collMod pipeline on a collection", {"collMod": "src", "viewOn": "other", "pipeline": []}),
    ("collMod validator", {"collMod": "v1", "validator": {"a": 1}}),
    ("collMod validationLevel", {"collMod": "v1", "validationLevel": "off"}),
    ("collMod index", {"collMod": "v1", "index": {"name": "x", "hidden": True}}),
    ("collMod cycle", {"collMod": "v1", "viewOn": "v4", "pipeline": []}),
    ("collMod unknown stage", {"collMod": "v1", "viewOn": "src", "pipeline": [{"$nope": 1}]}),
    ("collMod $out", {"collMod": "v1", "viewOn": "src", "pipeline": [{"$out": "x"}]}),
    ("collMod pipeline not an array", {"collMod": "v1", "viewOn": "src", "pipeline": {"a": 1}}),
    ("collMod viewOn empty", {"collMod": "v1", "viewOn": "", "pipeline": []}),
    ("drop", {"drop": "v4"}),
    ("drop again", {"drop": "v4"}),
    ("find system.views", {"find": "system.views", "sort": {"_id": 1}}),
    ("listCollections system.views", {"listCollections": 1, "filter": {"name": "system.views"}}),
]


def typed(v):
    if isinstance(v, bool) or v is None or isinstance(v, str):
        return v
    if isinstance(v, Int64):
        return f"long:{int(v)}"
    if isinstance(v, int):
        return f"int:{v}"
    if isinstance(v, float):
        return f"double:{v}"
    if isinstance(v, dict):
        return {k: typed(x) for k, x in v.items() if k not in NOISE}
    if isinstance(v, (list, tuple)):
        return [typed(x) for x in v]
    return repr(v)


def run(client):
    client.drop_database(DB)
    db = client[DB]
    db.src.insert_many([{"_id": i, "g": i % 3, "v": i * 10} for i in range(10)])
    db.other.insert_many([{"_id": i, "g": i} for i in range(3)])
    results = {}
    for label, command in SEQUENCE:
        target = client.admin if label.startswith("admin:") else db
        try:
            reply = typed(dict(target.command(command)))
            if "values" in reply:
                reply["values"] = sorted(reply["values"])
            results[label] = ("OK", reply)
        except PyMongoError as exc:
            details = getattr(exc, "details", None) or {}
            results[label] = ("ERR", details.get("code"), (details.get("errmsg") or str(exc))[:240])
    return results


def main():
    with probe_targets(replica_set="secantus") as (mon, targets):
        divergent = {label: 0 for label, _ in targets}
        expected = run(mon)
        got = {name: run(cli) for name, cli in targets}
        for label, _ in SEQUENCE:
            off = {name for name, g in got.items() if g[label] != expected[label]}
            if not off:
                continue
            for name in off:
                divergent[name] += 1
            print(f"  {label}")
            print(f"    mongod  : {expected[label]}")
            for name, g in got.items():
                mark = "   <-- diverges" if name in off else ""
                print(f"    {name:8s}: {g[label]}{mark}")
        return report("views", len(SEQUENCE), divergent)


if __name__ == "__main__":
    sys.exit(main())
