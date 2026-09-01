"""Sweep ``explain``'s ``queryPlanner`` against mongod: parsedQuery + stage tree.

Two questions, both of which had never been measured before 2026-09-01:

* does ``parsedQuery`` come back in mongod's NORMALISED form (bare equality
  wrapped in ``$eq``, several fields folded into a sorted ``$and``, ``$ne``
  rewritten as ``$not``/``$eq``, ``$type`` reduced to numeric codes)?
* does ``winningPlan`` carry the STAGE TREE that describes the query -- ``SORT``
  / ``SKIP`` / ``LIMIT`` / ``PROJECTION_*`` above the scan -- or a single flat
  node?

The child ORDER inside ``$and`` is the part no documentation covers: mongod
sorts by an internal ``MatchExpression`` type ordinal. ``PROBE_ORDER=1`` runs
the pairwise derivation that produced ``secantus.explain.MATCH_TYPE_RANK``.

Run it like the other probes; ``PROBE_SERVER`` points at an already-running
server (this is how the Rust server is swept), otherwise an embedded Python one
is started.
"""

import itertools
import os
import sys
import tempfile

import pymongo
from bson import SON, json_util

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")

#: Filters, each swept through ``parsedQuery``. Ordered dicts throughout: half
#: the point is that mongod REORDERS the clauses it is given.
FILTERS = [
    {},
    {"a": 1},
    {"a": None},
    {"a": {"$eq": 5}},
    {"a": {"$ne": 3}},
    {"a": {"$nin": [1, 2]}},
    {"a": {"$in": [1, 2]}},
    {"a": {"$in": [1]}},
    {"a": {"$in": []}},
    {"a": {"$all": [1, 2]}},
    {"a": {"$all": [1]}},
    {"a": {"$all": [{"$elemMatch": {"x": 1}}]}},
    {"a": {"$exists": True}},
    {"a": {"$size": 2}},
    {"a": {"$mod": [2, 0]}},
    {"a": {"$type": "int"}},
    {"a": {"$type": 2}},
    {"a": {"$type": ["int", "string"]}},
    {"a": {"$type": ["string", "int"]}},
    {"a": {"$regex": "^x"}},
    {"a": {"$regex": "^x", "$options": "i"}},
    {"a": {"$not": {"$gt": 1}}},
    {"a": {"$not": {"$in": [1, 2]}}},
    {"a": {"$not": {"$regex": "^x"}}},
    {"a": {"$elemMatch": {"x": 1}}},
    {"a": {"$elemMatch": SON([("x", 1), ("y", {"$gt": 2})])}},
    {"a": {"$elemMatch": {"$gt": 1}}},
    SON([("a", {"$gt": 3}), ("a2", {"$lt": 9})]),
    {"a": SON([("$gt", 3), ("$lt", 9)])},
    {"a": SON([("$lt", 9), ("$gt", 3)])},
    {"a": SON([("$gte", 3), ("$lte", 9)])},
    {"a": SON([("$ne", 3), ("$gt", 1)])},
    {"a": SON([("$gt", 1), ("$lt", 9), ("$ne", 5)])},
    SON([("b", 2), ("a", 1)]),
    SON([("a", 1), ("b", 2), ("c", 3)]),
    SON([("a", {"$gt": 1}), ("b", 2)]),
    SON([("x.y", 1), ("a", 2)]),
    {"$and": [{"a": 1}, {"b": 2}]},
    {"$and": [{"a": 1}]},
    {"$and": [{"$and": [{"a": 1}]}, {"b": 2}]},
    {"$or": [{"a": 1}, {"b": 2}]},
    {"$or": [{"a": 1}]},
    {"$or": [SON([("a", 1), ("b", 2)])]},
    {"$nor": [{"a": 1}]},
    {"$nor": [{"a": 1}, {"b": 2}]},
    SON([("$or", [{"b": 1}, {"c": 2}]), ("a", 1)]),
    SON([("$or", [{"b": 1}, {"c": 2}]), ("a", {"$elemMatch": {"z": 1}})]),
    SON([("$or", [{"b": 1}, {"c": 2}]), ("a", {"$size": 2})]),
    SON([("$or", [{"b": 1}, {"c": 2}]), ("a", {"$type": "int"})]),
    SON([("$or", [{"b": 1}, {"c": 2}]), ("a", {"$ne": 1})]),
    SON([("$and", [{"$or": [{"b": 1}, {"c": 2}]}, {"$or": [{"d": 1}, {"e": 2}]}])]),
    {"$and": [{"a": 1}, {"a": 2}]},
    {"$and": [{"z": 1}, {"a": 1}]},
    SON([("a", 1), ("$comment", "hi")]),
    {"a": {"$bitsAllSet": 1}},
    {"sub": {"k": 1}},
]

#: ``find`` shapes swept through ``winningPlan``'s stage chain.
PLANS = [
    {},
    {"filter": {"nope": 1}},
    {"filter": {}, "hint": {"$natural": -1}},
    {"filter": {"a": 7}, "hint": "a_1"},
    {"filter": {"b": "7"}, "hint": "b_uniq"},
    {"filter": {"s": 7}, "hint": "s_sparse"},
    {"filter": {"a": {"$gt": 5}}, "hint": "a_part"},
    {"filter": {"nope": 1}, "sort": {"a": -1}},
    {"filter": {"nope": 1}, "sort": {"zzz": 1}},
    {"filter": {"nope": 1}, "limit": 3},
    {"filter": {"nope": 1}, "skip": 3},
    {"filter": {"nope": 1}, "limit": 3, "skip": 2},
    {"filter": {"nope": 1}, "projection": {"a": 1}},
    {"filter": {"nope": 1}, "projection": {"a": 0}},
    {"filter": {}, "projection": {"a.b": 1}},
    {"filter": {}, "projection": {"a": {"$elemMatch": {"x": 1}}}},
    {"filter": {}, "projection": {"_id": 0, "a": 1}},
    {"filter": {"nope": 1}, "projection": {"a": 1}, "limit": 3},
    {"filter": {"nope": 1}, "projection": {"a": 1}, "limit": 3, "skip": 2},
    {"filter": {"nope": 1}, "sort": {"zzz": 1}, "limit": 3},
    {"filter": {"nope": 1}, "sort": {"zzz": 1}, "skip": 3},
    {"filter": {"nope": 1}, "sort": {"zzz": 1}, "projection": {"a": 1}},
    {
        "filter": {"nope": 1},
        "sort": {"zzz": 1},
        "projection": {"a": 1},
        "limit": 3,
        "skip": 2,
    },
    {"filter": {"nope": 1}, "sort": {"a": 1}, "limit": 3},
    {"filter": {"a": 1, "b": "1"}},
]

#: The pairwise operator corpus behind ``MATCH_TYPE_RANK`` (``PROBE_ORDER=1``).
RANK_OPS = {
    "$eq": ("$eq", 1),
    "$lte": ("$lte", 9),
    "$lt": ("$lt", 9),
    "$gt": ("$gt", 0),
    "$gte": ("$gte", 0),
    "$regex": ("$regex", "^x"),
    "$mod": ("$mod", [2, 0]),
    "$exists": ("$exists", True),
    "$in": ("$in", [1, 2]),
    "$ne": ("$ne", 7),
    "$elemMatch": ("$elemMatch", {"z": 1}),
    "$size": ("$size", 2),
    "$type": ("$type", "int"),
    "$bitsAllSet": ("$bitsAllSet", 1),
}


def seed(db):
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "a": i, "b": str(i), "s": i, "sub": {"k": i}} for i in range(20)])
    db.c.create_index([("a", 1)], name="a_1")
    db.c.create_index([("b", 1)], name="b_uniq", unique=True)
    db.c.create_index([("s", 1)], name="s_sparse", sparse=True)
    db.c.create_index([("a", 1)], name="a_part", partialFilterExpression={"a": {"$gt": 5}})


def planner(db, cmd):
    try:
        return db.command({"explain": cmd, "verbosity": "queryPlanner"})["queryPlanner"]
    except pymongo.errors.PyMongoError as exc:
        return {"ERROR": str(exc)[:200]}


def chain(node):
    """The stage chain as (stage, sorted-non-child-keys) pairs."""
    out = []
    while isinstance(node, dict) and "stage" in node:
        out.append(
            (node["stage"], sorted(k for k in node if k not in ("inputStage", "inputStages")))
        )
        node = node.get("inputStage")
    return out


def main():
    mon = pymongo.MongoClient(MONGOD)
    if SERVER:
        sec = pymongo.MongoClient(SERVER)
        srv = None
    else:
        from secantus import SecantusDBServer

        srv = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
        srv.start()
        sec = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}")

    dbn = "probe_explain"
    mon.drop_database(dbn)
    sec.drop_database(dbn)
    mdb, sdb = mon[dbn], sec[dbn]
    seed(mdb)
    seed(sdb)

    if os.environ.get("PROBE_ORDER"):
        for x, y in itertools.combinations(RANK_OPS, 2):
            f = {"a": SON([RANK_OPS[x], RANK_OPS[y]])}
            pq = planner(mdb, {"find": "c", "filter": f}).get("parsedQuery") or {}
            if "$and" in pq:
                print(f"{x:12} + {y:12} -> {[next(iter(c['a'])) for c in pq['$and']]}")
        return 0

    diffs = 0
    for f in FILTERS:
        m = planner(mdb, {"find": "c", "filter": f}).get("parsedQuery")
        s = planner(sdb, {"find": "c", "filter": f}).get("parsedQuery")
        if m != s:
            diffs += 1
            print(f"DIFF parsedQuery {json_util.dumps(f)}")
            print(f"  mongod: {json_util.dumps(m)}")
            print(f"  secant: {json_util.dumps(s)}")
    print(f"--- parsedQuery: {diffs} of {len(FILTERS)} divergent")

    plan_diffs = 0
    for shape in PLANS:
        cmd = {"find": "c", **shape}
        m = chain(planner(mdb, cmd).get("winningPlan"))
        s = chain(planner(sdb, cmd).get("winningPlan"))
        if m != s:
            plan_diffs += 1
            print(f"DIFF winningPlan {json_util.dumps(shape)}")
            print(f"  mongod: {m}")
            print(f"  secant: {s}")
    print(f"--- winningPlan: {plan_diffs} of {len(PLANS)} divergent")

    if srv is not None:
        srv.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
