"""Differential-probe mongod's Boolean simplifier over a `find`'s filter.

Runs seeded random `$and` / `$or` (/ `$nor`) trees over nine predicates
through `explain` on mongod and on `secantusd-mdb` and prints the ones whose
`parsedQuery` differs (RM-774, RM-778).

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 \
    PROBE_N=2000 PROBE_SEED=1 PROBE_KINDS='$and,$or,$or,$and' \
        python tools/probes/boolean_simplifier.py

Add `$nor` to `PROBE_KINDS` for nested `$nor` (about 1 tree in 140 differs).
Run it on a mongod whose `internalQueryEnableBooleanExpressionsSimplifier` is
true (the default); it sets it so.
"""

import os
import random

import pymongo
from bson import json_util

LEAVES = [
    {"a": 1},
    {"b": 1},
    {"c": 1},
    {"a": 2},
    {"d": {"$gt": 1}},
    {"a": {"$ne": 1}},
    {"b": {"$ne": 1}},
    {"c": {"$ne": 1}},
    {"d": {"$not": {"$gt": 1}}},
]


def tree(rng, depth, kinds):
    if depth == 0 or rng.random() < 0.3:
        return dict(rng.choice(LEAVES))
    kind = rng.choice(kinds)
    return {kind: [tree(rng, depth - 1, kinds) for _ in range(rng.choice([2, 2, 3]))]}


def parsed(client, filt):
    try:
        reply = client.p.command("explain", {"find": "c", "filter": filt})
        return reply["queryPlanner"]["parsedQuery"]
    except pymongo.errors.PyMongoError as err:
        return f"ERR {str(err)[:60]}"


def main():
    mongod = pymongo.MongoClient(os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27017"))
    server = pymongo.MongoClient(os.environ["PROBE_SERVER"])
    mongod.admin.command(
        "setParameter",
        1,
        internalQueryEnableBooleanExpressionsSimplifier=True,
        internalQueryDoNotOpenContainedOrsInSimplifier=True,
    )
    for client in (mongod, server):
        client.p.c.delete_many({})
        client.p.c.insert_one({"a": 1})
    rng = random.Random(int(os.environ.get("PROBE_SEED", "1")))
    kinds = os.environ.get("PROBE_KINDS", "$and,$or,$or,$and").split(",")
    n = int(os.environ.get("PROBE_N", "2000"))
    shown = int(os.environ.get("PROBE_SHOW", "5"))
    bad = 0
    for _ in range(n):
        filt = tree(rng, rng.choice([2, 3, 3, 4]), kinds)
        ours, theirs = parsed(server, filt), parsed(mongod, filt)
        if ours != theirs:
            bad += 1
            if bad <= shown:
                print(json_util.dumps(filt))
                print("  server:", json_util.dumps(ours))
                print("  mongod:", json_util.dumps(theirs))
    print(f"=== boolean simplifier: {n} trees -- {bad} differ ===")
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
