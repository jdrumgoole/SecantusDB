"""Differential-probe document validation against a real mongod.

One stateful sequence: `create` / `collMod` with `validator`,
`validationLevel` and `validationAction`, then inserts, updates, upserts,
`findAndModify`, `bypassDocumentValidation`, and what `listCollections`
reports. Error 121's `errInfo` is compared whole.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/validators.py
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _servers import probe_targets, report  # noqa: E402
from bson import Int64  # noqa: E402
from pymongo.errors import PyMongoError  # noqa: E402

NOISE = ("$clusterTime", "operationTime", "opTime", "electionId", "uuid", "idIndex")
DB = "validators_probe"
SCHEMA = {
    "$jsonSchema": {
        "bsonType": "object",
        "required": ["name", "age"],
        "properties": {
            "name": {"bsonType": "string", "minLength": 2},
            "age": {"bsonType": "int", "minimum": 0, "maximum": 150},
        },
    }
}
QUERY = {"qty": {"$gte": 0}, "kind": {"$in": ["a", "b"]}}


def ins(coll, doc, **extra):
    return {"insert": coll, "documents": [doc], **extra}


def upd(coll, q, u, **extra):
    return {"update": coll, "updates": [{"q": q, "u": u, **extra}]}


#: (label, command)
SEQUENCE = [
    ("create schema", {"create": "s", "validator": SCHEMA}),
    ("create query", {"create": "q", "validator": QUERY}),
    ("create warn", {"create": "w", "validator": QUERY, "validationAction": "warn"}),
    ("create moderate", {"create": "m", "validator": QUERY, "validationLevel": "moderate"}),
    ("create off", {"create": "o", "validator": QUERY, "validationLevel": "off"}),
    ("create bad level", {"create": "x1", "validator": QUERY, "validationLevel": "sometimes"}),
    ("create bad action", {"create": "x2", "validator": QUERY, "validationAction": "shout"}),
    ("create level no validator", {"create": "x3", "validationLevel": "moderate"}),
    ("create action errorAndLog", {"create": "x4", "validator": QUERY, "validationAction": "errorAndLog"}),
    ("create validator not object", {"create": "x5", "validator": 5}),
    ("create validator $where", {"create": "x6", "validator": {"$where": "true"}}),
    ("create validator $text", {"create": "x7", "validator": {"$text": {"$search": "a"}}}),
    ("create validator $near", {"create": "x8", "validator": {"loc": {"$near": [0, 0]}}}),
    ("create validator $expr", {"create": "x9", "validator": {"$expr": {"$gt": ["$a", "$b"]}}}),
    ("create validator empty", {"create": "x10", "validator": {}}),
    ("listCollections", {"listCollections": 1, "filter": {"name": {"$in": ["s", "q", "w", "m", "o", "x3", "x10"]}}}),
    ("schema ok", ins("s", {"_id": 1, "name": "ab", "age": 3})),
    ("schema missing required", ins("s", {"_id": 2, "name": "ab"})),
    ("schema wrong type", ins("s", {"_id": 3, "name": "ab", "age": "x"})),
    ("schema long age", ins("s", {"_id": 4, "name": "ab", "age": Int64(3)})),
    ("schema too short", ins("s", {"_id": 5, "name": "a", "age": 3})),
    ("schema over maximum", ins("s", {"_id": 6, "name": "ab", "age": 200})),
    ("schema two failures", ins("s", {"_id": 7, "name": 1, "age": -1})),
    ("query ok", ins("q", {"_id": 1, "qty": 1, "kind": "a"})),
    ("query fail one", ins("q", {"_id": 2, "qty": -1, "kind": "a"})),
    ("query fail both", ins("q", {"_id": 3, "qty": -1, "kind": "z"})),
    ("query missing field", ins("q", {"_id": 4, "kind": "a"})),
    ("expr fail", ins("x9", {"_id": 1, "a": 1, "b": 2})),
    ("expr ok", ins("x9", {"_id": 2, "a": 3, "b": 2})),
    (
        "batch ordered",
        {"insert": "q", "documents": [{"_id": 10, "qty": 1, "kind": "a"}, {"_id": 11, "qty": -1, "kind": "a"}, {"_id": 12, "qty": 1, "kind": "b"}]},
    ),
    (
        "batch unordered",
        {"insert": "q", "ordered": False, "documents": [{"_id": 20, "qty": 1, "kind": "a"}, {"_id": 21, "qty": -1, "kind": "a"}, {"_id": 22, "qty": 1, "kind": "b"}]},
    ),
    ("find q", {"find": "q", "projection": {"_id": 1}, "sort": {"_id": 1}}),
    ("bypass insert", ins("q", {"_id": 30, "qty": -5, "kind": "z"}, bypassDocumentValidation=True)),
    ("bypass false", ins("q", {"_id": 31, "qty": -5, "kind": "z"}, bypassDocumentValidation=False)),
    ("update to invalid", upd("q", {"_id": 1}, {"$set": {"qty": -1}})),
    ("update stays valid", upd("q", {"_id": 1}, {"$set": {"qty": 5}})),
    ("update invalid doc to invalid", upd("q", {"_id": 30}, {"$set": {"qty": -6}})),
    ("update invalid doc to valid", upd("q", {"_id": 30}, {"$set": {"qty": 6, "kind": "a"}})),
    ("update multi partly invalid", {"update": "q", "updates": [{"q": {}, "u": {"$inc": {"qty": -6}}, "multi": True}]}),
    ("find q after multi", {"find": "q", "projection": {"qty": 1}, "sort": {"_id": 1}}),
    ("replace invalid", upd("q", {"_id": 1}, {"qty": -1, "kind": "a"})),
    ("upsert invalid", upd("q", {"_id": 40}, {"$set": {"qty": -1, "kind": "a"}}, upsert=True)),
    ("upsert valid", upd("q", {"_id": 41}, {"$set": {"qty": 1, "kind": "a"}}, upsert=True)),
    ("bypass update", {**upd("q", {"_id": 41}, {"$set": {"qty": -9}}), "bypassDocumentValidation": True}),
    ("findAndModify invalid", {"findAndModify": "q", "query": {"_id": 10}, "update": {"$set": {"qty": -1}}}),
    ("findAndModify bypass", {"findAndModify": "q", "query": {"_id": 10}, "update": {"$set": {"qty": -1}}, "bypassDocumentValidation": True}),
    ("findAndModify upsert invalid", {"findAndModify": "q", "query": {"_id": 50}, "update": {"$set": {"qty": -1}}, "upsert": True}),
    ("warn insert invalid", ins("w", {"_id": 1, "qty": -1, "kind": "z"})),
    ("find w", {"find": "w"}),
    ("off insert invalid", ins("o", {"_id": 1, "qty": -1, "kind": "z"})),
    ("moderate insert invalid", ins("m", {"_id": 1, "qty": -1, "kind": "z"})),
    ("moderate bypass seed", ins("m", {"_id": 2, "qty": -1, "kind": "z"}, bypassDocumentValidation=True)),
    ("moderate update invalid existing", upd("m", {"_id": 2}, {"$set": {"qty": -2}})),
    ("moderate valid seed", ins("m", {"_id": 3, "qty": 1, "kind": "a"})),
    ("moderate update valid to invalid", upd("m", {"_id": 3}, {"$set": {"qty": -2}})),
    ("collMod add validator", {"collMod": "o", "validator": {"z": {"$exists": True}}, "validationLevel": "strict"}),
    ("after collMod insert invalid", ins("o", {"_id": 2})),
    ("after collMod old invalid doc update", upd("o", {"_id": 1}, {"$set": {"y": 1}})),
    ("collMod level only", {"collMod": "o", "validationLevel": "off"}),
    ("collMod action only", {"collMod": "q", "validationAction": "warn"}),
    ("collMod bad level", {"collMod": "q", "validationLevel": "nope"}),
    ("collMod remove validator", {"collMod": "s", "validator": {}}),
    ("after removal insert", ins("s", {"_id": 99})),
    ("collMod validator not object", {"collMod": "q", "validator": "x"}),
    ("listCollections after", {"listCollections": 1, "filter": {"name": {"$in": ["s", "q", "o"]}}}),
    ("aggregate $out to validated", {"aggregate": "w", "pipeline": [{"$out": "m"}], "cursor": {}}),
    ("aggregate $merge to validated", {"aggregate": "w", "pipeline": [{"$merge": {"into": "m"}}], "cursor": {}}),
    ("find m", {"find": "m", "sort": {"_id": 1}}),
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
    results = {}
    for label, command in SEQUENCE:
        try:
            reply = typed(dict(db.command(command)))
            if label.startswith("listCollections"):
                reply = sorted(reply["cursor"]["firstBatch"], key=lambda d: d["name"])
            results[label] = ("OK", reply)
        except PyMongoError as exc:
            details = typed(dict(getattr(exc, "details", None) or {"errmsg": str(exc)[:200]}))
            details.pop("ok", None)
            results[label] = ("ERR", details)
    return results


def main():
    with probe_targets() as (mon, targets):
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
        return report("validators", len(SEQUENCE), divergent)


if __name__ == "__main__":
    sys.exit(main())
