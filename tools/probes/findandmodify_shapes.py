"""Differential-probe findAndModify reply shapes against a real mongod.

findAndModify's reply carries a `lastErrorObject` whose contents vary by case
(updated vs upserted vs deleted vs no-match), plus `value`. Those shapes are what
drivers key on, and nothing in the gauges compares them field for field.
"""

import os
import tempfile

import pymongo

from secantus import SecantusDBServer

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")

CASES = [
    # (label, seed docs, command-extras)
    ("update match", [{"_id": 1, "a": 1}], {"query": {"_id": 1}, "update": {"$set": {"a": 2}}}),
    ("update no match", [{"_id": 1, "a": 1}], {"query": {"_id": 9}, "update": {"$set": {"a": 2}}}),
    (
        "update new=True",
        [{"_id": 1, "a": 1}],
        {"query": {"_id": 1}, "update": {"$set": {"a": 2}}, "new": True},
    ),
    ("upsert creates", [], {"query": {"_id": 5}, "update": {"$set": {"a": 1}}, "upsert": True}),
    (
        "upsert creates new",
        [],
        {"query": {"_id": 5}, "update": {"$set": {"a": 1}}, "upsert": True, "new": True},
    ),
    (
        "upsert matches",
        [{"_id": 5, "a": 0}],
        {"query": {"_id": 5}, "update": {"$set": {"a": 1}}, "upsert": True},
    ),
    ("remove match", [{"_id": 1, "a": 1}], {"query": {"_id": 1}, "remove": True}),
    ("remove no match", [{"_id": 1, "a": 1}], {"query": {"_id": 9}, "remove": True}),
    (
        "sort picks first",
        [{"_id": 1, "a": 3}, {"_id": 2, "a": 1}],
        {"query": {}, "sort": {"a": 1}, "update": {"$set": {"z": 1}}},
    ),
    (
        "fields projection",
        [{"_id": 1, "a": 1, "b": 2}],
        {"query": {"_id": 1}, "fields": {"a": 1}, "update": {"$set": {"a": 9}}},
    ),
    ("replacement doc", [{"_id": 1, "a": 1}], {"query": {"_id": 1}, "update": {"b": 7}}),
    (
        "pipeline update",
        [{"_id": 1, "a": 1}],
        {"query": {"_id": 1}, "update": [{"$set": {"a": 5}}]},
    ),
    (
        "arrayFilters",
        [{"_id": 1, "a": [1, 2, 3]}],
        {
            "query": {"_id": 1},
            "update": {"$set": {"a.$[e]": 9}},
            "arrayFilters": [{"e": {"$gt": 1}}],
        },
    ),
    # error shapes
    (
        "remove+update",
        [{"_id": 1}],
        {"query": {"_id": 1}, "remove": True, "update": {"$set": {"a": 1}}},
    ),
    ("neither", [{"_id": 1}], {"query": {"_id": 1}}),
    ("remove+new", [{"_id": 1}], {"query": {"_id": 1}, "remove": True, "new": True}),
    ("remove+upsert", [{"_id": 1}], {"query": {"_id": 1}, "remove": True, "upsert": True}),
    ("bad update type", [{"_id": 1}], {"query": {"_id": 1}, "update": 5}),
]


def run(cli, dbname, seed, extras):
    db = cli[dbname]
    db.c.drop()
    if seed:
        db.c.insert_many([dict(d) for d in seed])
    else:
        db.create_collection("c")
    cmd = {"findAndModify": "c"}
    cmd.update(extras)
    try:
        r = db.command(cmd)
        leo = r.get("lastErrorObject")
        return (
            "OK",
            repr(r.get("value")),
            repr(leo),
            sorted(k for k in r if k not in ("$clusterTime", "operationTime", "ok")),
        )
    except Exception as e:
        return ("ERR", getattr(e, "code", None), str(e).split(", full error")[0][:64])


def main():
    d = tempfile.mkdtemp()
    s = SecantusDBServer(port=0, storage_path=d, replica_set_name="secantus")
    s.start()
    sec = pymongo.MongoClient(f"mongodb://{s.address[0]}:{s.address[1]}", directConnection=True)
    mon = pymongo.MongoClient(MONGOD, directConnection=True, serverSelectionTimeoutMS=8000)
    diffs = []
    for i, (label, seed, extras) in enumerate(CASES):
        a = run(mon, f"fam{i}", seed, extras)
        b = run(sec, f"fam{i}", seed, extras)
        if a != b:
            diffs.append((label, a, b))
    print(f"  cases: {len(CASES)}   divergences: {len(diffs)}\n")
    for label, mongo, ours in diffs:
        print(f"  {label}")
        print(f"    mongod  : {mongo}")
        print(f"    secantus: {ours}\n")
    sec.close()
    s.stop()


main()
