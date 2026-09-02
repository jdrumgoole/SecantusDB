"""Differential-probe findAndModify reply shapes against a real mongod.

findAndModify's reply carries a `lastErrorObject` whose contents vary by case
(updated vs upserted vs deleted vs no-match), plus `value`. Those shapes are what
drivers key on, and nothing in the gauges compares them field for field.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

from _servers import probe_targets, report  # noqa: E402

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
    with probe_targets(replica_set="secantus") as (mon, targets):
        divergent = {label: 0 for label, _ in targets}
        for i, (label, seed, extras) in enumerate(CASES):
            expected = run(mon, f"fam{i}", seed, extras)
            got = {name: run(cli, f"fam{i}", seed, extras) for name, cli in targets}
            off = {k for k, v in got.items() if v != expected}
            if not off:
                continue
            for name in off:
                divergent[name] += 1
            print(f"  {label}")
            print(f"    mongod  : {expected}")
            for name, value in got.items():
                mark = "   <-- diverges" if name in off else ""
                print(f"    {name:8s}: {value}{mark}")
            print()
        return report("findAndModify reply shapes", len(CASES), divergent)


sys.exit(main())
