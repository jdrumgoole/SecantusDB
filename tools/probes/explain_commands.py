"""Differential-probe `explain` against a real mongod.

What `explain` answers for `find`, `count`, `distinct`, `aggregate`, `update`,
`delete` and `findAndModify`: the parsed query, the winning plan's stage tree
with its index bounds, the execution counts, and what the command refuses.

Almost every scenario gives the planner ONE usable index or none, so the plan
is a rule and not the result of mongod's trial runs. The scenarios named
`choice: ...` are the exception: they offer two usable indexes, and are
there to count how often the two servers pick the same one.

What is compared, and what is not:

* compared: `parsedQuery`, the whole `winningPlan` tree (stage names, index
  flags, `indexBounds`, `direction`, `filter`, `transformBy`, ...), the NUMBER
  of rejected plans, and under `executionStats` each stage's `nReturned`,
  `keysExamined`, `docsExamined` and the write counters;
* not compared: hashes and timings, `serverInfo`, `serverParameters`, the
  slot-based plan text, plan node ids, and the per-stage work accounting
  (`works`, `advanced`, `needTime`, `seeks`, ...), which describes mongod's
  executor rather than the query.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/explain_commands.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import datetime
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Int64, MaxKey, MinKey, ObjectId, Regex  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
RUN = f"{os.getpid() % 100000}"

#: Dropped wherever they appear: they vary from run to run, or describe
#: mongod's executor and not the query.
DROP = {
    "$clusterTime",
    "operationTime",
    "opTime",
    "electionId",
    "serverInfo",
    "serverParameters",
    "queryHash",
    "planCacheShapeHash",
    "planCacheKey",
    "queryShapeHash",
    "optimizationTimeMillis",
    "executionTimeMillis",
    "executionTimeMillisEstimate",
    "works",
    "advanced",
    "needTime",
    "needYield",
    "saveState",
    "restoreState",
    "isEOF",
    "seeks",
    "dupsTested",
    "dupsDropped",
    "alreadyHasObj",
    "slotBasedPlan",
    "planNodeId",
    "opens",
    "closes",
    # What the sort held in memory, by mongod's own accounting of a document.
    "totalDataSizeSorted",
}

D = datetime.datetime(2024, 5, 1, 12, 0, 0)

# One usable index per field: a (ascending), b (descending), c+d (compound),
# e+f (compound, mixed directions), t (multikey), s (unique, sparse),
# p (partial on q > 0), n.k (dotted), w (a date). x and y have none.
DOCS = [
    {
        "_id": i,
        "a": i % 5,
        "b": i % 4,
        "c": i % 3,
        "d": i % 7,
        "e": i % 2,
        "f": i,
        "t": [i, i + 1],
        "s": f"s{i:02d}",
        "p": i,
        "q": i - 10,
        "n": {"k": i % 6},
        "w": D + datetime.timedelta(days=i),
        "x": i % 5,
        "y": f"y{i % 3}",
    }
    for i in range(1, 25)
] + [
    {"_id": 100, "a": None, "x": None},
    {"_id": 101, "a": "str", "b": "str"},
    {"_id": 102},
]
INDEXES = [
    {"key": {"a": 1}, "name": "a_1"},
    {"key": {"b": -1}, "name": "b_-1"},
    {"key": {"c": 1, "d": 1}, "name": "c_1_d_1"},
    {"key": {"e": 1, "f": -1}, "name": "e_1_f_-1"},
    {"key": {"t": 1}, "name": "t_1"},
    {"key": {"s": 1}, "name": "s_1", "unique": True, "sparse": True},
    {"key": {"p": 1}, "name": "p_1", "partialFilterExpression": {"q": {"$gt": 0}}},
    {"key": {"n.k": 1}, "name": "n.k_1"},
    {"key": {"w": 1}, "name": "w_1"},
]
SEED = [
    {"insert": "c", "documents": DOCS},
    {"createIndexes": "c", "indexes": INDEXES},
    # Two usable indexes on one field, for the `choice:` scenarios.
    {"insert": "m", "documents": [{"_id": i, "a": i % 5, "b": i % 3, "z": i} for i in range(40)]},
    {
        "createIndexes": "m",
        "indexes": [
            {"key": {"a": 1}, "name": "a_1"},
            {"key": {"a": 1, "b": 1}, "name": "a_1_b_1"},
            {"key": {"b": 1}, "name": "b_1"},
        ],
    },
    {"create": "v", "viewOn": "c", "pipeline": [{"$match": {"a": {"$gt": 1}}}]},
]

S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, SEED + list(steps)))


def ex(command, verbosity="queryPlanner", **more):
    out = {"explain": command}
    if verbosity is not None:
        out["verbosity"] = verbosity
    out.update(more)
    return out


def find(filter_=None, coll="c", **more):
    return {"find": coll, "filter": filter_ or {}, **more}


def finds(name, filters, verbosity="queryPlanner", **more):
    scenario(name, *[ex(find(f, **more), verbosity) for f in filters])


def agg(*pipeline, coll="c", **kw):
    return {"aggregate": coll, "pipeline": list(pipeline), "cursor": {}, **kw}


def count(query=None, coll="c", **more):
    out = {"count": coll}
    if query is not None:
        out["query"] = query
    return {**out, **more}


def distinct(key, query=None, coll="c", **more):
    out = {"distinct": coll, "key": key}
    if query is not None:
        out["query"] = query
    return {**out, **more}


def update(q, u, coll="c", **more):
    return {"update": coll, "updates": [{"q": q, "u": u, **more}]}


def delete(q, limit=0, coll="c", **more):
    return {"delete": coll, "deletes": [{"q": q, "limit": limit, **more}]}


def fam(query, coll="c", **more):
    return {"findAndModify": coll, "query": query, **more}


# ----------------------------------------------------------- find: the scan

finds(
    "find: collection scans",
    [
        {},
        {"x": 1},
        {"x": {"$gt": 1, "$lt": 4}},
        {"x": 1, "y": "y1"},
        {"x": {"$in": [1, 2]}},
        {"$or": [{"x": 1}, {"y": "y2"}]},
        {"x": {"$exists": False}},
        {"$expr": {"$eq": ["$x", 1]}},
        {"$nor": [{"x": 1}]},
        {"$where": "true"},
    ],
)
scenario(
    "find: $natural",
    ex(find(hint={"$natural": 1})),
    ex(find(hint={"$natural": -1})),
    ex(find({"a": 2}, hint={"$natural": 1})),
    ex(find({"a": 2}, hint={"$natural": -1})),
    ex(find(sort={"$natural": 1})),
    ex(find(sort={"$natural": -1})),
    ex(find({"_id": 3}, hint={"$natural": 1})),
)
finds(
    "find: one _id",
    [
        {"_id": 3},
        {"_id": {"$eq": 3}},
        {"_id": 999},
        {"_id": "x"},
        {"_id": None},
        {"_id": {"k": 1}},
        {"_id": ObjectId("0123456789abcdef01234567")},
        {"_id": 3.0},
        {"_id": Int64(3)},
    ],
)
scenario(
    "find: one _id with options",
    ex(find({"_id": 3}, projection={"a": 1})),
    ex(find({"_id": 3}, projection={"_id": 1})),
    ex(find({"_id": 3}, projection={"a": 0})),
    ex(find({"_id": 3}, sort={"a": 1})),
    ex(find({"_id": 3}, sort={"_id": -1})),
    ex(find({"_id": 3}, limit=1)),
    ex(find({"_id": 3}, limit=5)),
    ex(find({"_id": 3}, skip=1)),
    ex(find({"_id": 3}, batchSize=1)),
    ex(find({"_id": 3}, singleBatch=True)),
    ex(find({"_id": 3}, hint="_id_")),
    ex(find({"_id": 3}, hint={"_id": 1})),
    ex(find({"_id": 3}, collation={"locale": "en"})),
    ex(find({"_id": "x"}, collation={"locale": "en", "strength": 2})),
)
finds(
    "find: other _id filters",
    [
        {"_id": {"$gt": 3}},
        {"_id": {"$gte": 3, "$lt": 9}},
        {"_id": {"$in": [1, 2, 3]}},
        {"_id": {"$in": [3]}},
        {"_id": {"$in": []}},
        {"_id": {"$ne": 3}},
        {"_id": 3, "a": 3},
        {"_id": 3, "x": 3},
        {"_id": {"$exists": True}},
        {"$and": [{"_id": 3}]},
        {"$or": [{"_id": 3}]},
        {"$or": [{"_id": 3}, {"_id": 4}]},
    ],
)

# ----------------------------------------------------- find: index bounds

finds(
    "bounds: comparisons on an ascending index",
    [
        {"a": 2},
        {"a": {"$eq": 2}},
        {"a": {"$gt": 2}},
        {"a": {"$gte": 2}},
        {"a": {"$lt": 2}},
        {"a": {"$lte": 2}},
        {"a": {"$gt": 1, "$lt": 4}},
        {"a": {"$gte": 1, "$lte": 4}},
        {"a": {"$gt": 4, "$lt": 1}},
        {"a": {"$gt": 1, "$gte": 3}},
        {"a": {"$gt": 2, "$lt": 2}},
        {"a": {"$gte": 2, "$lte": 2}},
        {"$and": [{"a": {"$gt": 1}}, {"a": {"$lt": 4}}]},
        {"$and": [{"a": 2}, {"a": 3}]},
    ],
)
finds(
    "bounds: comparisons on a descending index",
    [
        {"b": 2},
        {"b": {"$gt": 2}},
        {"b": {"$gte": 1}},
        {"b": {"$lt": 2}},
        {"b": {"$lte": 2}},
        {"b": {"$gt": 0, "$lt": 3}},
        {"b": {"$in": [1, 3]}},
        {"b": {"$in": [3, 1]}},
        {"b": None},
        {"b": "str"},
    ],
)
finds(
    "bounds: $in, $ne, $nin, $exists, $not",
    [
        {"a": {"$in": [1, 3]}},
        {"a": {"$in": [3, 1, 3]}},
        {"a": {"$in": [1]}},
        {"a": {"$in": []}},
        {"a": {"$in": [1, "x", None]}},
        {"a": {"$in": [1, Regex("^s")]}},
        {"a": {"$ne": 2}},
        {"a": {"$nin": [1, 3]}},
        {"a": {"$nin": []}},
        {"a": {"$exists": True}},
        {"a": {"$exists": False}},
        {"a": {"$not": {"$gt": 2}}},
        {"a": {"$not": {"$in": [1, 2]}}},
        {"a": {"$not": {"$eq": 2}}},
    ],
)
finds(
    "bounds: by type",
    [
        {"a": None},
        {"a": {"$eq": None}},
        {"a": {"$ne": None}},
        {"a": {"$gt": None}},
        {"a": {"$gte": None}},
        {"a": "str"},
        {"a": {"$gt": "s"}},
        {"a": {"$lt": "s"}},
        {"a": {"$gte": "a", "$lt": "t"}},
        {"a": True},
        {"a": {"$gt": False}},
        {"a": 2.5},
        {"a": Int64(2)},
        {"a": {"$gt": 1.5, "$lte": Int64(3)}},
        {"a": {"k": 1}},
        {"a": {"$gt": {"k": 1}}},
        {"a": [1, 2]},
        {"a": {"$gt": [1]}},
        {"a": []},
        {"a": MinKey()},
        {"a": {"$gt": MinKey()}},
        {"a": {"$lt": MaxKey()}},
        {"a": {"$gte": MinKey(), "$lte": MaxKey()}},
        {"a": ObjectId("0123456789abcdef01234567")},
        {"a": float("nan")},
        {"a": {"$gt": float("inf")}},
        {"a": {"$lt": float("-inf")}},
    ],
)
finds(
    "bounds: dates",
    [
        {"w": D},
        {"w": {"$gt": D}},
        {"w": {"$gte": D, "$lt": D + datetime.timedelta(days=3)}},
        {"w": {"$lt": D}},
        {"w": {"$in": [D, D + datetime.timedelta(days=1)]}},
    ],
)
finds(
    "bounds: $regex, $type, $mod, $size, $all, $elemMatch",
    [
        {"s": Regex("^s0")},
        {"s": {"$regex": "^s0"}},
        {"s": {"$regex": "^s0", "$options": "i"}},
        {"s": {"$regex": "s0"}},
        {"s": {"$regex": "^s0.*"}},
        {"s": {"$regex": "^s\\.0"}},
        {"s": {"$regex": "^"}},
        {"s": {"$not": {"$regex": "^s0"}}},
        {"a": {"$type": "int"}},
        {"a": {"$type": "string"}},
        {"a": {"$type": "number"}},
        {"a": {"$type": ["int", "string"]}},
        {"a": {"$type": "null"}},
        {"a": {"$type": "array"}},
        {"a": {"$mod": [2, 0]}},
        {"a": {"$size": 2}},
        {"a": {"$all": [1, 2]}},
        {"a": {"$all": [1]}},
        {"a": {"$elemMatch": {"$gt": 1}}},
        {"a": {"$bitsAllSet": 1}},
    ],
)
finds(
    "bounds: a compound index",
    [
        {"c": 1},
        {"c": 1, "d": 2},
        {"d": 2, "c": 1},
        {"c": 1, "d": {"$gt": 2}},
        {"c": 1, "d": {"$gte": 2, "$lt": 5}},
        {"c": {"$gt": 0}},
        {"c": {"$gt": 0}, "d": 2},
        {"c": {"$gt": 0}, "d": {"$lt": 3}},
        {"c": {"$in": [0, 2]}, "d": {"$in": [1, 3]}},
        {"c": {"$in": [0, 2]}, "d": {"$gt": 3}},
        {"d": 2},
        {"c": 1, "d": 2, "x": 3},
        {"c": 1, "x": 3},
        {"c": 1, "d": None},
        {"c": 1, "d": {"$ne": 2}},
        {"c": 1, "d": {"$exists": True}},
        {"c": {"$ne": 1}, "d": 2},
    ],
)
finds(
    "bounds: a compound index of mixed directions",
    [
        {"e": 1},
        {"e": 1, "f": 5},
        {"e": 1, "f": {"$gt": 5}},
        {"e": 1, "f": {"$lte": 5}},
        {"e": 1, "f": {"$gt": 3, "$lt": 9}},
        {"e": {"$gte": 0}, "f": {"$lt": 9}},
        {"e": 1, "f": {"$in": [3, 5, 7]}},
        {"f": 5},
    ],
)
finds(
    "bounds: a multikey index",
    [
        {"t": 3},
        {"t": {"$gte": 3}},
        {"t": {"$gt": 3, "$lt": 9}},
        {"t": {"$lt": 9, "$gt": 3}},
        {"t": {"$elemMatch": {"$gt": 3, "$lt": 9}}},
        {"t": {"$elemMatch": {"$eq": 3}}},
        {"t": {"$in": [3, 5]}},
        {"t": [3, 4]},
        {"t": []},
        {"t": {"$all": [3, 4]}},
        {"t": {"$size": 2}},
        {"t": {"$ne": 3}},
        {"t": None},
        {"t": {"$exists": True}},
        {"t": {"$not": {"$gt": 3}}},
        {"t": {"$elemMatch": {"$in": [3, 5]}}},
    ],
)
finds(
    "bounds: a dotted path",
    [
        {"n.k": 2},
        {"n.k": {"$gt": 2}},
        {"n": {"k": 2}},
        {"n.k": {"$in": [1, 2]}},
        {"n.k": None},
        {"n": {"$elemMatch": {"k": 2}}},
    ],
)
finds(
    "index flags: unique, sparse, partial",
    [
        {"s": "s03"},
        {"s": {"$gt": "s03"}},
        {"s": None},
        {"s": {"$exists": False}},
        {"s": {"$exists": True}},
        {"s": {"$ne": "s03"}},
        {"s": {"$in": ["s03", "s04"]}},
        {"p": 5},
        {"p": 15, "q": 5},
        {"p": 15, "q": {"$gt": 0}},
        {"p": 15, "q": {"$gt": 3}},
        {"p": 15, "q": {"$gte": 0}},
        {"p": {"$gt": 12}, "q": {"$gt": 1}},
        {"q": {"$gt": 0}},
    ],
)

# ------------------------------------------- find: sort, projection, paging

scenario(
    "sort: served by an index or not",
    ex(find(sort={"a": 1})),
    ex(find(sort={"a": -1})),
    ex(find(sort={"b": 1})),
    ex(find(sort={"b": -1})),
    ex(find(sort={"x": 1})),
    ex(find(sort={"_id": 1})),
    ex(find(sort={"_id": -1})),
    ex(find({"a": {"$gt": 1}}, sort={"a": 1})),
    ex(find({"a": {"$gt": 1}}, sort={"a": -1})),
    ex(find({"a": 2}, sort={"a": 1})),
    ex(find({"a": 2}, sort={"x": 1})),
    ex(find({"x": 2}, sort={"a": 1})),
    ex(find({"x": 2}, sort={"y": 1})),
    ex(find(sort={"t": 1})),
    ex(find(sort={"s": 1})),
    ex(find(sort={"p": 1})),
    ex(find(sort={"n.k": 1})),
    ex(find(sort={"a": 1, "_id": 1})),
    ex(find(sort={"x": 1, "y": -1})),
)
scenario(
    "sort: over a compound index",
    ex(find(sort={"c": 1})),
    ex(find(sort={"c": 1, "d": 1})),
    ex(find(sort={"c": -1, "d": -1})),
    ex(find(sort={"c": 1, "d": -1})),
    ex(find(sort={"d": 1})),
    ex(find({"c": 1}, sort={"d": 1})),
    ex(find({"c": 1}, sort={"d": -1})),
    ex(find({"c": {"$in": [0, 1]}}, sort={"d": 1})),
    ex(find({"c": {"$gt": 0}}, sort={"d": 1})),
    ex(find({"c": 1, "d": {"$gt": 2}}, sort={"d": 1})),
    ex(find(sort={"e": 1, "f": -1})),
    ex(find(sort={"e": -1, "f": 1})),
    ex(find(sort={"e": 1, "f": 1})),
    ex(find({"e": 1}, sort={"f": 1})),
)
scenario(
    "sort: with limit and skip",
    ex(find(sort={"x": 1}, limit=3)),
    ex(find(sort={"x": 1}, skip=2)),
    ex(find(sort={"x": 1}, limit=3, skip=2)),
    ex(find(sort={"a": 1}, limit=3)),
    ex(find(sort={"a": 1}, skip=2)),
    ex(find(sort={"a": 1}, limit=3, skip=2)),
    ex(find({"x": 1}, sort={"y": 1}, limit=3, skip=2)),
    ex(find({"a": 1}, sort={"x": 1}, limit=3, skip=2)),
    ex(find(sort={"x": 1}, limit=-3)),
    ex(find(sort={"x": 1}, limit=3, singleBatch=True)),
    ex(find(sort={"x": 1}, batchSize=2)),
    ex(find(sort={"x": 1}, limit=0)),
    ex(find(sort={"x": 1}, allowDiskUse=True)),
    ex(find(sort={"x": 1}, allowDiskUse=False)),
)
scenario(
    "paging: limit and skip without a sort",
    ex(find(limit=3)),
    ex(find(skip=2)),
    ex(find(limit=3, skip=2)),
    ex(find({"a": 2}, limit=3)),
    ex(find({"a": 2}, skip=2)),
    ex(find({"a": 2}, limit=3, skip=2)),
    ex(find({"a": 2, "x": 2}, limit=3, skip=2)),
    ex(find({"x": 2}, limit=3, skip=2)),
    ex(find(limit=-3)),
    ex(find(limit=0, skip=0)),
    ex(find(batchSize=0)),
    ex(find(limit=Int64(3))),
    ex(find(limit=3.0)),
)
scenario(
    "projection: covered, simple and default",
    ex(find({"a": 2}, projection={"a": 1, "_id": 0})),
    ex(find({"a": 2}, projection={"a": 1})),
    ex(find({"a": 2}, projection={"_id": 0, "a": 1})),
    ex(find({"a": 2}, projection={"x": 1})),
    ex(find({"a": 2}, projection={"a": 0})),
    ex(find({"a": 2}, projection={"_id": 0})),
    ex(find({"a": 2}, projection={})),
    ex(find({"x": 2}, projection={"a": 1, "_id": 0})),
    ex(find({"x": 2}, projection={"n.k": 1})),
    ex(find({"x": 2}, projection={"z": {"$add": ["$a", 1]}})),
    ex(find({"x": 2}, projection={"t": {"$slice": 1}})),
    ex(find({"t": 3}, projection={"t.$": 1})),
    ex(find({"x": 2}, projection={"t": {"$elemMatch": {"$gt": 3}}})),
    ex(find({"c": 1}, projection={"c": 1, "d": 1, "_id": 0})),
    ex(find({"c": 1}, projection={"d": 1, "_id": 0})),
    ex(find({"t": 3}, projection={"t": 1, "_id": 0})),
    ex(find({"n.k": 2}, projection={"n.k": 1, "_id": 0})),
    ex(find({"a": 2}, projection={"a": True, "_id": False})),
    ex(find({"a": 2}, projection={"a": 1.0, "_id": 0})),
    ex(find(projection={"a": 1, "_id": 0})),
    ex(find(projection={"a": 1, "_id": 0}, sort={"a": 1})),
    ex(find({"a": 2}, projection={"a": 1, "_id": 0}, sort={"x": 1})),
    ex(find({"a": 2}, projection={"a": 1, "_id": 0}, limit=2, skip=1)),
    ex(find({"x": 2}, projection={"a": 1}, sort={"y": 1}, limit=2, skip=1)),
)

# ----------------------------------------------------------- find: $or, hint

finds(
    "$or and $and",
    [
        {"$or": [{"a": 1}, {"b": 2}]},
        {"$or": [{"a": 1}, {"a": 3}]},
        {"$or": [{"a": 1}, {"x": 2}]},
        {"$or": [{"a": 1}]},
        {"$or": [{"a": {"$gt": 3}}, {"b": {"$lt": 1}}, {"s": "s03"}]},
        {"$or": [{"a": 1}, {"b": 2}], "x": 1},
        {"$or": [{"a": 1}, {"b": 2}], "c": 1},
        {"$or": [{"a": 1, "x": 1}, {"b": 2}]},
        {"$or": [{"c": 1, "d": 2}, {"c": 2, "d": 3}]},
        {"$or": [{"$and": [{"a": 1}, {"x": 2}]}, {"b": 2}]},
        {"$and": [{"$or": [{"a": 1}, {"b": 2}]}, {"$or": [{"s": "s01"}, {"t": 3}]}]},
        {"$or": [{"t": 3}, {"t": 5}]},
        {"$or": [{"_id": 1}, {"a": 2}]},
        {"$nor": [{"a": 1}, {"b": 2}]},
        {"$and": [{"a": 1}, {"x": 2}]},
        {"$and": [{"a": {"$gt": 1}}, {"x": 2}, {"a": {"$lt": 4}}]},
    ],
)
scenario(
    "$or with sort, limit and projection",
    ex(find({"$or": [{"a": 1}, {"b": 2}]}, sort={"x": 1})),
    ex(find({"$or": [{"a": 1}, {"b": 2}]}, sort={"a": 1})),
    ex(find({"$or": [{"a": 1}, {"a": 3}]}, sort={"a": 1})),
    ex(find({"$or": [{"a": 1}, {"b": 2}]}, limit=3)),
    ex(find({"$or": [{"a": 1}, {"b": 2}]}, skip=3)),
    ex(find({"$or": [{"a": 1}, {"b": 2}]}, projection={"a": 1, "_id": 0})),
    ex(find({"$or": [{"a": 1}, {"b": 2}]}, sort={"x": 1}, limit=2, skip=1)),
)
scenario(
    "hint",
    ex(find(hint="a_1")),
    ex(find(hint={"a": 1})),
    ex(find({"a": 2}, hint="a_1")),
    ex(find({"x": 2}, hint="a_1")),
    ex(find({"a": 2}, hint="b_-1")),
    ex(find({"a": 2}, hint={"b": -1})),
    ex(find({"a": 2}, hint="_id_")),
    ex(find({"a": 2}, hint={"_id": 1})),
    ex(find(hint="_id_")),
    ex(find(hint="c_1_d_1")),
    ex(find({"d": 2}, hint="c_1_d_1")),
    ex(find(hint="t_1")),
    ex(find(hint="s_1")),
    ex(find(hint="p_1")),
    ex(find({"q": {"$gt": 0}}, hint="p_1")),
    ex(find({"s": None}, hint="s_1")),
    ex(find(hint="nope")),
    ex(find(hint={"nope": 1})),
    ex(find(hint={"a": -1})),
    ex(find(hint=1)),
    ex(find(hint={})),
    ex(find(hint="")),
    ex(find(hint="a_1", sort={"a": -1})),
    ex(find(hint="a_1", sort={"x": 1})),
    ex(find({"a": 2}, hint="a_1", projection={"a": 1, "_id": 0})),
)
scenario(
    "min, max and returnKey",
    ex(find(hint="a_1", min={"a": 2})),
    ex(find(hint="a_1", max={"a": 3})),
    ex(find(hint="a_1", min={"a": 2}, max={"a": 4})),
    ex(find(min={"a": 2})),
    ex(find(hint="a_1", min={"b": 2})),
    ex(find({"a": 2}, returnKey=True)),
    ex(find({"a": 2}, showRecordId=True)),
    ex(find({"x": 2}, showRecordId=True)),
)
scenario(
    "collation",
    ex(find({"a": 2}, collation={"locale": "en"})),
    ex(find({"a": "str"}, collation={"locale": "en"})),
    ex(find({"a": {"$gt": 1}}, collation={"locale": "en", "strength": 2})),
    ex(find({"a": {"$gt": "a"}}, collation={"locale": "en", "strength": 2})),
    ex(find(sort={"a": 1}, collation={"locale": "en"})),
    ex(find({"a": 2}, collation={"locale": "simple"})),
    ex(find({"x": "y"}, collation={"locale": "fr", "backwards": True})),
    ex(find({"a": 2}, collation={"locale": "zz"})),
    ex(find({"a": 2}, collation={})),
    ex(find({"a": {"$in": [1, "x"]}}, collation={"locale": "en"})),
)

# ------------------------------------------------------------------- count

scenario(
    "count",
    ex(count()),
    ex(count({})),
    ex(count({"a": 2})),
    ex(count({"a": {"$gt": 1, "$lt": 4}})),
    ex(count({"a": {"$in": [1, 3]}})),
    ex(count({"x": 2})),
    ex(count({"_id": 3})),
    ex(count({"_id": {"$gt": 3}})),
    ex(count({"c": 1, "d": 2})),
    ex(count({"c": 1})),
    ex(count({"t": 3})),
    ex(count({"t": {"$gt": 3}})),
    ex(count({"a": None})),
    ex(count({"a": 2, "x": 2})),
    ex(count({"s": "s03"})),
    ex(count({"b": {"$gte": 1}})),
    ex(count({"$or": [{"a": 1}, {"b": 2}]})),
)
scenario(
    "count: skip, limit and hint",
    ex(count(skip=2)),
    ex(count(limit=3)),
    ex(count(skip=2, limit=3)),
    ex(count({"a": 2}, skip=2)),
    ex(count({"a": 2}, limit=3)),
    ex(count({"a": 2}, skip=1, limit=2)),
    ex(count({"x": 2}, skip=1, limit=2)),
    ex(count({"a": 2}, limit=-2)),
    ex(count({"a": 2}, limit=0)),
    ex(count(hint="a_1")),
    ex(count({"x": 2}, hint="a_1")),
    ex(count({"a": 2}, hint={"$natural": 1})),
    ex(count({"a": 2}, hint="nope")),
    ex(count({"a": 2}, collation={"locale": "en"})),
    ex(count({"a": 2}, coll="nope")),
    ex(count(coll="nope")),
)

# ---------------------------------------------------------------- distinct

scenario(
    "distinct",
    ex(distinct("a")),
    ex(distinct("b")),
    ex(distinct("x")),
    ex(distinct("_id")),
    ex(distinct("c")),
    ex(distinct("d")),
    ex(distinct("t")),
    ex(distinct("s")),
    ex(distinct("p")),
    ex(distinct("n.k")),
    ex(distinct("a", {})),
    ex(distinct("a", {"a": {"$gt": 1}})),
    ex(distinct("a", {"x": 2})),
    ex(distinct("x", {"a": 2})),
    ex(distinct("d", {"c": 1})),
    ex(distinct("c", {"d": 1})),
    ex(distinct("a", {"_id": 3})),
    ex(distinct("x", {"x": 2})),
    ex(distinct("a", coll="nope")),
    ex(distinct("a", hint="a_1")),
    ex(distinct("a", hint={"$natural": 1})),
    ex(distinct("a", collation={"locale": "en"})),
    ex(distinct("")),
    ex(distinct(1)),
)

# --------------------------------------------------------------- aggregate

scenario(
    "aggregate: a pipeline the planner absorbs",
    ex(agg()),
    ex(agg({"$match": {"a": 2}})),
    ex(agg({"$match": {"x": 2}})),
    ex(agg({"$match": {"_id": 3}})),
    ex(agg({"$match": {"a": 2}}, {"$sort": {"x": 1}})),
    ex(agg({"$match": {"a": {"$gt": 1}}}, {"$sort": {"a": 1}})),
    ex(agg({"$sort": {"a": 1}})),
    ex(agg({"$sort": {"x": 1}})),
    ex(agg({"$sort": {"x": 1}}, {"$limit": 3})),
    ex(agg({"$limit": 3})),
    ex(agg({"$skip": 3})),
    ex(agg({"$skip": 3}, {"$limit": 2})),
    ex(agg({"$match": {"a": 2}}, {"$project": {"a": 1, "_id": 0}})),
    ex(agg({"$match": {"x": 2}}, {"$project": {"a": 1}})),
    ex(agg({"$project": {"a": 1}})),
    ex(agg({"$match": {"a": 2}}, {"$match": {"x": 1}})),
    ex(agg({"$match": {"a": 2}}, {"$limit": 2}, {"$skip": 1})),
)
scenario(
    "aggregate: stages after the cursor",
    ex(agg({"$unwind": "$t"})),
    ex(agg({"$match": {"a": 2}}, {"$unwind": "$t"})),
    ex(agg({"$unwind": "$t"}, {"$match": {"a": 2}})),
    ex(agg({"$unwind": "$t"}, {"$sort": {"t": 1}})),
    ex(agg({"$addFields": {"z": 1}})),
    ex(agg({"$match": {"a": 2}}, {"$addFields": {"z": {"$add": ["$a", 1]}}})),
    ex(agg({"$count": "n"})),
    ex(agg({"$match": {"a": 2}}, {"$count": "n"})),
    ex(agg({"$sortByCount": "$a"})),
    ex(agg({"$sample": {"size": 3}})),
    ex(agg({"$facet": {"x": [{"$match": {"a": 1}}]}})),
    ex(agg({"$bucket": {"groupBy": "$a", "boundaries": [0, 2, 9]}})),
    ex(agg({"$replaceRoot": {"newRoot": "$n"}})),
    ex(agg({"$unset": "a"})),
    ex(agg({"$redact": "$$KEEP"})),
    ex(agg({"$unwind": "$t"}, {"$group": {"_id": "$t", "n": {"$sum": 1}}})),
    ex(agg({"$unwind": "$t"}, {"$limit": 3})),
    ex(agg({"$unwind": "$t"}, {"$skip": 3}, {"$limit": 3})),
    ex(agg({"$unwind": "$t"}, {"$project": {"t": 1}})),
)
scenario(
    "aggregate: $group and $lookup",
    ex(agg({"$group": {"_id": "$a"}})),
    ex(agg({"$group": {"_id": "$x", "n": {"$sum": 1}}})),
    ex(agg({"$match": {"a": 2}}, {"$group": {"_id": "$x", "n": {"$sum": "$b"}}})),
    ex(agg({"$group": {"_id": None, "n": {"$sum": 1}}})),
    ex(agg({"$sort": {"a": 1}}, {"$group": {"_id": "$a", "f": {"$first": "$x"}}})),
    ex(agg({"$lookup": {"from": "m", "localField": "a", "foreignField": "a", "as": "j"}})),
    ex(agg({"$lookup": {"from": "m", "localField": "a", "foreignField": "z", "as": "j"}})),
    ex(agg({"$lookup": {"from": "nope", "localField": "a", "foreignField": "z", "as": "j"}})),
    ex(agg({"$group": {"_id": "$a"}}, {"$sort": {"_id": 1}})),
    ex(agg({"$group": {"_id": "$a"}}, {"$limit": 2})),
)
scenario(
    "aggregate: options and special sources",
    ex(agg({"$match": {"a": 2}}, hint="a_1")),
    ex(agg({"$match": {"x": 2}}, hint="a_1")),
    ex(agg({"$match": {"a": 2}}, hint={"$natural": 1})),
    ex(agg({"$match": {"a": 2}}, hint="nope")),
    ex(agg({"$match": {"a": 2}}, collation={"locale": "en"})),
    ex(agg({"$match": {"a": 2}}, allowDiskUse=True)),
    ex(agg({"$match": {"a": 2}}, coll="nope")),
    ex(agg({"$unwind": "$t"}, coll="nope")),
    ex(agg({"$match": {"a": 2}}, coll="v")),
    ex(agg(coll="v")),
    ex({"aggregate": 1, "pipeline": [{"$documents": [{"a": 1}]}], "cursor": {}}),
    ex(agg({"$collStats": {"count": {}}})),
    ex(agg({"$indexStats": {}})),
    ex(agg({"$out": "o"})),
    ex(agg({"$merge": {"into": "o"}})),
    ex(agg({"$match": {"a": 2}}, {"$out": "o"})),
    {"listCollections": 1, "filter": {"name": "o"}, "nameOnly": True},
    ex(agg({"$nope": 1})),
    ex(agg({"$match": {"a": {"$nope": 1}}})),
    ex({"aggregate": "c", "pipeline": [], "explain": True}),
    {"aggregate": "c", "pipeline": [{"$match": {"a": 2}}], "explain": True},
    {"aggregate": "c", "pipeline": [{"$match": {"a": 2}}], "explain": False, "cursor": {}},
    {"aggregate": "c", "pipeline": [{"$unwind": "$t"}], "explain": True},
    {"aggregate": "c", "pipeline": [], "explain": True, "cursor": {}},
    {"aggregate": "c", "pipeline": [], "explain": 1},
)

# ------------------------------------------------------------------ writes

scenario(
    "update",
    ex(update({"a": 2}, {"$set": {"z": 1}})),
    ex(update({"a": 2}, {"$set": {"z": 1}}, multi=True)),
    ex(update({"x": 2}, {"$set": {"z": 1}}, multi=True)),
    ex(update({"_id": 3}, {"$set": {"z": 1}})),
    ex(update({"_id": 3}, {"$set": {"z": 1}}, multi=True)),
    ex(update({"_id": 999}, {"$set": {"z": 1}}, upsert=True)),
    ex(update({"a": 99}, {"$set": {"z": 1}}, upsert=True)),
    ex(update({}, {"$set": {"z": 1}}, multi=True)),
    ex(update({"a": 2}, {"z": 1})),
    ex(update({"a": 2}, [{"$set": {"z": 1}}])),
    ex(update({"a": 2}, {"$set": {"z": 1}}, hint="a_1")),
    ex(update({"a": 2}, {"$set": {"z": 1}}, hint="nope")),
    ex(update({"a": 2}, {"$set": {"z": 1}}, collation={"locale": "en"})),
    ex(update({"a": 2}, {"$set": {"z": 1}}, coll="nope")),
    ex(update({"a": 2}, {"$nope": {"z": 1}})),
    ex(update({"a": 2}, {"$set": {"_id": 1}})),
    ex(update({"c": 1, "d": {"$gt": 2}}, {"$inc": {"z": 1}}, multi=True)),
    ex(update({"$or": [{"a": 1}, {"b": 2}]}, {"$inc": {"z": 1}}, multi=True)),
    ex(
        {
            "update": "c",
            "updates": [
                {"q": {"a": 1}, "u": {"$set": {"z": 1}}},
                {"q": {"a": 2}, "u": {"$set": {"z": 1}}},
            ],
        }
    ),
    ex({"update": "c", "updates": []}),
    ex({"update": "c"}),
    ex(update({"a": 2}, {"$set": {"z": 1}}, coll="v")),
    {"find": "c", "filter": {"z": {"$exists": True}}},
)
scenario(
    "delete",
    ex(delete({"a": 2})),
    ex(delete({"a": 2}, limit=1)),
    ex(delete({"x": 2})),
    ex(delete({"x": 2}, limit=1)),
    ex(delete({"_id": 3})),
    ex(delete({"_id": 3}, limit=1)),
    ex(delete({})),
    ex(delete({}, limit=1)),
    ex(delete({"a": 2}, hint="a_1")),
    ex(delete({"a": 2}, hint="nope")),
    ex(delete({"a": 2}, collation={"locale": "en"})),
    ex(delete({"a": 2}, coll="nope")),
    ex(delete({"a": 2}, limit=2)),
    ex(delete({"c": 1, "d": {"$gt": 2}})),
    ex(
        {
            "delete": "c",
            "deletes": [{"q": {"a": 1}, "limit": 0}, {"q": {"a": 2}, "limit": 0}],
        }
    ),
    ex({"delete": "c", "deletes": []}),
    ex(delete({"a": 2}, coll="v")),
    {"count": "c"},
)
scenario(
    "findAndModify",
    ex(fam({"a": 2}, update={"$set": {"z": 1}})),
    ex(fam({"a": 2}, remove=True)),
    ex(fam({"x": 2}, update={"$set": {"z": 1}})),
    ex(fam({"x": 2}, remove=True)),
    ex(fam({"_id": 3}, update={"$set": {"z": 1}})),
    ex(fam({"_id": 3}, remove=True)),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}, sort={"x": 1})),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}, sort={"a": -1})),
    ex(fam({"a": 2}, remove=True, sort={"x": 1})),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}, new=True)),
    ex(fam({"a": 99}, update={"$set": {"z": 1}}, upsert=True)),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}, fields={"a": 1})),
    ex(fam({"a": 2}, remove=True, fields={"a": 1, "_id": 0})),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}, hint="a_1")),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}, hint="nope")),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}, coll="nope")),
    ex(fam({"a": 2})),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}, remove=True)),
    ex(fam({}, update={"z": 1})),
    ex(fam({"a": 2}, update=[{"$set": {"z": 1}}])),
    {"find": "c", "filter": {"z": {"$exists": True}}},
    {"count": "c"},
)

# --------------------------------------------------------- execution counts

scenario(
    "executionStats: find",
    ex(find(), "executionStats"),
    ex(find({"x": 2}), "executionStats"),
    ex(find({"a": 2}), "executionStats"),
    ex(find({"a": {"$gt": 1, "$lt": 4}}), "executionStats"),
    ex(find({"a": {"$in": [1, 3]}}), "executionStats"),
    ex(find({"a": 2, "x": 3}), "executionStats"),
    ex(find({"a": 99}), "executionStats"),
    ex(find({"_id": 3}), "executionStats"),
    ex(find({"_id": 999}), "executionStats"),
    ex(find({"_id": {"$gt": 20}}), "executionStats"),
    ex(find({"c": 1, "d": 2}), "executionStats"),
    ex(find({"c": 1, "d": {"$gt": 2}}), "executionStats"),
    ex(find({"t": 3}), "executionStats"),
    ex(find({"t": {"$gte": 3, "$lt": 6}}), "executionStats"),
    ex(find({"t": {"$in": [3, 4]}}), "executionStats"),
    ex(find({"s": "s03"}), "executionStats"),
    ex(find({"p": 15, "q": 5}), "executionStats"),
    ex(find({"$or": [{"a": 1}, {"b": 2}]}), "executionStats"),
    ex(find({"a": 2}, coll="nope"), "executionStats"),
)
scenario(
    "executionStats: sort, limit, skip and projection",
    ex(find(sort={"x": 1}), "executionStats"),
    ex(find(sort={"x": 1}, limit=3), "executionStats"),
    ex(find(sort={"a": 1}, limit=3), "executionStats"),
    ex(find(sort={"a": -1}, limit=3, skip=2), "executionStats"),
    ex(find(limit=3), "executionStats"),
    ex(find(skip=20), "executionStats"),
    ex(find(limit=3, skip=2), "executionStats"),
    ex(find({"a": 2}, limit=2), "executionStats"),
    ex(find({"a": 2}, skip=2), "executionStats"),
    ex(find({"x": 2}, limit=2), "executionStats"),
    ex(find({"a": 2}, projection={"a": 1, "_id": 0}), "executionStats"),
    ex(find({"a": 2}, projection={"x": 1}), "executionStats"),
    ex(find({"a": 2}, sort={"x": 1}, limit=2), "executionStats"),
    ex(find({"a": 2}, batchSize=2), "executionStats"),
    ex(find({"a": 2}, limit=-2), "executionStats"),
    ex(find(hint="a_1"), "executionStats"),
    ex(find({"x": 2}, hint="a_1"), "executionStats"),
)
scenario(
    "executionStats: count and distinct",
    ex(count(), "executionStats"),
    ex(count({"a": 2}), "executionStats"),
    ex(count({"x": 2}), "executionStats"),
    ex(count({"a": {"$gt": 1}}), "executionStats"),
    ex(count({"a": 2}, skip=1, limit=2), "executionStats"),
    ex(count({"_id": 3}), "executionStats"),
    ex(count({"a": 2, "x": 2}), "executionStats"),
    ex(count(coll="nope"), "executionStats"),
    ex(distinct("a"), "executionStats"),
    ex(distinct("x"), "executionStats"),
    ex(distinct("a", {"a": {"$gt": 1}}), "executionStats"),
    ex(distinct("x", {"a": 2}), "executionStats"),
    ex(distinct("t"), "executionStats"),
)
scenario(
    "executionStats: aggregate",
    ex(agg(), "executionStats"),
    ex(agg({"$match": {"a": 2}}), "executionStats"),
    ex(agg({"$match": {"x": 2}}, {"$limit": 2}), "executionStats"),
    ex(agg({"$unwind": "$t"}), "executionStats"),
    ex(agg({"$match": {"a": 2}}, {"$unwind": "$t"}, {"$limit": 3}), "executionStats"),
    ex(agg({"$group": {"_id": "$x", "n": {"$sum": 1}}}), "executionStats"),
    ex(agg({"$match": {"a": 2}}, {"$count": "n"}), "executionStats"),
    ex(agg({"$sort": {"x": 1}}, {"$limit": 3}), "executionStats"),
)
scenario(
    "executionStats: writes change nothing",
    ex(update({"a": 2}, {"$set": {"z": 1}}, multi=True), "executionStats"),
    ex(update({"a": 2}, {"$set": {"z": 1}}), "executionStats"),
    ex(update({"x": 2}, {"$set": {"x": 2}}, multi=True), "executionStats"),
    ex(update({"_id": 3}, {"$set": {"z": 1}}), "executionStats"),
    ex(update({"a": 99}, {"$set": {"z": 1}}, upsert=True), "executionStats"),
    ex(update({"a": 99}, {"$set": {"z": 1}}), "executionStats"),
    ex(delete({"a": 2}), "executionStats"),
    ex(delete({"a": 2}, limit=1), "executionStats"),
    ex(delete({"x": 2}), "executionStats"),
    ex(delete({"_id": 3}, limit=1), "executionStats"),
    ex(delete({"a": 99}), "executionStats"),
    ex(fam({"a": 2}, update={"$set": {"z": 1}}), "executionStats"),
    ex(fam({"a": 2}, remove=True), "executionStats"),
    ex(fam({"a": 2}, remove=True, sort={"x": 1}), "executionStats"),
    ex(fam({"a": 99}, update={"$set": {"z": 1}}, upsert=True), "executionStats"),
    ex(fam({"_id": 3}, update={"$set": {"z": 1}}), "executionStats"),
    {"find": "c", "filter": {"z": {"$exists": True}}},
    {"count": "c"},
)
scenario(
    "allPlansExecution",
    ex(find({"a": 2}), "allPlansExecution"),
    ex(find({"x": 2}), "allPlansExecution"),
    ex(find({"_id": 3}), "allPlansExecution"),
    ex(count({"a": 2}), "allPlansExecution"),
    ex(distinct("a"), "allPlansExecution"),
    ex(agg({"$match": {"a": 2}}), "allPlansExecution"),
    ex(agg({"$unwind": "$t"}), "allPlansExecution"),
    ex(update({"a": 2}, {"$set": {"z": 1}}), "allPlansExecution"),
    ex(delete({"a": 2}), "allPlansExecution"),
    ex(fam({"a": 2}, remove=True), "allPlansExecution"),
)

# ------------------------------------------- namespaces: missing, view, etc.

scenario(
    "a collection that does not exist",
    ex(find(coll="nope")),
    ex(find({"a": 2}, coll="nope")),
    ex(find({"_id": 3}, coll="nope")),
    ex(find({"a": 2}, coll="nope", sort={"a": 1}, limit=2, projection={"a": 1})),
    ex(find({"a": 2}, coll="nope", hint="a_1")),
    ex(find({"a": 2}, coll="nope", hint={"$natural": 1})),
    ex(count({"a": 2}, coll="nope")),
    ex(distinct("a", {"a": 2}, coll="nope")),
    ex(agg({"$match": {"a": 2}}, coll="nope")),
    ex(agg({"$group": {"_id": "$a"}}, coll="nope")),
    ex(update({"a": 2}, {"$set": {"z": 1}}, coll="nope", upsert=True)),
    ex(delete({"a": 2}, coll="nope")),
    ex(fam({"a": 2}, coll="nope", remove=True)),
    ("@missing", ex(find({"a": 2}))),
    ("@missing", ex(count())),
    {"listCollections": 1, "filter": {"name": "nope"}, "nameOnly": True},
)
scenario(
    "a view",
    ex(find(coll="v")),
    ex(find({"b": 2}, coll="v")),
    ex(find({"b": 2}, coll="v", sort={"x": 1}, limit=2)),
    ex(find({"_id": 3}, coll="v")),
    ex(count({"b": 2}, coll="v")),
    ex(count(coll="v")),
    ex(distinct("a", coll="v")),
    ex(distinct("a", {"b": 2}, coll="v")),
    ex(agg({"$match": {"b": 2}}, coll="v")),
    ex(agg({"$group": {"_id": "$a"}}, coll="v")),
    ex(find(coll="v"), "executionStats"),
    ex(fam({"a": 2}, coll="v", remove=True)),
)
scenario(
    "an empty collection and an empty result",
    {"create": "e"},
    ex(find(coll="e")),
    ex(find({"a": 2}, coll="e")),
    ex(find({"_id": 2}, coll="e")),
    ex(find(coll="e"), "executionStats"),
    ex(count(coll="e")),
    ex(count(coll="e"), "executionStats"),
    ex(distinct("a", coll="e")),
    ex(agg({"$match": {"a": 2}}, coll="e")),
    ex(find({"a": {"$in": []}})),
    ex(find({"a": {"$in": []}}), "executionStats"),
    ex(find({"$and": [{"a": 1}, {"a": {"$in": []}}]})),
    ex(find({"$alwaysFalse": 1})),
    ex(find({"$alwaysTrue": 1})),
    ex(find({"$nor": []})),
    ex(find({"$expr": False})),
)

# ----------------------------------------------------- what explain refuses

scenario(
    "arguments: verbosity",
    *[
        {"explain": find({"a": 2}), "verbosity": v}
        for v in [
            "queryPlanner",
            "executionStats",
            "allPlansExecution",
            "nope",
            "QUERYPLANNER",
            "",
            1,
            None,
            True,
            {},
            [],
            2.5,
        ]
    ],
    {"explain": find({"a": 2})},
)
scenario(
    "arguments: the explained command",
    *[
        {"explain": v}
        for v in [
            1,
            "find",
            None,
            [],
            True,
            {},
            {"nope": "c"},
            {"find": 1},
            {"find": ""},
            {"find": None},
            {"find": {}},
            {"find": "c", "nope": 1},
            {"find": "c", "filter": 1},
            {"find": "c", "filter": {"a": {"$nope": 1}}},
            {"find": "c", "sort": 1},
            {"find": "c", "limit": "x"},
            {"find": "c", "projection": {"a": 1, "b": 0}},
            {"count": 1},
            {"count": "c", "query": 1},
            {"count": "c", "skip": -1},
            {"distinct": "c"},
            {"distinct": "c", "key": "a", "query": 1},
            {"aggregate": "c"},
            {"aggregate": "c", "pipeline": 1},
            {"aggregate": "c", "pipeline": []},
            {"aggregate": "c", "pipeline": [], "cursor": 1},
            {"aggregate": "c", "pipeline": [], "cursor": {"batchSize": 0}},
            {"update": "c", "updates": 1},
            {"delete": "c", "deletes": [{"q": {}}]},
            {"findAndModify": "c"},
            {"findandmodify": "c", "query": {"a": 2}, "remove": True},
            {"findAndModify": "c", "query": 1, "remove": True},
        ]
    ],
)
scenario(
    "arguments: commands that cannot be explained",
    *[
        {"explain": v}
        for v in [
            {"insert": "c", "documents": [{"_id": 500}]},
            {"explain": {"find": "c"}},
            {"ping": 1},
            {"hello": 1},
            {"getMore": Int64(1), "collection": "c"},
            {"listIndexes": "c"},
            {"listCollections": 1},
            {"createIndexes": "c", "indexes": [{"key": {"zz": 1}, "name": "zz_1"}]},
            {"drop": "c"},
            {"create": "new"},
            {"collStats": "c"},
            {"dbStats": 1},
            {"killCursors": "c", "cursors": [Int64(1)]},
            {"bulkWrite": 1, "ops": [], "nsInfo": []},
            {"mapReduce": "c", "map": "function(){}", "reduce": "function(){}", "out": "o"},
            {"validate": "c"},
            {"renameCollection": "a.b", "to": "a.c"},
            {"commitTransaction": 1},
        ]
    ],
    {"count": "c"},
    {"listIndexes": "c", "cursor": {"batchSize": 0}},
)
scenario(
    "arguments: fields beside the command",
    {"explain": find({"a": 2}), "nope": 1},
    {"explain": find({"a": 2}), "comment": "x"},
    {"explain": find({"a": 2}, comment="x")},
    {"explain": find({"a": 2}), "maxTimeMS": 1000},
    {"explain": find({"a": 2}, maxTimeMS=1000)},
    {"explain": find({"a": 2}), "maxTimeMS": "x"},
    {"explain": find({"a": 2}), "readConcern": {"level": "local"}},
    {"explain": find({"a": 2}), "readConcern": {"level": "majority"}},
    {"explain": find({"a": 2}), "readConcern": {"level": "snapshot"}},
    {"explain": find({"a": 2}, readConcern={"level": "local"})},
    {"explain": find({"a": 2}), "writeConcern": {"w": 1}},
    {"explain": find({"a": 2}), "writeConcern": {"w": "majority"}},
    {"explain": update({"a": 2}, {"$set": {"z": 1}}), "writeConcern": {"w": 1}},
    {"explain": update({"a": 2}, {"$set": {"z": 1}}), "writeConcern": {"w": "majority"}},
    {"explain": update({"a": 2}, {"$set": {"z": 1}}) | {"writeConcern": {"w": 1}}},
    {"explain": update({"a": 2}, {"$set": {"z": 1}}) | {"writeConcern": {"w": "majority"}}},
    {"explain": delete({"a": 2}) | {"writeConcern": {"j": True}}},
    {"explain": find({"a": 2}) | {"$db": "other"}},
    {"explain": find({"a": 2}) | {"$db": "@db"}},
    {"explain": find({"a": 2}) | {"lsid": {"id": 1}}},
    {"explain": find({"a": 2}), "apiVersion": "1"},
    {"explain": find({"a": 2}), "apiVersion": "1", "apiStrict": True},
    {"explain": agg({"$match": {"a": 2}}), "apiVersion": "1", "apiStrict": True},
    ("admin", {"explain": {"find": "c", "filter": {}}}),
    ("local", {"explain": {"find": "oplog.rs", "filter": {}}, "verbosity": "queryPlanner"}),
    ("config", {"explain": {"find": "nope", "filter": {}}}),
    {"explain": {"find": "system.views", "filter": {}}},
    {"count": "c"},
)

# -------------------------------------------- two usable indexes: the choice

scenario(
    "choice: two indexes on one field",
    ex(find({"a": 2}, coll="m")),
    ex(find({"a": {"$gt": 1}}, coll="m")),
    ex(find({"a": 2, "b": 1}, coll="m")),
    ex(find({"a": 2, "b": {"$gt": 0}}, coll="m")),
    ex(find({"a": 2}, coll="m", sort={"b": 1})),
    ex(find({"b": 1}, coll="m", sort={"a": 1})),
    ex(find(coll="m", sort={"a": 1})),
    ex(find({"a": 2}, coll="m", projection={"a": 1, "b": 1, "_id": 0})),
    ex(find({"a": 2, "z": 3}, coll="m")),
    ex(find({"$or": [{"a": 1}, {"b": 2}]}, coll="m")),
)
scenario(
    "choice: count, distinct and writes",
    ex(count({"a": 2}, coll="m")),
    ex(count({"a": 2, "b": 1}, coll="m")),
    ex(distinct("a", coll="m")),
    ex(distinct("b", {"a": 2}, coll="m")),
    ex(agg({"$match": {"a": 2}}, coll="m")),
    ex(agg({"$match": {"a": 2}}, {"$group": {"_id": "$b"}}, coll="m")),
    ex(update({"a": 2}, {"$set": {"q": 1}}, coll="m", multi=True)),
    ex(delete({"a": 2, "b": 1}, coll="m")),
    ex(find({"a": 2}, coll="m"), "executionStats"),
    ex(find({"a": 2, "b": 1}, coll="m"), "executionStats"),
)


# ------------------------------------------------------------------ running

UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def resolve(value, db):
    if isinstance(value, dict):
        return {k: resolve(v, db) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve(v, db) for v in value]
    if value == "@db":
        return db
    return value


def clean(value, db, key=None):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k in DROP:
                continue
            if k == "rejectedPlans" and isinstance(v, list):
                out[k] = f"{len(v)} plans"
                continue
            if k == "allPlansExecution" and isinstance(v, list):
                out[k] = f"{len(v)} plans"
                continue
            out[k] = clean(v, db, k)
        # A slot-based plan nests the classic tree under `queryPlan`.
        if set(out) == {"queryPlan"} and isinstance(out["queryPlan"], dict):
            return out["queryPlan"]
        return out
    if isinstance(value, list):
        return [clean(v, db, key) for v in value]
    if isinstance(value, Int64):
        # An int32 and an int64 print alike; keep them apart.
        return f"Int64({int(value)})"
    if isinstance(value, str):
        text = value.replace(db, "@db")
        text = UUID_TEXT.sub("UUID", text)
        # mongod prints its parse of the query in front of a planner error.
        text = re.sub(r"Tree: .*?\n planner returned", " planner returned", text, flags=re.DOTALL)
        # The order of the types a slot accepts varies from build to build.
        return re.sub(
            r"expected types '\[([^\]]*)\]'",
            lambda m: "expected types '[" + ", ".join(sorted(m.group(1).split(", "))) + "]'",
            text,
        )
    if isinstance(value, float) and value != value:
        return "NaN"
    return value


def run_scenario(uri, index, steps):
    db = f"exp{RUN}_{index}"
    wire = Wire(uri)
    replies = []
    for step in steps:
        where, command = step if isinstance(step, tuple) else (None, step)
        where = db if where is None else (db + "_missing" if where == "@missing" else where)
        try:
            reply = wire.command(where, resolve(command, db))
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
        replies.append(clean(reply, db))
    try:
        wire.command(db, {"dropDatabase": 1})
    except (TimeoutError, ConnectionError, OSError):
        print(f"  NOTE: cleanup of {db} did not finish on {uri}", file=sys.stderr)
    finally:
        wire.close()
    return replies


def show(step):
    where, command = step if isinstance(step, tuple) else (None, step)
    return (("" if where is None else f"[{where}] ") + repr(command))[:220]


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "1200"))

    # Self-check: the reference must plan the seeded collection the way the
    # probe assumes, and count what it ran.
    check = run_scenario(mongod_uri, 0, [*SEED, ex(find({"a": 2}), "executionStats")])[-1]
    plan = check.get("queryPlanner", {}).get("winningPlan", {})
    scan = plan.get("inputStage", {})
    if not (
        scan.get("indexName") == "a_1"
        and scan.get("indexBounds") == {"a": ["[2, 2]"]}
        and check.get("executionStats", {}).get("nReturned") == 5
        and check["queryPlanner"].get("rejectedPlans") == "0 plans"
    ):
        print("self-check failed on mongod:", check, file=sys.stderr)
        return 2

    divergent = replies = replies_bad = total = 0
    n_seed = len(SEED)
    for index, (name, steps) in enumerate(S, start=1):
        if only and only not in name:
            continue
        total += 1
        want = run_scenario(mongod_uri, index, steps)
        got = run_scenario(server_uri, index, steps)
        replies += len(steps) - n_seed
        bad = [i for i, (w, g) in enumerate(zip(want, got, strict=True)) if repr(w) != repr(g)]
        if not bad:
            continue
        divergent += 1
        replies_bad += len(bad)
        print(f"\n--- {name} ({len(bad)} of {len(steps) - n_seed})")
        for i in bad:
            print(f"  step {i - n_seed}: {show(steps[i])}")
            print(f"    mongod: {str(want[i])[:width]}")
            print(f"    server: {str(got[i])[:width]}")
    print(
        f"\n=== explain: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
