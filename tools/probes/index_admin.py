"""Differential-probe index management against a real mongod.

One stateful sequence of `createIndexes`, `listIndexes`, `dropIndexes` and
`collMod index` commands: reply shapes, option handling, and the errors for
conflicting, duplicate and malformed index specs.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/index_admin.py
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _servers import probe_targets, report  # noqa: E402
from bson import Int64  # noqa: E402
from pymongo.errors import PyMongoError  # noqa: E402

NOISE = ("$clusterTime", "operationTime", "opTime", "electionId", "uuid")
DB = "index_admin_probe"


def ci(coll, *specs, **extra):
    return {"createIndexes": coll, "indexes": list(specs), **extra}


def ix(key, name=None, **options):
    spec = {"key": key, **options}
    spec["name"] = name or "_".join(f"{k}_{v}" for k, v in key.items())
    return spec


SEED = {
    "insert": "c",
    "documents": [{"_id": i, "a": i, "b": i % 2, "t": "x", "arr": [i, i + 1]} for i in range(4)],
}

#: (label, command)
SEQUENCE = [
    ("seed", SEED),
    ("create one", ci("c", ix({"a": 1}))),
    ("create same again", ci("c", ix({"a": 1}))),
    ("create same key other name", ci("c", ix({"a": 1}, "other"))),
    ("create same name other key", ci("c", ix({"b": 1}, "a_1"))),
    ("create same key other options", ci("c", ix({"a": 1}, unique=True))),
    ("create two", ci("c", ix({"b": 1}), ix({"a": 1, "b": -1}))),
    ("create one new one existing", ci("c", ix({"b": 1}), ix({"t": 1}))),
    ("create on missing collection", ci("fresh", ix({"a": 1}))),
    ("create empty indexes", {"createIndexes": "c", "indexes": []}),
    ("create indexes not array", {"createIndexes": "c", "indexes": {"key": {"a": 1}}}),
    ("create no key", ci("c", {"name": "x"})),
    ("create no name", ci("c", {"key": {"z": 1}})),
    ("create empty key", ci("c", {"key": {}, "name": "e"})),
    ("create key value 0", ci("c", ix({"z": 0}))),
    ("create key value 2", ci("c", ix({"z2": 2}))),
    ("create key value -5", ci("c", ix({"z3": -5}))),
    ("create key value 1.5", ci("c", ix({"z4": 1.5}))),
    ("create key value string bad", ci("c", ix({"z5": "nope"}))),
    ("create key value true", ci("c", ix({"z6": True}))),
    ("create key empty field", ci("c", {"key": {"": 1}, "name": "ef"})),
    ("create key $field", ci("c", {"key": {"$x": 1}, "name": "dx"})),
    ("create key dotted", ci("c", ix({"p.q": 1}))),
    ("create _id index", ci("c", ix({"_id": 1}, "_id_"))),
    ("create _id index other name", ci("c", ix({"_id": 1}, "myid"))),
    ("create _id desc", ci("c", ix({"_id": -1}))),
    ("create _id unique false", ci("c", {"key": {"_id": 1}, "name": "_id_", "unique": False})),
    ("create _id sparse", ci("c", {"key": {"_id": 1}, "name": "_id_", "sparse": True})),
    ("create unknown option", ci("c", ix({"u": 1}, bogus=True))),
    ("create unique on duplicates", ci("c", ix({"b": 1}, "b_unique", unique=True))),
    ("create unique ok", ci("c", ix({"a": 1, "t": 1}, unique=True))),
    ("create sparse", ci("c", ix({"s": 1}, sparse=True))),
    ("create sparse not bool", ci("c", ix({"s2": 1}, sparse="yes"))),
    ("create unique number", ci("c", ix({"s3": 1}, unique=1))),
    ("create hidden", ci("c", ix({"h": 1}, hidden=True))),
    ("create hidden _id", ci("c", {"key": {"_id": 1}, "name": "_id_", "hidden": True})),
    ("create ttl", ci("c", ix({"when": 1}, expireAfterSeconds=3600))),
    ("create ttl negative", ci("c", ix({"when2": 1}, expireAfterSeconds=-1))),
    ("create ttl string", ci("c", ix({"when3": 1}, expireAfterSeconds="x"))),
    ("create ttl compound", ci("c", ix({"when4": 1, "a": 1}, expireAfterSeconds=10))),
    ("create ttl huge", ci("c", ix({"when5": 1}, expireAfterSeconds=Int64(2**40)))),
    ("create partial", ci("c", ix({"pf": 1}, partialFilterExpression={"a": {"$gt": 1}}))),
    ("create partial bad op", ci("c", ix({"pf2": 1}, partialFilterExpression={"a": {"$ne": 1}}))),
    (
        "create partial and sparse",
        ci("c", ix({"pf3": 1}, sparse=True, partialFilterExpression={"a": 1})),
    ),
    ("create partial not object", ci("c", ix({"pf4": 1}, partialFilterExpression=5))),
    ("create text", ci("c", ix({"t": "text"}))),
    ("create hashed", ci("c", ix({"a": "hashed"}))),
    ("create wildcard", ci("c", ix({"$**": 1}))),
    ("create 2dsphere", ci("c", ix({"loc": "2dsphere"}))),
    ("create collation", ci("c", ix({"cl": 1}, collation={"locale": "en", "strength": 2}))),
    ("create collation bad locale", ci("c", ix({"cl2": 1}, collation={"locale": "zz_nope"}))),
    ("create v 1", ci("c", ix({"v1": 1}, v=1))),
    ("create v 3", ci("c", ix({"v3": 1}, v=3))),
    ("create background", ci("c", ix({"bg": 1}, background=True))),
    ("create commitQuorum", ci("c", ix({"cq": 1}), commitQuorum="majority")),
    ("create name empty", ci("c", {"key": {"ne": 1}, "name": ""})),
    ("create name not string", ci("c", {"key": {"ns": 1}, "name": 5})),
    ("create name star", ci("c", {"key": {"st": 1}, "name": "*"})),
    ("create 32 fields", ci("c", {"key": {f"f{i}": 1 for i in range(32)}, "name": "wide32"})),
    ("create 33 fields", ci("c", {"key": {f"g{i}": 1 for i in range(33)}, "name": "wide33"})),
    ("listIndexes", {"listIndexes": "c"}),
    ("listIndexes missing", {"listIndexes": "nosuch"}),
    ("listIndexes batchSize", {"listIndexes": "c", "cursor": {"batchSize": 2}}),
    ("collMod hide", {"collMod": "c", "index": {"name": "a_1", "hidden": True}}),
    ("collMod hide again", {"collMod": "c", "index": {"name": "a_1", "hidden": True}}),
    ("collMod unhide by key", {"collMod": "c", "index": {"keyPattern": {"a": 1}, "hidden": False}}),
    ("collMod hide _id", {"collMod": "c", "index": {"name": "_id_", "hidden": True}}),
    ("collMod ttl", {"collMod": "c", "index": {"name": "when_1", "expireAfterSeconds": 60}}),
    (
        "collMod ttl on non-ttl",
        {"collMod": "c", "index": {"name": "a_1", "expireAfterSeconds": 60}},
    ),
    ("collMod missing index", {"collMod": "c", "index": {"name": "nope", "hidden": True}}),
    ("collMod index no name", {"collMod": "c", "index": {"hidden": True}}),
    ("collMod index nothing to do", {"collMod": "c", "index": {"name": "a_1"}}),
    ("drop by name", {"dropIndexes": "c", "index": "s_1"}),
    ("drop by name again", {"dropIndexes": "c", "index": "s_1"}),
    ("drop by key", {"dropIndexes": "c", "index": {"h": 1}}),
    ("drop by key missing", {"dropIndexes": "c", "index": {"nokey": 1}}),
    ("drop _id by name", {"dropIndexes": "c", "index": "_id_"}),
    ("drop _id by key", {"dropIndexes": "c", "index": {"_id": 1}}),
    ("drop list", {"dropIndexes": "c", "index": ["b_1", "t_1"]}),
    ("drop list one missing", {"dropIndexes": "c", "index": ["a_1", "nope"]}),
    ("drop list with _id", {"dropIndexes": "c", "index": ["a_1", "_id_"]}),
    ("drop number", {"dropIndexes": "c", "index": 5}),
    ("drop no index field", {"dropIndexes": "c"}),
    ("drop on missing collection", {"dropIndexes": "nosuch", "index": "*"}),
    ("drop star", {"dropIndexes": "c", "index": "*"}),
    ("listIndexes after", {"listIndexes": "c"}),
    ("drop star again", {"dropIndexes": "c", "index": "*"}),
]


UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def typed(v):
    if isinstance(v, str):
        # An index build's id and the collection's are random per run.
        return UUID.sub("<uuid>", v)
    if isinstance(v, bool) or v is None:
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
            if label.startswith("listIndexes") and "cursor" in reply:
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
        return report("index_admin", len(SEQUENCE), divergent)


if __name__ == "__main__":
    sys.exit(main())
